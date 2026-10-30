//! LAN worlds announced over real multicast, as Minecraft does, with a fake status server.
use blockops_coordination::lan::LanWorlds;
use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Answers the status ping like a Minecraft server of `version`.
async fn status_server(version: &'static str) -> u16 {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut request = [0u8; 256];
                let _ = socket.read(&mut request).await;
                let json = format!(
                    r#"{{"version":{{"name":"{version}","protocol":763}},"description":{{"text":"A Minecraft Server"}}}}"#
                );
                let mut body = vec![0x00, json.len() as u8];
                body.extend_from_slice(json.as_bytes());
                let mut packet = vec![body.len() as u8];
                packet.extend_from_slice(&body);
                let _ = socket.write_all(&packet).await;
            });
        }
    });
    port
}

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn announced_worlds_are_found_versioned_and_followed_until_closed() {
    let announce_port = free_udp_port();
    let worlds = LanWorlds::listen_on(announce_port).unwrap();
    let game_port = status_server("1.20.1").await;
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    let announcer = tokio::spawn(async move {
        let socket = tokio::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .await
            .unwrap();
        socket.set_multicast_loop_v4(true).unwrap();
        let target = SocketAddr::from((Ipv4Addr::new(224, 0, 2, 60), announce_port));
        let message = format!("[MOTD]Steve - \u{a7}aNew World[/MOTD][AD]{game_port}[/AD]");
        loop {
            socket.send_to(message.as_bytes(), target).await.unwrap();
            tokio::select! {
                _ = &mut stop_rx => return,
                _ = tokio::time::sleep(Duration::from_millis(300)) => {}
            }
        }
    });
    let found = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(world) = worlds
                .current()
                .into_iter()
                .find(|world| world.port == game_port && world.version.is_some())
            {
                return world;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("announced world was not found");
    assert_eq!(found.name, "Steve - New World");
    assert_eq!(found.version.as_deref(), Some("1.20.1"));

    let mut watch = worlds.watch(game_port);
    assert!(*watch.alive.borrow());
    stop_tx.send(()).unwrap();
    announcer.await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *watch.alive.borrow() {
            watch.alive.changed().await.unwrap();
        }
    })
    .await
    .expect("world still followed after its announcements stopped");
    assert!(worlds.current().iter().all(|world| world.port != game_port));
}
