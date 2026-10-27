use blockops_tunnel::{host, join, now};
use std::{net::SocketAddr, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    task::JoinHandle,
    time::timeout,
};

struct Echo {
    address: SocketAddr,
    task: JoinHandle<()>,
}
impl Drop for Echo {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn echo() -> Echo {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut socket,_) = accepted.unwrap();
                    tasks.spawn(async move { let (mut r,mut w) = socket.split(); let _ = tokio::io::copy(&mut r,&mut w).await; });
                }
                _ = tasks.join_next(), if !tasks.is_empty() => {}
            }
        }
    });
    Echo { address, task }
}
async fn relay() -> (Child, libp2p::Multiaddr) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_blockops-relay"))
        .args(["--listen", "/ip4/127.0.0.1/tcp/0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut line = String::new();
    timeout(
        Duration::from_secs(10),
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line),
    )
    .await
    .unwrap()
    .unwrap();
    (child, line.trim().parse().unwrap())
}
async fn exchange(endpoint: SocketAddr, size: usize) {
    timeout(Duration::from_secs(15), async {
        let mut client = TcpStream::connect(endpoint).await.unwrap();
        let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        client.write_all(&data).await.unwrap();
        client.shutdown().await.unwrap();
        let mut output = Vec::new();
        client.read_to_end(&mut output).await.unwrap();
        assert_eq!(output, data);
    })
    .await
    .expect("TCP exchange timed out");
}
#[tokio::test]
async fn direct_bidirectional_concurrent_and_stop() {
    let echo = echo().await;
    let host = host(echo.address, None, Duration::from_secs(60))
        .await
        .unwrap();
    let guest = join(host.invite.clone().unwrap(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let endpoint = guest.endpoint.unwrap();
    tokio::join!(exchange(endpoint, 16000), exchange(endpoint, 32000));
    let mut active = TcpStream::connect(endpoint).await.unwrap();
    active.write_all(b"live").await.unwrap();
    let mut data = [0; 4];
    timeout(Duration::from_secs(10), active.read_exact(&mut data))
        .await
        .unwrap()
        .unwrap();
    host.stop().await;
    let result = timeout(Duration::from_secs(5), active.read(&mut data))
        .await
        .expect("active connection remained open");
    assert!(matches!(result, Ok(0) | Err(_)));
    guest.stop().await;
    assert!(TcpStream::connect(endpoint).await.is_err());
}
#[tokio::test]
async fn separate_relay_process_transfers_tcp() {
    let (_relay, address) = relay().await;
    let echo = echo().await;
    let host = host(echo.address, Some(address), Duration::from_secs(60))
        .await
        .unwrap();
    assert!(host
        .invite
        .as_ref()
        .unwrap()
        .address
        .contains("/p2p-circuit/"));
    let guest = join(host.invite.clone().unwrap(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    exchange(guest.endpoint.unwrap(), 262144).await;
    assert_eq!(guest.status.borrow().transport.as_deref(), Some("relay"));
    guest.stop().await;
    host.stop().await;
}
#[tokio::test]
async fn wrong_capability_never_reaches_target() {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = host(target.local_addr().unwrap(), None, Duration::from_secs(60))
        .await
        .unwrap();
    let mut invite = host.invite.clone().unwrap();
    invite.token = "00".repeat(32);
    let guest = join(invite, "127.0.0.1:0".parse().unwrap()).await.unwrap();
    let mut client = TcpStream::connect(guest.endpoint.unwrap()).await.unwrap();
    client.write_all(b"unauthorized").await.unwrap();
    let mut b = [0; 1];
    let result = timeout(Duration::from_secs(10), client.read(&mut b))
        .await
        .unwrap();
    assert!(matches!(result, Ok(0) | Err(_)));
    assert!(timeout(Duration::from_millis(200), target.accept())
        .await
        .is_err());
    guest.stop().await;
    host.stop().await;
}
#[tokio::test]
async fn rejects_expiry_and_non_loopback_addresses() {
    assert!(host(
        "0.0.0.0:25565".parse().unwrap(),
        None,
        Duration::from_secs(60)
    )
    .await
    .is_err());
    assert!(host(
        "192.168.1.1:25565".parse().unwrap(),
        None,
        Duration::from_secs(60)
    )
    .await
    .is_err());
    let echo = echo().await;
    let host = host(echo.address, None, Duration::from_secs(60))
        .await
        .unwrap();
    let mut invite = host.invite.clone().unwrap();
    assert!(join(invite.clone(), "0.0.0.0:0".parse().unwrap())
        .await
        .is_err());
    invite.expires_at = now() - 1;
    assert!(join(invite, "127.0.0.1:0".parse().unwrap()).await.is_err());
    host.stop().await;
}
#[tokio::test]
async fn unreachable_target_reports_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = listener.local_addr().unwrap();
    drop(listener);
    let host = host(target, None, Duration::from_secs(60)).await.unwrap();
    let guest = join(host.invite.clone().unwrap(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let mut client = TcpStream::connect(guest.endpoint.unwrap()).await.unwrap();
    let mut b = [0; 1];
    let result = timeout(Duration::from_secs(10), client.read(&mut b))
        .await
        .unwrap();
    assert!(matches!(result, Ok(0) | Err(_)));
    assert!(host.status.borrow().last_error.is_some());
    guest.stop().await;
    host.stop().await;
}

#[tokio::test]
async fn host_enforces_expiry_even_if_guest_edits_invite() {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = host(target.local_addr().unwrap(), None, Duration::from_secs(1))
        .await
        .unwrap();
    let mut invite = host.invite.clone().unwrap();
    invite.expires_at = now() + 3600;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let guest = join(invite, "127.0.0.1:0".parse().unwrap()).await.unwrap();
    let mut client = TcpStream::connect(guest.endpoint.unwrap()).await.unwrap();
    let mut b = [0; 1];
    let result = timeout(Duration::from_secs(10), client.read(&mut b))
        .await
        .unwrap();
    assert!(matches!(result, Ok(0) | Err(_)));
    assert!(timeout(Duration::from_millis(100), target.accept())
        .await
        .is_err());
    guest.stop().await;
    host.stop().await;
}
