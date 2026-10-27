use blockops_coordination::{
    client::Client,
    service::{serve, Store},
};
use blockops_tunnel::{host_controlled, join_with_identity};
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
struct Task(tokio::task::JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn service() -> (Task, Client, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let task = Task(tokio::spawn(async move {
        serve(listener, Store::default()).await.unwrap();
    }));
    (task, Client::new(&url).unwrap(), url)
}
async fn echo() -> (Task, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = Task(tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                result=listener.accept()=>{let(mut socket,_)=result.unwrap();tasks.spawn(async move{let(r,w)=socket.split();let _=tokio::io::copy(&mut {r},&mut {w}).await;});}
                _=tasks.join_next(),if !tasks.is_empty()=>{}
            }
        }
    }));
    (task, address)
}
async fn exchange(endpoint: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(endpoint).await.unwrap();
    stream.write_all(b"minecraft TCP").await.unwrap();
    let mut data = [0; 13];
    timeout(Duration::from_secs(5), stream.read_exact(&mut data))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&data, b"minecraft TCP");
    stream
}
#[tokio::test]
async fn code_is_single_use_and_identity_bound_and_revocable() {
    let (_service, client, _) = service().await;
    let (_echo, target) = echo().await;
    let host = host_controlled(target, None, Duration::from_secs(60))
        .await
        .unwrap();
    let code = client.publish(&host, 30).await.unwrap();
    let secret = code.status.borrow().code.clone();
    let key = libp2p::identity::Keypair::generate_ed25519();
    let invite = client
        .redeem(&secret, key.public().to_peer_id())
        .await
        .unwrap();
    assert!(client
        .redeem(&secret, libp2p::PeerId::random())
        .await
        .is_err());
    let wrong = join_with_identity(
        invite.clone(),
        "127.0.0.1:0".parse().unwrap(),
        libp2p::identity::Keypair::generate_ed25519(),
    )
    .await
    .unwrap();
    let mut bad = TcpStream::connect(wrong.endpoint.unwrap()).await.unwrap();
    let mut byte = [0; 1];
    assert!(matches!(
        timeout(Duration::from_secs(5), bad.read(&mut byte))
            .await
            .unwrap(),
        Ok(0) | Err(_)
    ));
    wrong.stop().await;
    let guest = join_with_identity(invite, "127.0.0.1:0".parse().unwrap(), key)
        .await
        .unwrap();
    let mut active = exchange(guest.endpoint.unwrap()).await;
    code.revoke().await;
    assert!(matches!(
        timeout(Duration::from_secs(5), active.read(&mut byte))
            .await
            .unwrap(),
        Ok(0) | Err(_)
    ));
    let mut later = TcpStream::connect(guest.endpoint.unwrap()).await.unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(5), later.read(&mut byte))
            .await
            .unwrap(),
        Ok(0) | Err(_)
    ));
    guest.stop().await;
    host.stop().await;
}
#[tokio::test]
async fn simultaneous_redemption_has_exactly_one_winner() {
    let (_service, client, _) = service().await;
    let (_echo, target) = echo().await;
    let host = host_controlled(target, None, Duration::from_secs(60))
        .await
        .unwrap();
    let code = client.publish(&host, 30).await.unwrap();
    let secret = code.status.borrow().code.clone();
    let (a, b) = tokio::join!(
        client.redeem(&secret, libp2p::PeerId::random()),
        client.redeem(&secret, libp2p::PeerId::random())
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    code.revoke().await;
    host.stop().await;
}
#[tokio::test]
async fn pending_code_expires() {
    let (_service, client, _) = service().await;
    let (_echo, target) = echo().await;
    let host = host_controlled(target, None, Duration::from_secs(60))
        .await
        .unwrap();
    let code = client.publish(&host, 1).await.unwrap();
    let secret = code.status.borrow().code.clone();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(client
        .redeem(&secret, libp2p::PeerId::random())
        .await
        .is_err());
    code.revoke().await;
    host.stop().await;
}
#[tokio::test]
async fn coordinator_outage_revokes_active_member() {
    let (service, client, _) = service().await;
    let (_echo, target) = echo().await;
    let host = host_controlled(target, None, Duration::from_secs(60))
        .await
        .unwrap();
    let code = client.publish(&host, 30).await.unwrap();
    let secret = code.status.borrow().code.clone();
    let guest = client
        .join(&secret, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let mut active = exchange(guest.endpoint.unwrap()).await;
    service.0.abort();
    let mut status = code.status.clone();
    timeout(Duration::from_secs(6), async {
        loop {
            if status.borrow().phase == "failed" {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let mut byte = [0; 1];
    assert!(matches!(
        timeout(Duration::from_secs(5), active.read(&mut byte))
            .await
            .unwrap(),
        Ok(0) | Err(_)
    ));
    code.revoke().await;
    guest.stop().await;
    host.stop().await;
}
#[tokio::test]
async fn http_management_requires_secret_and_redeem_is_rate_limited() {
    let (_service, client, url) = service().await;
    let (_echo, target) = echo().await;
    let host = host_controlled(target, None, Duration::from_secs(60))
        .await
        .unwrap();
    let code = client.publish(&host, 30).await.unwrap();
    let id = code.status.borrow().id.clone();
    let http = reqwest::Client::new();
    let response = http
        .delete(format!("{url}v1/invites/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    for _ in 0..30 {
        let _=http.post(format!("{url}v1/redeem")).json(&serde_json::json!({"code":"0000-0000-0000","peer":libp2p::PeerId::random().to_string()})).send().await.unwrap();
    }
    let response=http.post(format!("{url}v1/redeem")).json(&serde_json::json!({"code":"0000-0000-0000","peer":libp2p::PeerId::random().to_string()})).send().await.unwrap();
    assert_eq!(response.status(), 429);
    code.revoke().await;
    host.stop().await;
}
#[test]
fn client_rejects_insecure_remote_urls() {
    for url in [
        "http://example.com",
        "https://user:password@example.com",
        "https://example.com/extra",
        "https://example.com/?token=x",
    ] {
        assert!(Client::new(url).is_err());
    }
    assert!(Client::new("https://example.com").is_ok());
    assert!(Client::new("http://127.0.0.1:8080").is_ok());
}

#[tokio::test]
async fn revoking_one_guest_preserves_another() {
    let (_service, client, _) = service().await;
    let (_echo, target) = echo().await;
    let host = host_controlled(target, None, Duration::from_secs(60))
        .await
        .unwrap();
    let first = client.publish(&host, 30).await.unwrap();
    let second = client.publish(&host, 30).await.unwrap();
    let first_code = first.status.borrow().code.clone();
    let second_code = second.status.borrow().code.clone();
    let a = client
        .join(&first_code, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let b = client
        .join(&second_code, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let mut a_stream = exchange(a.endpoint.unwrap()).await;
    first.revoke().await;
    let mut byte = [0; 1];
    assert!(matches!(
        timeout(Duration::from_secs(5), a_stream.read(&mut byte))
            .await
            .unwrap(),
        Ok(0) | Err(_)
    ));
    drop(exchange(b.endpoint.unwrap()).await);
    second.revoke().await;
    a.stop().await;
    b.stop().await;
    host.stop().await;
}

#[tokio::test]
async fn invalid_or_occupied_guest_port_does_not_consume_invite() {
    let (_service, client, _) = service().await;
    let (_echo, target) = echo().await;
    let host = host_controlled(target, None, Duration::from_secs(60))
        .await
        .unwrap();
    let code = client.publish(&host, 30).await.unwrap();
    let secret = code.status.borrow().code.clone();
    let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = occupied.local_addr().unwrap();
    assert!(client
        .join(&secret, "0.0.0.0:0".parse().unwrap())
        .await
        .is_err());
    assert!(client.join(&secret, address).await.is_err());
    // The same code is still usable by a valid guest.
    let guest = client
        .join(&secret, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    drop(exchange(guest.endpoint.unwrap()).await);
    code.revoke().await;
    guest.stop().await;
    host.stop().await;
}
