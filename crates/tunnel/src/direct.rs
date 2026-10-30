//! Serverless mode: players meet through the public libp2p (IPFS) network instead of a
//! coordinator, prove a one-use code to each other with SPAKE2, and play over a direct,
//! hole-punched connection.
//!
//! The host announces itself in the DHT under a key derived from the code and keeps a
//! reservation on a public relay. The guest finds that announcement and connects through
//! the relay; DCUtR then punches a direct path. Public relays limit each circuit to a couple
//! of minutes and kilobytes, enough to meet and authenticate, so game streams only ever use
//! the direct connection; a pair whose NATs cannot be punched needs a relay of their own.
use crate::{
    access, forward,
    friends::{clean_label, meeting_key},
    millis, now, pake, serve_stream, swarm, Behaviour, BehaviourEvent, Gate, Roster, Session,
    Status, MAX_STREAMS, PROTOCOL, TIMEOUT,
};
use anyhow::{bail, ensure, Context, Result};
use futures::StreamExt;
use libp2p::{
    dcutr, identify, kad,
    multiaddr::Protocol,
    ping,
    swarm::{ConnectionId, SwarmEvent},
    Multiaddr, PeerId, StreamProtocol, Swarm,
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot, watch},
    task::JoinSet,
};
use tokio_util::{compat::FuturesAsyncReadCompatExt, sync::CancellationToken};

pub const PAKE_PROTOCOL: StreamProtocol = StreamProtocol::new("/blockops/pake/1");
/// A remembered friend asks to be let in without a code.
pub const FRIEND_PROTOCOL: StreamProtocol = StreamProtocol::new("/blockops/friend/1");
/// How often a host renews its announcement at the meeting point for its friends.
const MEETING_REFRESH: Duration = Duration::from_secs(600);
const WELCOME_VERSION: u8 = 1;
/// Reservations kept on public relays, so one relay going away does not hide the host.
const MAX_RELAYS: usize = 2;
/// Wrong guesses a code survives; the next one burns it.
const MAX_FAILED_ATTEMPTS: u32 = 3;
const MAX_HANDSHAKES: usize = 8;
const READY_TIMEOUT: Duration = Duration::from_secs(45);
/// A search that finds nothing is repeated after this pause: the host may still be
/// announcing its code, which takes up to a minute on the public DHT.
const SEARCH_RETRY: Duration = Duration::from_secs(4);

/// The IPFS bootstrap nodes; any reachable one is enough to join the DHT.
const PUBLIC_BOOTSTRAP: &[&str] = &[
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb",
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
    "/ip4/104.131.131.82/tcp/4001/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ",
    "/ip4/104.131.131.82/udp/4001/quic-v1/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ",
];

/// How to reach the network where players meet.
#[derive(Clone)]
pub struct Network {
    /// DHT entry points, each ending in `/p2p/<peer id>`.
    pub bootstrap: Vec<Multiaddr>,
    /// Relays to reserve, each ending in `/p2p/<peer id>`. When empty, relays are discovered
    /// among DHT peers and only those with public addresses are used.
    pub relays: Vec<Multiaddr>,
    /// Local sockets; hole punching needs them on all interfaces.
    pub listen: Vec<Multiaddr>,
    /// How long a guest looks for the host before giving up.
    pub find_timeout: Duration,
}

impl Network {
    /// The public IPFS network, listening on every interface for TCP and QUIC.
    pub fn public() -> Self {
        Self {
            bootstrap: PUBLIC_BOOTSTRAP
                .iter()
                .filter_map(|address| address.parse().ok())
                .collect(),
            relays: Vec::new(),
            listen: ["/ip4/0.0.0.0/tcp/0", "/ip4/0.0.0.0/udp/0/quic-v1"]
                .iter()
                .map(|address| address.parse().expect("static address"))
                .collect(),
            find_timeout: Duration::from_secs(90),
        }
    }
}

/// Same shape as a coordinator code's status, so the interface treats both alike.
#[derive(Clone, Serialize)]
pub struct CodeStatus {
    pub id: String,
    pub code: String,
    pub expires_at: u64,
    /// `publishing`, `waiting`, `active`, `revoked` or `failed`.
    pub phase: String,
    pub peer: Option<String>,
    pub error: Option<String>,
}

/// What a guest keeps to enter a host's world again without a code: the host, the
/// capability it granted, and the secret meeting point where the host announces itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendWorld {
    pub host: PeerId,
    pub token: [u8; 32],
    pub meeting: [u8; 32],
    pub label: String,
}

/// How a guest finds and enters a host.
pub enum Ticket {
    /// A one-use code from the host.
    Code(String),
    /// A world this computer was let into before; the host must still remember it.
    Friend(FriendWorld),
}

/// A host's lasting identity, what it calls its world, and who comes in without a code.
pub struct HostIdentity {
    pub key: libp2p::identity::Keypair,
    pub meeting: [u8; 32],
    pub label: String,
    pub friends: Vec<(PeerId, [u8; 32])>,
}

impl HostIdentity {
    /// A one-off identity with no friends, for a host that keeps nothing.
    pub fn ephemeral(label: &str) -> Self {
        let mut meeting = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut meeting);
        Self {
            key: libp2p::identity::Keypair::generate_ed25519(),
            meeting,
            label: label.into(),
            friends: Vec::new(),
        }
    }
}

/// At most 255 bytes of a cleaned label, cut between characters.
fn label_bytes(label: &str) -> Vec<u8> {
    let mut label = clean_label(label);
    while label.len() > 255 {
        label.pop();
    }
    label.into_bytes()
}

/// After a code is proved, the host tells the guest where to find it again.
async fn write_welcome(
    stream: &mut (impl AsyncWrite + Unpin),
    meeting: &[u8; 32],
    label: &str,
) -> Result<()> {
    let label = label_bytes(label);
    stream.write_all(&[WELCOME_VERSION]).await?;
    stream.write_all(meeting).await?;
    stream.write_all(&[label.len() as u8]).await?;
    stream.write_all(&label).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_label(stream: &mut (impl AsyncRead + Unpin)) -> Result<String> {
    let mut label = vec![0; usize::from(stream.read_u8().await?)];
    stream.read_exact(&mut label).await?;
    Ok(clean_label(&String::from_utf8_lossy(&label)))
}

async fn read_welcome(stream: &mut (impl AsyncRead + Unpin)) -> Result<([u8; 32], String)> {
    ensure!(
        stream.read_u8().await? == WELCOME_VERSION,
        "Unknown welcome"
    );
    let mut meeting = [0u8; 32];
    stream.read_exact(&mut meeting).await?;
    Ok((meeting, read_label(stream).await?))
}

/// Answer a remembered friend: in, with the world's current name, or not.
async fn greet(
    stream: libp2p::Stream,
    guest: PeerId,
    access: access::Access,
    label: Arc<([u8; 32], String)>,
) -> Result<()> {
    let mut stream = stream.compat();
    let mut token = [0u8; 32];
    tokio::time::timeout(TIMEOUT, stream.read_exact(&mut token)).await??;
    if access.authorize(&token, guest).is_none() {
        stream.write_all(&[0]).await?;
        return Ok(());
    }
    let label = label_bytes(&label.1);
    stream.write_all(&[1, label.len() as u8]).await?;
    stream.write_all(&label).await?;
    stream.flush().await?;
    Ok(())
}

const REFUSED: &str = "The friend no longer lets this computer in without a code";

pub(crate) enum Command {
    Issue {
        ttl: Duration,
        reply: oneshot::Sender<Result<DirectCode>>,
    },
    Revoke {
        key: pake::RendezvousKey,
    },
}

/// A one-use code for one friend. Dropping it does not revoke it; `revoke` does.
pub struct DirectCode {
    pub status: watch::Receiver<CodeStatus>,
    key: pake::RendezvousKey,
    commands: mpsc::Sender<Command>,
    codes: Codes,
}
impl DirectCode {
    /// Once the code was used: the friend's peer and the capability it was granted, which
    /// the host may keep to let the friend in again without a code.
    pub fn friend(&self) -> Option<(PeerId, [u8; 32])> {
        self.codes.lock().unwrap().get(&self.key)?.friend
    }
    /// Withdraw the code, or end the access it already granted.
    pub async fn revoke(&self) {
        let _ = self.commands.send(Command::Revoke { key: self.key }).await;
    }
}

impl Session {
    /// Whether this host meets its friends through the public network.
    pub fn is_direct(&self) -> bool {
        self.direct.is_some()
    }

    /// End all access of `peer` now, also its open connections.
    pub fn forget(&self, peer: PeerId) {
        if let Some(access) = &self.access {
            access.revoke_peer(peer);
        }
    }

    /// Announce a new one-use code for a serverless host.
    pub async fn issue_direct_code(&self, ttl: Duration) -> Result<DirectCode> {
        ensure!(
            ttl.as_secs() > 0 && ttl <= Duration::from_secs(900),
            "Code lifetime must be 1..900 seconds"
        );
        let commands = self
            .direct
            .as_ref()
            .context("This host does not use the serverless mode")?;
        let (reply, response) = oneshot::channel();
        commands
            .send(Command::Issue { ttl, reply })
            .await
            .map_err(|_| anyhow::anyhow!("Host is stopped"))?;
        response
            .await
            .map_err(|_| anyhow::anyhow!("Host is stopped"))?
    }
}

struct CodeState {
    code: String,
    expires: u64,
    failures: u32,
    busy: bool,
    done: bool,
    grant: Option<access::Grant>,
    /// The guest who proved the code and the capability it received.
    friend: Option<(PeerId, [u8; 32])>,
    status: watch::Sender<CodeStatus>,
}

type Codes = Arc<Mutex<HashMap<pake::RendezvousKey, CodeState>>>;

fn finish(state: &mut CodeState, phase: &str, error: Option<&str>) {
    state.done = true;
    state.status.send_modify(|status| {
        status.phase = phase.into();
        status.error = error.map(Into::into);
    });
}

/// Addresses a relay can be reached at by both players: no private ranges, no circuits.
fn is_public(address: &Multiaddr) -> bool {
    if address.iter().any(|p| p == Protocol::P2pCircuit) {
        return false;
    }
    match address.iter().next() {
        Some(Protocol::Ip4(ip)) => {
            let [a, b, ..] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_documentation()
                // Carrier-grade NAT, 100.64.0.0/10.
                || (a == 100 && (64..128).contains(&b)))
        }
        Some(Protocol::Ip6(ip)) => {
            let first = ip.segments()[0];
            !(ip.is_loopback()
                || ip.is_unspecified()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80)
        }
        Some(Protocol::Dns(_) | Protocol::Dns4(_) | Protocol::Dns6(_)) => true,
        _ => false,
    }
}

fn peer_of(address: &Multiaddr) -> Option<PeerId> {
    match address.iter().last() {
        Some(Protocol::P2p(peer)) => Some(peer),
        _ => None,
    }
}

fn kad(swarm: &mut Swarm<Behaviour>) -> &mut kad::Behaviour<kad::store::MemoryStore> {
    swarm
        .behaviour_mut()
        .kad
        .as_mut()
        .expect("serverless swarms always run the DHT")
}

/// Start listening and dialing the bootstrap nodes; the DHT bootstraps once one answers.
fn join_network(swarm: &mut Swarm<Behaviour>, network: &Network) -> Result<()> {
    for address in &network.listen {
        swarm.listen_on(address.clone())?;
    }
    let mut known = 0;
    for address in network.bootstrap.iter().chain(&network.relays) {
        let Some(peer) = peer_of(address) else {
            continue;
        };
        if network.bootstrap.contains(address) {
            kad(swarm).add_address(&peer, address.clone());
        }
        swarm.add_peer_address(peer, address.clone());
        if swarm.dial(address.clone()).is_ok() {
            known += 1;
        }
    }
    ensure!(known > 0, "No network entry point is configured");
    Ok(())
}

/// Tracks relay reservations and the relayed connections that direct ones replace.
#[derive(Default)]
struct Links {
    reservations: HashMap<libp2p::core::transport::ListenerId, PeerId>,
    tried_relays: HashSet<PeerId>,
    relayed: HashMap<PeerId, Vec<ConnectionId>>,
    direct: HashMap<PeerId, usize>,
}

impl Links {
    fn reserve(&mut self, swarm: &mut Swarm<Behaviour>, relay: PeerId, address: Multiaddr) {
        if self.reservations.len() >= MAX_RELAYS || !self.tried_relays.insert(relay) {
            return;
        }
        let circuit = address
            .with(Protocol::P2p(relay))
            .with(Protocol::P2pCircuit);
        if let Ok(id) = swarm.listen_on(circuit) {
            self.reservations.insert(id, relay);
        }
    }

    /// A relay that advertises the hop protocol and a public address may hold our reservation.
    fn consider_relay(
        &mut self,
        swarm: &mut Swarm<Behaviour>,
        network: &Network,
        peer: PeerId,
        info: &identify::Info,
    ) {
        if !network.relays.is_empty() || !info.protocols.contains(&libp2p::relay::HOP_PROTOCOL_NAME)
        {
            return;
        }
        if let Some(address) = info.listen_addrs.iter().find(|address| is_public(address)) {
            let address: Multiaddr = address
                .iter()
                .filter(|p| !matches!(p, Protocol::P2p(_)))
                .collect();
            self.reserve(swarm, peer, address);
        }
    }

    /// Once a direct connection exists, relayed ones to the same peer are closed: streams
    /// pick a connection at random, and a public relay would cut the game after kilobytes.
    fn established(
        &mut self,
        swarm: &mut Swarm<Behaviour>,
        peer: PeerId,
        connection: ConnectionId,
        relayed: bool,
    ) -> bool {
        if relayed {
            if self.direct.contains_key(&peer) {
                swarm.close_connection(connection);
            } else {
                self.relayed.entry(peer).or_default().push(connection);
            }
            return false;
        }
        *self.direct.entry(peer).or_default() += 1;
        for old in self.relayed.remove(&peer).unwrap_or_default() {
            swarm.close_connection(old);
        }
        true
    }

    fn closed(&mut self, peer: PeerId, connection: ConnectionId, relayed: bool) {
        if relayed {
            if let Some(list) = self.relayed.get_mut(&peer) {
                list.retain(|c| *c != connection);
            }
        } else if let Some(count) = self.direct.get_mut(&peer) {
            *count -= 1;
            if *count == 0 {
                self.direct.remove(&peer);
            }
        }
    }
}

/// Answer one guest's key exchange, binding a grant to its peer id when it knows the code.
#[allow(clippy::too_many_arguments)]
async fn answer(
    stream: libp2p::Stream,
    guest: PeerId,
    host: PeerId,
    codes: Codes,
    access: access::Access,
    session_expires: u64,
    finished: mpsc::UnboundedSender<pake::RendezvousKey>,
    welcome: Arc<([u8; 32], String)>,
) -> Result<()> {
    let mut stream = stream.compat();
    let request = tokio::time::timeout(TIMEOUT, pake::read_request(&mut stream)).await??;
    let code = {
        let mut codes = codes.lock().unwrap();
        let state = codes
            .get_mut(&request.key)
            .context("Unknown or withdrawn code")?;
        ensure!(
            !state.done && !state.busy && state.expires > now(),
            "Code is not available"
        );
        // One attempt at a time, so guesses are counted before the next one starts.
        state.busy = true;
        state.code.clone()
    };
    let mut grant = None;
    let outcome = tokio::time::timeout(
        TIMEOUT,
        pake::host(&mut stream, &request, &code, host, guest, |token| {
            let issued = access.issue(token, session_expires)?;
            issued.bind(guest)?;
            grant = Some((issued, token));
            Ok(())
        }),
    )
    .await;
    {
        let mut codes = codes.lock().unwrap();
        let Some(state) = codes.get_mut(&request.key) else {
            bail!("Code withdrawn during the exchange");
        };
        state.busy = false;
        match outcome {
            Ok(Ok(pake::Outcome::Granted)) if !state.done => {
                if let Some((grant, token)) = grant {
                    state.grant = Some(grant);
                    state.friend = Some((guest, token));
                }
                finish(state, "active", None);
                state
                    .status
                    .send_modify(|status| status.peer = Some(guest.to_string()));
                let _ = finished.send(request.key);
            }
            Ok(Ok(pake::Outcome::Granted)) => {
                // Revoked while the exchange ran: the grant must not outlive the code.
                if let Some((grant, _)) = grant {
                    grant.revoke();
                }
                bail!("Code withdrawn during the exchange")
            }
            _ => {
                state.failures += 1;
                if state.failures >= MAX_FAILED_ATTEMPTS && !state.done {
                    finish(
                        state,
                        "failed",
                        Some("Слишком много попыток с неверным кодом"),
                    );
                    let _ = finished.send(request.key);
                }
                bail!("A guest used a wrong code")
            }
        }
    }
    // Where to find this host again; by itself it opens nothing.
    tokio::time::timeout(TIMEOUT, write_welcome(&mut stream, &welcome.0, &welcome.1)).await??;
    Ok(())
}

/// Host a loopback target for friends who hold serverless codes, and for the friends
/// `identity` remembers, who find the host at its meeting point.
pub async fn host_direct(
    target: SocketAddr,
    network: Network,
    ttl: Duration,
    identity: HostIdentity,
) -> Result<Session> {
    ensure!(
        target.ip().is_loopback() && target.port() != 0,
        "Target must be a concrete loopback port"
    );
    ensure!(
        ttl.as_secs() > 0 && ttl <= Duration::from_secs(86400),
        "Session lifetime must be 1..86400 seconds"
    );
    let mut swarm = swarm(identity.key, true)?;
    let local = *swarm.local_peer_id();
    let mut incoming = swarm.behaviour().streams.new_control().accept(PROTOCOL)?;
    let mut handshakes = swarm
        .behaviour()
        .streams
        .new_control()
        .accept(PAKE_PROTOCOL)?;
    let mut hellos = swarm
        .behaviour()
        .streams
        .new_control()
        .accept(FRIEND_PROTOCOL)?;
    join_network(&mut swarm, &network)?;
    let expires = now() + ttl.as_secs();
    let access = access::Access::default();
    for (friend, token) in identity
        .friends
        .into_iter()
        .take(crate::friends::MAX_FRIENDS)
    {
        if let Ok(grant) = access.issue(token, expires) {
            let _ = grant.bind(friend);
        }
    }
    let meeting = kad::RecordKey::new(&meeting_key(&identity.meeting));
    let welcome = Arc::new((identity.meeting, identity.label));
    let gate = Gate::Members(access.clone());
    let codes: Codes = Arc::default();
    let (commands_tx, mut commands) = mpsc::channel(8);
    let (finished_tx, mut finished) = mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let (status_tx, status) = watch::channel(Status::default());
    let task_commands = commands_tx.clone();
    let session_access = access.clone();
    let task = tokio::spawn(async move {
        let mut ready = Some(ready_tx);
        let mut links = Links::default();
        let mut bootstrapped = false;
        let mut tasks = JoinSet::new();
        let mut exchanges = JoinSet::new();
        let mut publishing: HashMap<kad::QueryId, pake::RendezvousKey> = HashMap::new();
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        let mut meeting_refresh = tokio::time::interval_at(
            tokio::time::Instant::now() + MEETING_REFRESH,
            MEETING_REFRESH,
        );
        let (seats, mut roster_events) = mpsc::unbounded_channel();
        let mut roster = Roster::default();
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                Some(event) = roster_events.recv() => {
                    roster.apply(event);
                    status_tx.send_modify(|s| roster.publish(s));
                }
                // Addresses change as relays come and go; friends need the current ones.
                _ = meeting_refresh.tick() => {
                    let _ = kad(&mut swarm).start_providing(meeting.clone());
                }
                _ = tick.tick() => {
                    let mut codes = codes.lock().unwrap();
                    for state in codes.values_mut() {
                        if !state.done && state.expires <= now() {
                            finish(state, "revoked", None);
                        }
                    }
                }
                event = swarm.select_next_some() => match event {
                    SwarmEvent::ConnectionEstablished { peer_id, connection_id, endpoint, .. } => {
                        let relayed = endpoint.is_relayed();
                        links.established(&mut swarm, peer_id, connection_id, relayed);
                        if !bootstrapped && network.bootstrap.iter().any(|a| peer_of(a) == Some(peer_id)) {
                            bootstrapped = kad(&mut swarm).bootstrap().is_ok();
                        }
                        if network.relays.iter().any(|a| peer_of(a) == Some(peer_id)) {
                            let address = network.relays.iter().find(|a| peer_of(a) == Some(peer_id)).cloned();
                            if let Some(address) = address {
                                let base: Multiaddr = address.iter().filter(|p| !matches!(p, Protocol::P2p(_))).collect();
                                links.reserve(&mut swarm, peer_id, base);
                            }
                        }
                    }
                    SwarmEvent::ConnectionClosed { peer_id, connection_id, endpoint, num_established, .. } => {
                        links.closed(peer_id, connection_id, endpoint.is_relayed());
                        if num_established == 0 { roster.disconnected(peer_id); }
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Ping(ping::Event { peer, result: Ok(rtt), .. })) => {
                        if roster.measured(peer, rtt) { status_tx.send_modify(|s| roster.publish(s)); }
                    }
                    SwarmEvent::NewListenAddr { address, listener_id } => {
                        if links.reservations.contains_key(&listener_id) && address.iter().any(|p| p == Protocol::P2pCircuit) {
                            if let Some(tx) = ready.take() {
                                let _ = tx.send(());
                                // Reachable now: remembered friends may look for this host.
                                let _ = kad(&mut swarm).start_providing(meeting.clone());
                            }
                        }
                    }
                    SwarmEvent::ListenerClosed { listener_id, .. } => {
                        links.reservations.remove(&listener_id);
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received { peer_id, info, .. })) => {
                        links.consider_relay(&mut swarm, &network, peer_id, &info);
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Dcutr(dcutr::Event { remote_peer_id, result: Err(error) })) => {
                        if !links.direct.contains_key(&remote_peer_id) {
                            status_tx.send_modify(|s| s.last_error = Some(format!("Direct connection failed: {error}")));
                        }
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::OutboundQueryProgressed { id, result: kad::QueryResult::StartProviding(result), .. })) => {
                        if let Some(key) = publishing.remove(&id) {
                            if let Some(state) = codes.lock().unwrap().get_mut(&key) {
                                if !state.done {
                                    state.status.send_modify(|status| match &result {
                                        Ok(_) => { status.phase = "waiting".into(); status.error = None; }
                                        // The record may still have reached some peers.
                                        Err(error) => { status.phase = "waiting".into(); status.error = Some(error.to_string()); }
                                    });
                                }
                            }
                        }
                    }
                    _ => {}
                },
                Some(command) = commands.recv() => match command {
                    Command::Issue { ttl, reply } => {
                        let code = pake::generate_code();
                        let key = pake::rendezvous_key(&pake::normalize(&code));
                        let code_expires = (now() + ttl.as_secs()).min(expires);
                        let result = kad(&mut swarm).start_providing(kad::RecordKey::new(&key));
                        let _ = reply.send(match result {
                            Ok(query) => {
                                publishing.insert(query, key);
                                let (tx, rx) = watch::channel(CodeStatus {
                                    id: hex::encode(&key[2..10]),
                                    code: code.clone(),
                                    expires_at: code_expires,
                                    phase: "publishing".into(),
                                    peer: None,
                                    error: None,
                                });
                                codes.lock().unwrap().insert(key, CodeState {
                                    code: pake::normalize(&code),
                                    expires: code_expires,
                                    failures: 0,
                                    busy: false,
                                    done: false,
                                    grant: None,
                                    friend: None,
                                    status: tx,
                                });
                                Ok(DirectCode { status: rx, key, commands: task_commands.clone(), codes: codes.clone() })
                            }
                            Err(error) => Err(anyhow::anyhow!("Cannot announce the code: {error:?}")),
                        });
                    }
                    Command::Revoke { key } => {
                        kad(&mut swarm).stop_providing(&kad::RecordKey::new(&key));
                        if let Some(state) = codes.lock().unwrap().get_mut(&key) {
                            if let Some(grant) = state.grant.take() { grant.revoke(); }
                            finish(state, "revoked", None);
                        }
                    }
                },
                Some(key) = finished.recv() => {
                    kad(&mut swarm).stop_providing(&kad::RecordKey::new(&key));
                }
                Some((guest, stream)) = handshakes.next() => {
                    if exchanges.len() >= MAX_HANDSHAKES { drop(stream); continue; }
                    exchanges.spawn(answer(stream, guest, local, codes.clone(), access.clone(), expires, finished_tx.clone(), welcome.clone()));
                }
                Some((guest, stream)) = hellos.next() => {
                    if exchanges.len() >= MAX_HANDSHAKES { drop(stream); continue; }
                    exchanges.spawn(greet(stream, guest, access.clone(), welcome.clone()));
                }
                Some(_) = exchanges.join_next(), if !exchanges.is_empty() => {}
                Some((remote_peer, stream)) = incoming.next() => {
                    if tasks.len() >= MAX_STREAMS { drop(stream); continue; }
                    tasks.spawn(serve_stream(stream, remote_peer, gate.clone(), target, expires, seats.clone()));
                    status_tx.send_modify(|s| s.active_connections = tasks.len());
                }
                Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                    status_tx.send_modify(|s| {
                        s.active_connections = tasks.len();
                        if let Err(e) = result.unwrap_or_else(|e| Err(e.into())) { s.last_error = Some(e.to_string()); }
                    });
                }
            }
        }
        for state in codes.lock().unwrap().values_mut() {
            if let Some(grant) = state.grant.take() {
                grant.revoke();
            }
        }
        exchanges.abort_all();
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    });
    let session = Session {
        invite: None,
        endpoint: None,
        status,
        cancel,
        task: Some(task),
        access: Some(session_access),
        direct: Some(commands_tx),
        world: None,
    };
    tokio::time::timeout(READY_TIMEOUT, ready_rx)
        .await
        .context("No relay of the public network accepted a reservation")?
        .context("Host stopped while joining the network")?;
    Ok(session)
}

/// What a successful attempt at the host brings: who it is, the capability, and the world
/// to remember when the host shared one.
type Entry = (PeerId, [u8; 32], Option<FriendWorld>);

/// Find the host behind `ticket` and get in: prove a code, or show a remembered friend's
/// capability. Local game connections are served once a direct connection exists. Returns
/// after the host let this computer in; the direct path may still be forming, and
/// `Status::transport` turns `direct` when it is ready. `key` is this computer's identity,
/// which the host binds the capability to.
pub async fn join_direct(
    ticket: Ticket,
    listener: TcpListener,
    network: Network,
    key: libp2p::identity::Keypair,
) -> Result<Session> {
    let endpoint = listener.local_addr()?;
    ensure!(
        endpoint.ip().is_loopback(),
        "Guest listener must bind loopback"
    );
    let rendezvous = match &ticket {
        Ticket::Code(code) => {
            ensure!(pake::is_direct_code(code), "Not a serverless code");
            pake::rendezvous_key(&pake::normalize(code))
        }
        Ticket::Friend(world) => meeting_key(&world.meeting),
    };
    let mut swarm = swarm(key, true)?;
    let local = *swarm.local_peer_id();
    let control = swarm.behaviour().streams.new_control();
    join_network(&mut swarm, &network)?;
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let (status_tx, status) = watch::channel(Status::default());
    let (accepted_tx, accepted_rx) = oneshot::channel::<Result<Option<FriendWorld>>>();
    let find_timeout = network.find_timeout;
    let friend = matches!(ticket, Ticket::Friend(_));
    let task = tokio::spawn(async move {
        let mut accepted = Some(accepted_tx);
        let mut links = Links::default();
        let mut bootstrapped = false;
        let mut searching = None;
        let mut retry = tokio::time::interval(SEARCH_RETRY);
        let mut tried = HashSet::new();
        let mut attempts = JoinSet::new();
        let mut host: Option<(PeerId, [u8; 32])> = None;
        let mut tasks = JoinSet::new();
        loop {
            let ready = host.is_some_and(|(peer, _)| links.direct.contains_key(&peer));
            tokio::select! {
                _ = stop.cancelled() => break,
                _ = retry.tick(), if bootstrapped && searching.is_none() && host.is_none() => {
                    searching = Some(kad(&mut swarm).get_providers(kad::RecordKey::new(&rendezvous)));
                }
                event = swarm.select_next_some() => match event {
                    SwarmEvent::ConnectionEstablished { peer_id, connection_id, endpoint, .. } => {
                        let relayed = endpoint.is_relayed();
                        let direct = links.established(&mut swarm, peer_id, connection_id, relayed);
                        if !bootstrapped && network.bootstrap.iter().any(|a| peer_of(a) == Some(peer_id)) {
                            bootstrapped = kad(&mut swarm).bootstrap().is_ok();
                            searching = Some(kad(&mut swarm).get_providers(kad::RecordKey::new(&rendezvous)));
                        }
                        if direct && host.is_some_and(|(peer, _)| peer == peer_id) {
                            status_tx.send_modify(|s| { s.transport = Some("direct".into()); s.last_error = None; });
                        }
                    }
                    SwarmEvent::ConnectionClosed { peer_id, connection_id, endpoint, .. } => {
                        links.closed(peer_id, connection_id, endpoint.is_relayed());
                        if host.is_some_and(|(peer, _)| peer == peer_id) && !links.direct.contains_key(&peer_id) {
                            status_tx.send_modify(|s| { s.transport = None; s.rtt_ms = None; });
                        }
                    }
                    // Relayed round trips say nothing about play, which runs only on the direct link.
                    SwarmEvent::Behaviour(BehaviourEvent::Ping(ping::Event { peer, connection, result: Ok(rtt) })) => {
                        let relayed = links.relayed.get(&peer).is_some_and(|relayed| relayed.contains(&connection));
                        if host.is_some_and(|(host, _)| host == peer) && !relayed {
                            status_tx.send_modify(|s| s.rtt_ms = Some(millis(&rtt)));
                        }
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Dcutr(dcutr::Event { remote_peer_id, result: Err(error) })) => {
                        if host.is_some_and(|(peer, _)| peer == remote_peer_id) && !links.direct.contains_key(&remote_peer_id) {
                            status_tx.send_modify(|s| s.last_error = Some(format!("Direct connection failed: {error}")));
                        }
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::OutboundQueryProgressed { id, result: kad::QueryResult::GetProviders(result), step, .. })) if Some(id) == searching => {
                        if step.last {
                            searching = None;
                        }
                        let Ok(kad::GetProvidersOk::FoundProviders { providers, .. }) = result else { continue; };
                        for provider in providers {
                            if provider == local || host.is_some() {
                                continue;
                            }
                            // Addresses come from the running query, so dial while it runs.
                            let mut control = control.clone();
                            match &ticket {
                                // Each provider gets one try: a wrong match costs the host a guess.
                                Ticket::Code(code) if tried.insert(provider) => {
                                    let code = pake::normalize(code);
                                    attempts.spawn(async move {
                                        let mut stream = tokio::time::timeout(TIMEOUT, control.open_stream(provider, PAKE_PROTOCOL)).await??.compat();
                                        let token = tokio::time::timeout(TIMEOUT, pake::guest(&mut stream, &code, provider, local)).await??;
                                        // A host without the welcome cannot be found again; the code still worked.
                                        let welcome = tokio::time::timeout(TIMEOUT, read_welcome(&mut stream)).await.ok().and_then(Result::ok);
                                        let world = welcome.map(|(meeting, label)| FriendWorld { host: provider, token, meeting, label });
                                        Ok::<Entry, anyhow::Error>((provider, token, world))
                                    });
                                }
                                // Only the remembered host counts; it may take a few searches to reach.
                                Ticket::Friend(world) if provider == world.host && attempts.is_empty() => {
                                    let mut world = world.clone();
                                    attempts.spawn(async move {
                                        let mut stream = tokio::time::timeout(TIMEOUT, control.open_stream(provider, FRIEND_PROTOCOL)).await??.compat();
                                        stream.write_all(&world.token).await?;
                                        tokio::time::timeout(TIMEOUT, async {
                                            ensure!(stream.read_u8().await? == 1, REFUSED);
                                            world.label = read_label(&mut stream).await?;
                                            Ok(())
                                        }).await??;
                                        Ok::<Entry, anyhow::Error>((provider, world.token, Some(world)))
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                },
                Some(result) = attempts.join_next(), if !attempts.is_empty() => {
                    match result.unwrap_or_else(|e| Err(e.into())) {
                        Ok((peer, token, world)) if host.is_none() => {
                            host = Some((peer, token));
                            attempts.abort_all();
                            if links.direct.contains_key(&peer) {
                                status_tx.send_modify(|s| s.transport = Some("direct".into()));
                            }
                            if let Some(tx) = accepted.take() { let _ = tx.send(Ok(world)); }
                        }
                        Ok(_) => {}
                        Err(error) if error.to_string() == REFUSED => {
                            if let Some(tx) = accepted.take() { let _ = tx.send(Err(error)); }
                            break;
                        }
                        Err(error) => status_tx.send_modify(|s| s.last_error = Some(error.to_string())),
                    }
                }
                accepted = listener.accept(), if ready && tasks.len() < MAX_STREAMS => {
                    let Ok((socket, _)) = accepted else { break; };
                    let Some((peer, token)) = host else { continue; };
                    tasks.spawn(forward(socket, control.clone(), peer, token, u64::MAX));
                    status_tx.send_modify(|s| s.active_connections = tasks.len());
                }
                Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                    status_tx.send_modify(|s| {
                        s.active_connections = tasks.len();
                        if let Err(e) = result.unwrap_or_else(|e| Err(e.into())) { s.last_error = Some(e.to_string()); }
                    });
                }
            }
        }
        attempts.abort_all();
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    });
    let mut session = Session {
        invite: None,
        endpoint: Some(endpoint),
        status,
        cancel,
        task: Some(task),
        access: None,
        direct: None,
        world: None,
    };
    match tokio::time::timeout(find_timeout, accepted_rx).await {
        Ok(Ok(Ok(world))) => {
            session.world = world;
            Ok(session)
        }
        Ok(Ok(Err(error))) => {
            session.stop().await;
            Err(error)
        }
        _ => {
            let reason = session.status.borrow().last_error.clone();
            session.stop().await;
            match reason {
                Some(reason) if reason.contains("does not match") => {
                    bail!("The code does not match this host")
                }
                _ if friend => bail!("The friend's world was not found"),
                _ => bail!("No host with this code was found"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses_qualify_for_relays() {
        for public in [
            "/ip4/104.131.131.82/tcp/4001",
            "/ip6/2604:a880:1:20::203:d001/udp/4001/quic-v1",
            "/dns4/relay.example.org/tcp/4001",
        ] {
            assert!(is_public(&public.parse().unwrap()), "{public}");
        }
        for private in [
            "/ip4/127.0.0.1/tcp/4001",
            "/ip4/192.168.1.5/tcp/4001",
            "/ip4/10.0.0.1/tcp/4001",
            "/ip4/100.64.1.1/tcp/4001",
            "/ip6/::1/tcp/4001",
            "/ip6/fd00::1/tcp/4001",
            "/ip6/fe80::1/tcp/4001",
            "/ip4/104.131.131.82/tcp/4001/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ/p2p-circuit",
        ] {
            assert!(!is_public(&private.parse().unwrap()), "{private}");
        }
    }

    #[test]
    fn the_public_network_has_usable_entry_points() {
        let network = Network::public();
        assert_eq!(network.bootstrap.len(), PUBLIC_BOOTSTRAP.len());
        assert!(network.bootstrap.iter().all(|a| peer_of(a).is_some()));
        assert!(network.relays.is_empty());
    }
}
