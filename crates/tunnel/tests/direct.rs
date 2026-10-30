//! The serverless mode end to end, with one local node standing in for the public network:
//! it serves the DHT and a relay with the public network's tight circuit limits.
use blockops_tunnel::{
    direct::{host_direct, join_direct, HostIdentity, Network, Ticket},
    pake,
};
use futures::StreamExt;
use libp2p::identity::Keypair;
use libp2p::{
    identify, kad, noise, relay,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, SwarmBuilder,
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

#[derive(NetworkBehaviour)]
struct PublicNode {
    relay: relay::Behaviour,
    identify: identify::Behaviour,
    kad: kad::Behaviour<kad::store::MemoryStore>,
}

/// A DHT server and relay on loopback, returning its full address.
async fn public_node() -> Multiaddr {
    let mut swarm = SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_behaviour(|key| {
            let peer = key.public().to_peer_id();
            let mut kad = kad::Behaviour::with_config(
                peer,
                kad::store::MemoryStore::new(peer),
                kad::Config::new(kad::PROTOCOL_NAME),
            );
            kad.set_mode(Some(kad::Mode::Server));
            PublicNode {
                // The defaults of public relays: circuits of two minutes and 128 KiB.
                relay: relay::Behaviour::new(peer, relay::Config::default()),
                identify: identify::Behaviour::new(identify::Config::new(
                    "/ipfs/0.1.0".into(),
                    key.public(),
                )),
                kad,
            }
        })
        .unwrap()
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();
    swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .unwrap();
    let peer = *swarm.local_peer_id();
    let address = loop {
        if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
            break address;
        }
    };
    swarm.add_external_address(address.clone());
    tokio::spawn(async move {
        loop {
            swarm.select_next_some().await;
        }
    });
    address.with(libp2p::multiaddr::Protocol::P2p(peer))
}

fn network(node: &Multiaddr) -> Network {
    Network {
        bootstrap: vec![node.clone()],
        relays: vec![node.clone()],
        listen: vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()],
        find_timeout: Duration::from_secs(20),
    }
}

async fn echo() -> (tokio::task::JoinHandle<()>, std::net::SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let (mut read, mut write) = socket.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    (task, address)
}

#[tokio::test]
async fn players_meet_through_the_network_and_play_directly() {
    let node = public_node().await;
    let (_echo, target) = echo().await;
    let host = host_direct(
        target,
        network(&node),
        Duration::from_secs(600),
        HostIdentity::ephemeral("Steve - World"),
    )
    .await
    .unwrap();
    let code = host
        .issue_direct_code(Duration::from_secs(300))
        .await
        .unwrap();
    let typed = code.status.borrow().code.to_lowercase();
    assert!(pake::is_direct_code(&typed));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let guest = join_direct(
        Ticket::Code(typed.clone()),
        listener,
        network(&node),
        Keypair::generate_ed25519(),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut status = guest.status.clone();
        while status.borrow().transport.as_deref() != Some("direct") {
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("no direct connection");
    assert_eq!(code.status.borrow().phase, "active");
    assert!(code.status.borrow().peer.is_some());

    // More than a public relay's 128 KiB limit: only a direct path carries it.
    let payload: Vec<u8> = (0..512 * 1024).map(|i| (i % 251) as u8).collect();
    let mut stream = TcpStream::connect(guest.endpoint.unwrap()).await.unwrap();
    let (mut read, mut write) = stream.split();
    let sent = payload.clone();
    let (written, received) = tokio::join!(write.write_all(&sent), async {
        let mut received = vec![0; payload.len()];
        tokio::time::timeout(Duration::from_secs(20), read.read_exact(&mut received))
            .await
            .expect("echo timed out")
            .unwrap();
        received
    });
    written.unwrap();
    assert_eq!(received, payload);

    // The code is spent: a second guest finds no one who will accept it.
    let again = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut slow = network(&node);
    slow.find_timeout = Duration::from_secs(8);
    assert!(join_direct(
        Ticket::Code(typed.clone()),
        again,
        slow,
        Keypair::generate_ed25519()
    )
    .await
    .is_err());

    // Revoking the friend's code ends their game.
    code.revoke().await;
    let mut buffer = [0; 1];
    let closed = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buffer))
        .await
        .expect("stream stayed open after revocation");
    assert!(matches!(closed, Ok(0) | Err(_)));
    assert_eq!(code.status.borrow().phase, "revoked");
    guest.stop().await;
    host.stop().await;
}

#[tokio::test]
async fn an_unknown_code_finds_no_host() {
    let node = public_node().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut quick = network(&node);
    quick.find_timeout = Duration::from_secs(6);
    let error = join_direct(
        Ticket::Code(pake::generate_code()),
        listener,
        quick,
        Keypair::generate_ed25519(),
    )
    .await
    .err()
    .expect("joined without a host");
    assert!(error.to_string().contains("No host"), "{error}");
}

/// Wait until the guest's game connections can use the direct link.
async fn direct(guest: &blockops_tunnel::Session) {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut status = guest.status.clone();
        while status.borrow().transport.as_deref() != Some("direct") {
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("no direct connection");
}

async fn echoes(stream: &mut TcpStream, bytes: &[u8]) {
    stream.write_all(bytes).await.unwrap();
    let mut received = vec![0; bytes.len()];
    tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut received))
        .await
        .expect("echo timed out")
        .unwrap();
    assert_eq!(received, bytes);
}

#[tokio::test]
async fn remembered_friends_come_back_without_a_code() {
    let node = public_node().await;
    let (_echo, target) = echo().await;
    let host_key = Keypair::generate_ed25519();
    let guest_key = Keypair::generate_ed25519();
    let identity = |friends| HostIdentity {
        key: host_key.clone(),
        meeting: [9; 32],
        label: "Steve - World".into(),
        friends,
    };

    // The first visit uses a code; both sides learn what they need to meet again.
    let host = host_direct(
        target,
        network(&node),
        Duration::from_secs(600),
        identity(vec![]),
    )
    .await
    .unwrap();
    let code = host
        .issue_direct_code(Duration::from_secs(300))
        .await
        .unwrap();
    let typed = code.status.borrow().code.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let guest = join_direct(
        Ticket::Code(typed),
        listener,
        network(&node),
        guest_key.clone(),
    )
    .await
    .unwrap();
    let world = guest.world.clone().expect("the host shared its world");
    assert_eq!(world.host, host_key.public().to_peer_id());
    assert_eq!(world.meeting, [9; 32]);
    assert_eq!(world.label, "Steve - World");
    let (friend, token) = code.friend().expect("the host knows who used the code");
    assert_eq!(friend, guest_key.public().to_peer_id());
    assert_eq!(token, world.token);
    guest.stop().await;
    host.stop().await;

    // Another day: the host remembers the friend, who comes in without a code.
    let host = host_direct(
        target,
        network(&node),
        Duration::from_secs(600),
        identity(vec![(friend, token)]),
    )
    .await
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let guest = join_direct(
        Ticket::Friend(world.clone()),
        listener,
        network(&node),
        guest_key.clone(),
    )
    .await
    .unwrap();
    assert_eq!(guest.world.as_ref().unwrap().label, "Steve - World");
    direct(&guest).await;
    let mut game = TcpStream::connect(guest.endpoint.unwrap()).await.unwrap();
    echoes(&mut game, b"back again").await;

    // The capability is useless from another computer.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let error = join_direct(
        Ticket::Friend(world.clone()),
        listener,
        network(&node),
        Keypair::generate_ed25519(),
    )
    .await
    .err()
    .expect("a stranger came in");
    assert!(error.to_string().contains("no longer lets"), "{error}");

    // Forgetting the friend ends the game at once and closes the door.
    host.forget(friend);
    let mut buffer = [0; 1];
    let closed = tokio::time::timeout(Duration::from_secs(10), game.read(&mut buffer))
        .await
        .expect("the game stayed open after forgetting");
    assert!(matches!(closed, Ok(0) | Err(_)));
    guest.stop().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let error = join_direct(Ticket::Friend(world), listener, network(&node), guest_key)
        .await
        .err()
        .expect("a forgotten friend came in");
    assert!(error.to_string().contains("no longer lets"), "{error}");
    host.stop().await;
}
