pub mod access;
pub mod direct;
pub mod friends;
pub mod minecraft;
pub mod pake;
// Capability-gated, single-target TCP tunnel over authenticated libp2p streams.
use anyhow::{bail, ensure, Context, Result};
use futures::StreamExt;
use libp2p::{
    dcutr, identify, kad, noise, ping, relay,
    swarm::{behaviour::toggle::Toggle, NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, StreamProtocol, Swarm, SwarmBuilder,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot, watch},
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
    /// Measures the round trip to every connected peer, shown as the friend's ping.
    ping: ping::Behaviour,
    /// Only the serverless mode joins the public DHT.
    kad: Toggle<kad::Behaviour<kad::store::MemoryStore>>,
}

/// `public` joins the IPFS DHT as a client, to meet a peer without a coordinator.
fn swarm(key: libp2p::identity::Keypair, public: bool) -> Result<Swarm<Behaviour>> {
    Ok(SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_dns()?
        .with_relay_client(noise::Config::new, yamux::Config::default)?
        .with_behaviour(|key, relay| {
            let peer = key.public().to_peer_id();
            let kad = public.then(|| {
                let mut kad = kad::Behaviour::with_config(
                    peer,
                    kad::store::MemoryStore::new(peer),
                    kad::Config::new(kad::PROTOCOL_NAME),
                );
                // Never serve the DHT: a player is not a public node.
                kad.set_mode(Some(kad::Mode::Client));
                kad
            });
            Behaviour {
                relay,
                identify: identify::Behaviour::new(identify::Config::new(
                    "/blockops/1".into(),
                    key.public(),
                )),
                dcutr: dcutr::Behaviour::new(peer),
                streams: libp2p_stream::Behaviour::new(),
                ping: ping::Behaviour::new(
                    ping::Config::new().with_interval(Duration::from_secs(10)),
                ),
                kad: kad.into(),
            }
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
        .build())
}

/// Who may use a tunnel stream: holders of one shared secret, or identity-bound grants.
#[derive(Clone)]
enum Gate {
    Secret([u8; 32]),
    Members(access::Access),
}

/// Who is in the world: one seat per login, announced to the session until dropped.
enum RosterEvent {
    Joined {
        id: u64,
        peer: PeerId,
        name: Option<String>,
    },
    Left(u64),
}

/// Held while a player's connection lasts; dropping it, also by aborting the task, frees it.
struct Seat {
    id: u64,
    events: mpsc::UnboundedSender<RosterEvent>,
}
impl Seat {
    fn take(
        events: &mpsc::UnboundedSender<RosterEvent>,
        peer: PeerId,
        name: Option<String>,
    ) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let _ = events.send(RosterEvent::Joined { id, peer, name });
        Self {
            id,
            events: events.clone(),
        }
    }
}
impl Drop for Seat {
    fn drop(&mut self) {
        let _ = self.events.send(RosterEvent::Left(self.id));
    }
}

/// The host's view of its players and of the round trip to each friend's computer.
#[derive(Default)]
struct Roster {
    players: BTreeMap<u64, (PeerId, Player)>,
    rtts: HashMap<PeerId, Duration>,
}
impl Roster {
    fn apply(&mut self, event: RosterEvent) {
        match event {
            RosterEvent::Joined { id, peer, name } => {
                let player = Player {
                    id,
                    peer: peer.to_string(),
                    name,
                    since: now(),
                    rtt_ms: None,
                };
                self.players.insert(id, (peer, player));
            }
            RosterEvent::Left(id) => {
                self.players.remove(&id);
            }
        }
    }
    /// Whether the measurement changes what the status shows.
    fn measured(&mut self, peer: PeerId, rtt: Duration) -> bool {
        self.rtts.insert(peer, rtt);
        self.players.values().any(|(p, _)| *p == peer)
    }
    fn disconnected(&mut self, peer: PeerId) {
        self.rtts.remove(&peer);
    }
    fn publish(&self, status: &mut Status) {
        status.players = self
            .players
            .values()
            .map(|(peer, player)| Player {
                rtt_ms: self.rtts.get(peer).map(millis),
                ..player.clone()
            })
            .collect();
    }
}

fn millis(duration: &Duration) -> u32 {
    duration.as_millis().try_into().unwrap_or(u32::MAX)
}

/// Serve one inbound tunnel stream: check its capability, then join it to the target. A
/// login takes a seat for as long as the connection lasts; a server list ping does not.
async fn serve_stream(
    stream: libp2p::Stream,
    remote_peer: PeerId,
    gate: Gate,
    target: SocketAddr,
    expires: u64,
    roster: mpsc::UnboundedSender<RosterEvent>,
) -> Result<()> {
    let mut stream = stream.compat();
    let mut received = [0; 32];
    tokio::time::timeout(TIMEOUT, stream.read_exact(&mut received)).await??;
    let (allowed, member) = match &gate {
        Gate::Secret(secret) => (bool::from(secret.ct_eq(&received)), None),
        Gate::Members(access) => {
            let member = access.authorize(&received, remote_peer);
            (member.is_some(), member)
        }
    };
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
    let mut opening = Vec::new();
    let _seat = tokio::select! {
        _ = revoked.cancelled() => bail!("Member access revoked"),
        kind = minecraft::sniff(&mut stream, &mut opening) => match kind {
            minecraft::Opening::Status => None,
            minecraft::Opening::Login(name) => Some(Seat::take(&roster, remote_peer, name)),
            minecraft::Opening::Unknown => Some(Seat::take(&roster, remote_peer, None)),
        },
    };
    tokio::select! {
        _ = revoked.cancelled() => bail!("Member access revoked"),
        result = async {
            target.write_all(&opening).await?;
            tokio::io::copy_bidirectional(&mut stream, &mut target).await
        } => { result?; }
    }
    Ok(())
}

/// Carry one local game connection to the host over a new authenticated stream.
async fn forward(
    mut socket: TcpStream,
    mut control: libp2p_stream::Control,
    peer: PeerId,
    token: [u8; 32],
    expires: u64,
) -> Result<()> {
    ensure!(now() < expires, "Invitation expired");
    let mut stream = tokio::time::timeout(TIMEOUT, control.open_stream(peer, PROTOCOL))
        .await??
        .compat();
    tokio::time::timeout(TIMEOUT, async {
        stream.write_all(&token).await?;
        ensure!(stream.read_u8().await? == 1, "Invitation rejected");
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    tokio::io::copy_bidirectional(&mut socket, &mut stream).await?;
    Ok(())
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

/// A player in the host's world.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Player {
    /// Unique among the players of this application run.
    pub id: u64,
    /// The friend's computer, which a host may remember to let in without a code.
    pub peer: String,
    /// The name the game logged in with, when it is a valid Minecraft name.
    pub name: Option<String>,
    /// When the player joined, in seconds since the Unix epoch.
    pub since: u64,
    /// Round trip to the player's computer, once measured.
    pub rtt_ms: Option<u32>,
}

#[derive(Clone, Serialize, Default)]
pub struct Status {
    pub active_connections: usize,
    pub last_error: Option<String>,
    pub transport: Option<String>,
    /// Host: who is in the world through the tunnel. Server list pings are not players.
    pub players: Vec<Player>,
    /// Guest: round trip to the host's computer, once measured.
    pub rtt_ms: Option<u32>,
}

pub struct Session {
    pub invite: Option<Invite>,
    pub endpoint: Option<SocketAddr>,
    pub status: watch::Receiver<Status>,
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
    access: Option<access::Access>,
    /// Issues codes on a serverless host.
    direct: Option<tokio::sync::mpsc::Sender<direct::Command>>,
    /// Guest: how to come back to this world without a code, when the host shared it.
    pub world: Option<direct::FriendWorld>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl Session {
    /// Request immediate cancellation without waiting for task cleanup.
    pub fn cancellation_handle(&self) -> CancellationToken {
        self.cancel.clone()
    }

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
    let mut swarm = swarm(libp2p::identity::Keypair::generate_ed25519(), false)?;
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
    let gate = match &access {
        Some(access) => Gate::Members(access.clone()),
        None => Gate::Secret(secret),
    };
    let task = tokio::spawn(async move {
        let mut ready = Some(ready_tx);
        let mut tasks = JoinSet::new();
        let (seats, mut roster_events) = mpsc::unbounded_channel();
        let mut roster = Roster::default();
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                Some(event) = roster_events.recv() => {
                    roster.apply(event);
                    status_tx.send_modify(|s| roster.publish(s));
                }
                event = swarm.select_next_some() => match event {
                    SwarmEvent::Behaviour(BehaviourEvent::Ping(ping::Event { peer, result: Ok(rtt), .. })) => {
                        if roster.measured(peer, rtt) { status_tx.send_modify(|s| roster.publish(s)); }
                    }
                    SwarmEvent::ConnectionClosed { peer_id, num_established: 0, .. } => roster.disconnected(peer_id),
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
                    tasks.spawn(serve_stream(stream, remote_peer, gate.clone(), target, expires, seats.clone()));
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
        direct: None,
        world: None,
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
    let expires = invite.expires_at;
    let mut swarm = swarm(key, false)?;
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
                    SwarmEvent::ConnectionClosed { peer_id, num_established: 0, .. } if peer_id == peer => status_tx.send_modify(|s| { s.transport = None; s.rtt_ms = None; }),
                    SwarmEvent::Behaviour(BehaviourEvent::Ping(ping::Event { peer: measured, result: Ok(rtt), .. })) if measured == peer => {
                        status_tx.send_modify(|s| s.rtt_ms = Some(millis(&rtt)));
                    }
                    SwarmEvent::OutgoingConnectionError { error, .. } => status_tx.send_modify(|s| s.last_error = Some(error.to_string())),
                    SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received { info, .. })) => { swarm.add_external_address(info.observed_addr); }
                    _ => {}
                },
                accepted = listener.accept(), if tasks.len() < MAX_STREAMS => {
                    let Ok((socket, _)) = accepted else { break; };
                    tasks.spawn(forward(socket, control.clone(), peer, token, expires));
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
        direct: None,
        world: None,
    })
}
