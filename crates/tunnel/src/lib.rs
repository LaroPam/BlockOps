pub mod access;
// Capability-gated, single-target TCP tunnel over authenticated libp2p streams.
use anyhow::{bail, ensure, Context, Result};
use futures::StreamExt;
use libp2p::{
    dcutr, identify, noise, relay,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, StreamProtocol, Swarm, SwarmBuilder,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch},
    task::JoinSet,
};
use tokio_util::{compat::FuturesAsyncReadCompatExt, sync::CancellationToken};

const PROTOCOL: StreamProtocol = StreamProtocol::new("/blockops/tcp/1");
const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_STREAMS: usize = 32;

#[derive(NetworkBehaviour)]
struct Behaviour {
    relay: relay::client::Behaviour,
    identify: identify::Behaviour,
    dcutr: dcutr::Behaviour,
    streams: libp2p_stream::Behaviour,
}

fn swarm(key: libp2p::identity::Keypair) -> Result<Swarm<Behaviour>> {
    Ok(SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_relay_client(noise::Config::new, yamux::Config::default)?
        .with_behaviour(|key, relay| Behaviour {
            relay,
            identify: identify::Behaviour::new(identify::Config::new(
                "/blockops/1".into(),
                key.public(),
            )),
            dcutr: dcutr::Behaviour::new(key.public().to_peer_id()),
            streams: libp2p_stream::Behaviour::new(),
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
        .build())
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Bearer secret. Intentionally does not implement Debug.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invite {
    pub version: u8,
    pub address: String,
    pub token: String,
    pub expires_at: u64,
}
impl Invite {
    fn validate(&self) -> Result<(Multiaddr, PeerId, [u8; 32])> {
        ensure!(self.version == 1, "Unsupported invitation version");
        ensure!(self.expires_at > now(), "Invitation expired");
        let address: Multiaddr = self.address.parse().context("Invalid peer address")?;
        let peer = match address.iter().last() {
            Some(libp2p::multiaddr::Protocol::P2p(peer)) => peer,
            _ => bail!("Invitation must identify the host peer"),
        };
        let token: [u8; 32] = hex::decode(&self.token)
            .context("Invalid capability")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid capability length"))?;
        Ok((address, peer, token))
    }
}

#[derive(Clone, Serialize, Default)]
pub struct Status {
    pub active_connections: usize,
    pub last_error: Option<String>,
    pub transport: Option<String>,
}

pub struct Session {
    pub invite: Option<Invite>,
    pub endpoint: Option<SocketAddr>,
    pub status: watch::Receiver<Status>,
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
    access: Option<access::Access>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl Session {
    pub fn issue_invite(&self) -> Result<(Invite, access::Grant)> {
        let access = self
            .access
            .as_ref()
            .context("Host does not support identity-bound invitations")?;
        ensure!(!self.cancel.is_cancelled(), "Host is stopped");
        let mut invite = self.invite.clone().context("Not a host")?;
        ensure!(invite.expires_at > now(), "Host session expired");
        let mut token = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut token);
        invite.token = hex::encode(token);
        let grant = access.issue(token, invite.expires_at)?;
        Ok((invite, grant))
    }
    pub async fn stop(mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

/// Listens only on loopback directly. A relay address explicitly opts into public reachability.
pub async fn host(
    target: SocketAddr,
    relay_address: Option<Multiaddr>,
    ttl: Duration,
) -> Result<Session> {
    host_inner(target, relay_address, ttl, None).await
}

/// Only identity-bound grants issued through Session::issue_invite are accepted.
pub async fn host_controlled(
    target: SocketAddr,
    relay_address: Option<Multiaddr>,
    ttl: Duration,
) -> Result<Session> {
    host_inner(target, relay_address, ttl, Some(access::Access::default())).await
}
async fn host_inner(
    target: SocketAddr,
    relay_address: Option<Multiaddr>,
    ttl: Duration,
    access: Option<access::Access>,
) -> Result<Session> {
    ensure!(
        target.ip().is_loopback() && target.port() != 0,
        "Target must be a concrete loopback port"
    );
    ensure!(
        ttl.as_secs() > 0 && ttl <= Duration::from_secs(86400),
        "Invitation lifetime must be 1..86400 seconds"
    );
    let mut swarm = swarm(libp2p::identity::Keypair::generate_ed25519())?;
    let peer = *swarm.local_peer_id();
    let mut incoming = swarm.behaviour().streams.new_control().accept(PROTOCOL)?;
    let listen = match relay_address {
        Some(mut a) => {
            a.push(libp2p::multiaddr::Protocol::P2pCircuit);
            a
        }
        None => "/ip4/127.0.0.1/tcp/0".parse()?,
    };
    swarm.listen_on(listen)?;
    let mut secret = [0; 32];
    rand::rngs::OsRng.fill_bytes(&mut secret);
    let expires = now() + ttl.as_secs();
    let (ready_tx, ready_rx) = oneshot::channel();
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let (status_tx, status) = watch::channel(Status::default());
    let host_access = access.clone();
    let task = tokio::spawn(async move {
        let mut ready = Some(ready_tx);
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                event = swarm.select_next_some() => match event {
                    SwarmEvent::NewListenAddr { mut address, .. } => {
                        if address.iter().last() != Some(libp2p::multiaddr::Protocol::P2p(peer)) {
                            address.push(libp2p::multiaddr::Protocol::P2p(peer));
                        }
                        if let Some(tx) = ready.take() { let _ = tx.send(Ok(address)); }
                    }
                    SwarmEvent::ListenerError { error, .. } => {
                        if let Some(tx) = ready.take() { let _ = tx.send(Err(anyhow::anyhow!(error.to_string()))); }
                        status_tx.send_modify(|s| s.last_error = Some(error.to_string()));
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received { info, .. })) => {
                        swarm.add_external_address(info.observed_addr);
                    }
                    _ => {}
                },
                Some((remote_peer, stream)) = incoming.next() => {
                    if tasks.len() >= MAX_STREAMS { drop(stream); continue; }
                    let access = host_access.clone();
                    tasks.spawn(async move {
                        let mut stream = stream.compat();
                        let mut received = [0;32];
                        tokio::time::timeout(TIMEOUT, stream.read_exact(&mut received)).await??;
                        let member = access.as_ref().and_then(|a| a.authorize(&received, remote_peer));
                        let allowed = if access.is_some() { member.is_some() } else { bool::from(secret.ct_eq(&received)) };
                        if now() >= expires || !allowed {
                            stream.write_all(&[0]).await?;
                            bail!("Invitation rejected");
                        }
                        let revoked = member.unwrap_or_default();
                        let mut target = tokio::select! {
                            biased;
                            _ = revoked.cancelled() => bail!("Member access revoked"),
                            result = tokio::time::timeout(TIMEOUT, TcpStream::connect(target)) => result??,
                        };
                        stream.write_all(&[1]).await?;
                        tokio::select! {
                            _ = revoked.cancelled() => bail!("Member access revoked"),
                            result = tokio::io::copy_bidirectional(&mut stream, &mut target) => { result?; }
                        }
                        Ok::<_, anyhow::Error>(())
                    });
                    status_tx.send_modify(|s| s.active_connections = tasks.len());
                },
                Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                    status_tx.send_modify(|s| {
                        s.active_connections = tasks.len();
                        if let Err(e) = result.unwrap_or_else(|e| Err(e.into())) { s.last_error = Some(e.to_string()); }
                    });
                }
            }
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    });
    let mut session = Session {
        invite: None,
        endpoint: None,
        status,
        cancel,
        task: Some(task),
        access,
    };
    let address = tokio::time::timeout(TIMEOUT, ready_rx)
        .await
        .context("Host listen/reservation timed out")???;
    session.invite = Some(Invite {
        version: 1,
        address: address.to_string(),
        token: hex::encode(secret),
        expires_at: expires,
    });
    Ok(session)
}

pub async fn join(invite: Invite, bind: SocketAddr) -> Result<Session> {
    join_with_identity(invite, bind, libp2p::identity::Keypair::generate_ed25519()).await
}
pub async fn join_with_identity(
    invite: Invite,
    bind: SocketAddr,
    key: libp2p::identity::Keypair,
) -> Result<Session> {
    ensure!(bind.ip().is_loopback(), "Guest listener must bind loopback");
    let listener = TcpListener::bind(bind).await?;
    join_with_listener(invite, listener, key).await
}

/// Starts a guest using a previously reserved local listener. This permits callers to
/// validate and reserve the endpoint before consuming a one-use invitation.
pub async fn join_with_listener(
    invite: Invite,
    listener: TcpListener,
    key: libp2p::identity::Keypair,
) -> Result<Session> {
    let endpoint = listener.local_addr()?;
    ensure!(
        endpoint.ip().is_loopback(),
        "Guest listener must bind loopback"
    );
    let (address, peer, token) = invite.validate()?;
    let mut swarm = swarm(key)?;
    swarm.add_peer_address(peer, address.clone());
    swarm.dial(address)?;
    let control = swarm.behaviour().streams.new_control();
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let (status_tx, status) = watch::channel(Status::default());
    let task = tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                event = swarm.select_next_some() => match event {
                    SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } if peer_id == peer => {
                        let relayed = endpoint.get_remote_address().iter().any(|p| p == libp2p::multiaddr::Protocol::P2pCircuit);
                        status_tx.send_modify(|s| s.transport = Some(if relayed { "relay" } else { "direct" }.into()));
                    }
                    SwarmEvent::ConnectionClosed { peer_id, num_established: 0, .. } if peer_id == peer => status_tx.send_modify(|s| s.transport = None),
                    SwarmEvent::OutgoingConnectionError { error, .. } => status_tx.send_modify(|s| s.last_error = Some(error.to_string())),
                    SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received { info, .. })) => { swarm.add_external_address(info.observed_addr); }
                    _ => {}
                },
                accepted = listener.accept(), if tasks.len() < MAX_STREAMS => {
                    let Ok((mut socket, _)) = accepted else { break; };
                    let mut control = control.clone();
                    tasks.spawn(async move {
                        ensure!(now() < invite.expires_at, "Invitation expired");
                        let mut stream = tokio::time::timeout(TIMEOUT, control.open_stream(peer, PROTOCOL)).await??.compat();
                        tokio::time::timeout(TIMEOUT, async {
                            stream.write_all(&token).await?;
                            ensure!(stream.read_u8().await? == 1, "Invitation rejected");
                            Ok::<_, anyhow::Error>(())
                        }).await??;
                        tokio::io::copy_bidirectional(&mut socket, &mut stream).await?;
                        Ok::<_, anyhow::Error>(())
                    });
                    status_tx.send_modify(|s| s.active_connections = tasks.len());
                },
                Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                    status_tx.send_modify(|s| {
                        s.active_connections = tasks.len();
                        if let Err(e) = result.unwrap_or_else(|e| Err(e.into())) { s.last_error = Some(e.to_string()); }
                    });
                }
            }
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    });
    // Keep control alive in the actor; listener readiness is not a remote health claim.
    Ok(Session {
        invite: None,
        endpoint: Some(endpoint),
        status,
        cancel,
        task: Some(task),
        access: None,
    })
}
