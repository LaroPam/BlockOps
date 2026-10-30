//! The host sees who logs in through the tunnel, server list pings aside, and both sides
//! measure the round trip between the two computers.
use blockops_tunnel::{host, join, Status};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
};

fn varint(mut value: u32, out: &mut Vec<u8>) {
    loop {
        if value & !0x7f == 0 {
            out.push(value as u8);
            return;
        }
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
}
fn packet(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    varint(body.len() as u32, &mut out);
    out.extend_from_slice(body);
    out
}
fn handshake(next: u32) -> Vec<u8> {
    let mut body = vec![0];
    varint(767, &mut body);
    varint(9, &mut body);
    body.extend_from_slice(b"127.0.0.1");
    body.extend_from_slice(&25565u16.to_be_bytes());
    varint(next, &mut body);
    packet(&body)
}

async fn wait(status: &mut watch::Receiver<Status>, done: impl Fn(&Status) -> bool) -> Status {
    tokio::time::timeout(Duration::from_secs(30), status.wait_for(|s| done(s)))
        .await
        .expect("status did not change in time")
        .unwrap()
        .clone()
}

/// Send `bytes` through the tunnel to the echoing world and check they come back unchanged.
async fn round_trip(game: &mut TcpStream, bytes: &[u8]) {
    game.write_all(bytes).await.unwrap();
    let mut echoed = vec![0; bytes.len()];
    game.read_exact(&mut echoed).await.unwrap();
    assert_eq!(echoed, bytes);
}

#[tokio::test]
async fn logins_take_a_seat_and_server_list_pings_do_not() {
    // A stand-in for the world: it echoes whatever it receives.
    let world = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = world.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = world.accept().await {
            tokio::spawn(async move {
                let (mut reader, mut writer) = socket.split();
                let _ = tokio::io::copy(&mut reader, &mut writer).await;
            });
        }
    });
    let host = host(target, None, Duration::from_secs(60)).await.unwrap();
    let guest = join(host.invite.clone().unwrap(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let endpoint = guest.endpoint.unwrap();
    let mut status = host.status.clone();

    // The server list asks for the status: answered, but nobody joins.
    let mut ping = TcpStream::connect(endpoint).await.unwrap();
    let mut request = handshake(1);
    request.extend(packet(&[0]));
    round_trip(&mut ping, &request).await;
    assert!(status.borrow().players.is_empty());
    drop(ping);

    // A login takes a seat under the player's name for as long as the connection lasts.
    let mut game = TcpStream::connect(endpoint).await.unwrap();
    let mut login = handshake(2);
    let mut start = vec![0, 5];
    start.extend_from_slice(b"Steve");
    start.extend_from_slice(&[7; 16]);
    login.extend(packet(&start));
    round_trip(&mut game, &login).await;
    let players = wait(&mut status, |s| !s.players.is_empty()).await.players;
    assert_eq!(players.len(), 1);
    assert_eq!(players[0].name.as_deref(), Some("Steve"));
    // Play continues through the tunnel as before.
    round_trip(&mut game, b"after login").await;

    // Both sides measure the round trip.
    wait(&mut guest.status.clone(), |s| s.rtt_ms.is_some()).await;
    wait(&mut status, |s| {
        s.players.iter().all(|p| p.rtt_ms.is_some())
    })
    .await;

    drop(game);
    wait(&mut status, |s| s.players.is_empty()).await;
    guest.stop().await;
    host.stop().await;
}
