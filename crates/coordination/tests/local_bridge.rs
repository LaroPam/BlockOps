use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Router,
};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
};
use tokio::sync::Mutex;

const TOKEN: [u8; 32] = [0xab; 32];

/// Independent implementation of the companion's side of the protocol.
fn mac(direction: &str, path: &str, nonce: &str, value: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(&TOKEN).unwrap();
    mac.update(format!("blockops-bridge-{direction}-v1\n{path}\n{nonce}\n{value}").as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn code(port: u16) -> String {
    format!("{port}:{}", hex::encode(TOKEN))
}

/// The nonce from a raw HTTP request, for hand-written chunked responses.
fn raw_nonce(request: &[u8]) -> String {
    let text = String::from_utf8_lossy(request);
    text.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("x-blockops-nonce")
                .then(|| value.trim().to_owned())
        })
        .unwrap()
}

#[derive(Clone, Copy, PartialEq)]
enum Proof {
    Valid,
    Missing,
    Forged,
}
type JoinState = Arc<Mutex<(Proof, Vec<HeaderMap>)>>;

async fn join_companion(
    State(state): State<JoinState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    assert!(body.is_empty());
    let mut state = state.lock().await;
    let nonce = headers["x-blockops-nonce"].to_str().unwrap().to_owned();
    let port = headers["x-blockops-port"].to_str().unwrap().to_owned();
    assert_eq!(
        headers["authorization"].to_str().unwrap(),
        format!(
            "BlockOps-HMAC {}",
            mac("request", "/v1/join", &nonce, &port)
        )
    );
    let proof = match state.0 {
        Proof::Valid => Some(mac("response", "/v1/join", &nonce, "")),
        Proof::Missing => None,
        // Correct key, but bound to a different request.
        Proof::Forged => Some(mac("response", "/v1/join", &"0".repeat(64), "")),
    };
    state.1.push(headers);
    let mut response = StatusCode::ACCEPTED.into_response();
    if let Some(proof) = proof {
        response
            .headers_mut()
            .insert("x-blockops-proof", proof.parse().unwrap());
    }
    response
}

#[tokio::test]
async fn local_client_authenticates_without_revealing_the_capability() {
    let state: JoinState = Arc::new(Mutex::new((Proof::Valid, Vec::new())));
    let app = Router::new()
        .route("/v1/join", post(join_companion))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let public: SocketAddr = "192.0.2.1:25565".parse().unwrap();
    assert!(
        blockops_coordination::local_bridge::request_join(port, &TOKEN, public)
            .await
            .is_err()
    );
    assert!(state.lock().await.1.is_empty());
    let endpoint: SocketAddr = (Ipv4Addr::LOCALHOST, 32123).into();
    blockops_coordination::local_bridge::request_join(port, &TOKEN, endpoint)
        .await
        .unwrap();
    blockops_coordination::local_bridge::request_join(port, &TOKEN, endpoint)
        .await
        .unwrap();
    for proof in [Proof::Missing, Proof::Forged] {
        state.lock().await.0 = proof;
        assert!(
            blockops_coordination::local_bridge::request_join(port, &TOKEN, endpoint)
                .await
                .is_err(),
            "accepted a companion that did not prove the capability"
        );
    }
    let received = std::mem::take(&mut state.lock().await.1);
    assert_eq!(received.len(), 4);
    let secret = hex::encode(TOKEN);
    for headers in &received {
        assert_eq!(headers["x-blockops-port"], "32123");
        for value in headers.values() {
            assert!(!value.to_str().unwrap().contains(&secret));
        }
    }
    assert_ne!(
        received[0]["x-blockops-nonce"], received[1]["x-blockops-nonce"],
        "nonce reused"
    );
    task.abort();
}

#[tokio::test]
async fn malformed_pairing_codes_never_appear_in_errors() {
    let endpoint = (Ipv4Addr::LOCALHOST, 25565).into();
    for code in [
        "1234:secret-not-hex",
        "0:",
        "65536:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "1:00",
        "localhost:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        let error = blockops_coordination::local_bridge::request_paired_join(code, endpoint)
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains(code));
    }
}

type WorldState = Arc<Mutex<(String, Option<String>)>>;

#[tokio::test]
async fn world_port_requires_a_valid_bounded_proven_response() {
    // (body, payload the proof covers; None means the proof matches the body)
    let response: WorldState = Arc::new(Mutex::new(("54321".to_owned(), None)));
    let app = Router::new()
        .route(
            "/v1/world",
            post(
                |State(value): State<WorldState>, headers: HeaderMap| async move {
                    let (body, proven) = value.lock().await.clone();
                    let nonce = headers["x-blockops-nonce"].to_str().unwrap();
                    assert_eq!(
                        headers["authorization"].to_str().unwrap(),
                        format!("BlockOps-HMAC {}", mac("request", "/v1/world", nonce, ""))
                    );
                    let proof = mac(
                        "response",
                        "/v1/world",
                        nonce,
                        proven.as_ref().unwrap_or(&body),
                    );
                    ([("x-blockops-proof", proof)], body)
                },
            ),
        )
        .with_state(response.clone());
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let code = code(port);
    assert_eq!(
        blockops_coordination::local_bridge::request_world_port(&code)
            .await
            .unwrap(),
        54321
    );
    for value in ["0", "65536", "12x", "123456", "", "127.0.0.1:25565"] {
        *response.lock().await = (value.to_owned(), None);
        assert!(
            blockops_coordination::local_bridge::request_world_port(&code)
                .await
                .is_err()
        );
    }
    // A port substituted after the companion signed a different one is rejected.
    *response.lock().await = ("54321".to_owned(), Some("12345".to_owned()));
    assert!(
        blockops_coordination::local_bridge::request_world_port(&code)
            .await
            .is_err()
    );
    task.abort();
}

/// Start a hand-written chunked watch response proving `proven` while announcing 43210.
async fn watch_server(
    proven: &'static str,
) -> (
    u16,
    tokio::sync::oneshot::Sender<Vec<u8>>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let (trigger, wait) = tokio::sync::oneshot::channel::<Vec<u8>>();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0; 4096];
        let read = stream.read(&mut request).await.unwrap();
        let proof = mac(
            "response",
            "/v1/watch-world",
            &raw_nonce(&request[..read]),
            proven,
        );
        let head = format!(
            "HTTP/1.1 200 OK\r\nX-BlockOps-Proof: {proof}\r\nTransfer-Encoding: chunked\r\n\r\n8\r\nW 43210\n\r\n"
        );
        let _ = stream.write_all(head.as_bytes()).await;
        if let Ok(ending) = wait.await {
            let _ = stream.write_all(&ending).await;
        }
    });
    (port, trigger, server)
}

#[tokio::test]
async fn world_watch_requires_a_proof_for_the_announced_port() {
    let (port, _trigger, server) = watch_server("12345").await;
    assert!(
        blockops_coordination::local_bridge::watch_world(&code(port))
            .await
            .is_err()
    );
    server.abort();
}

#[tokio::test]
async fn world_watch_reports_closure_and_stream_failure() {
    for ending in [
        b"2\r\nC\n\r\n0\r\n\r\n".as_slice(),
        b"3\r\nBAD\r\n0\r\n\r\n".as_slice(),
        b"".as_slice(),
    ] {
        let (port, trigger, server) = watch_server("43210").await;
        let code = code(port);
        let mut watch = blockops_coordination::local_bridge::watch_world(&code)
            .await
            .unwrap();
        assert_eq!(watch.port, 43210);
        assert!(*watch.alive.borrow());
        trigger.send(ending.to_vec()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while *watch.alive.borrow() {
                watch.alive.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        server.await.unwrap();
    }
}

#[tokio::test]
async fn world_watch_loss_closes_real_host_tunnel_and_active_stream() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let target = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let target_addr = target.local_addr().unwrap();
    let echo = tokio::spawn(async move {
        let (mut socket, _) = target.accept().await.unwrap();
        let (mut read, mut write) = socket.split();
        let _ = tokio::io::copy(&mut read, &mut write).await;
    });
    let watch_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let watch_port = watch_listener.local_addr().unwrap().port();
    let (close_tx, close_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = watch_listener.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let read = socket.read(&mut request).await.unwrap();
        let first = format!("W {}\n", target_addr.port());
        let proof = mac(
            "response",
            "/v1/watch-world",
            &raw_nonce(&request[..read]),
            &target_addr.port().to_string(),
        );
        let headers = format!(
            "HTTP/1.1 200 OK\r\nX-BlockOps-Proof: {proof}\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n",
            first.len(),
            first
        );
        socket.write_all(headers.as_bytes()).await.unwrap();
        let _ = close_rx.await;
        socket.write_all(b"2\r\nC\n\r\n0\r\n\r\n").await.unwrap();
    });
    let watch = blockops_coordination::local_bridge::watch_world(&code(watch_port))
        .await
        .unwrap();
    let host = blockops_tunnel::host(target_addr, None, std::time::Duration::from_secs(60))
        .await
        .unwrap();
    let guest = blockops_tunnel::join(host.invite.clone().unwrap(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    drop(blockops_coordination::local_bridge::cancel_on_world_loss(
        watch.alive.clone(),
        host.cancellation_handle(),
    ));
    let mut stream = tokio::net::TcpStream::connect(guest.endpoint.unwrap())
        .await
        .unwrap();
    stream.write_all(b"ping").await.unwrap();
    let mut reply = [0; 4];
    tokio::time::timeout(
        std::time::Duration::from_secs(8),
        stream.read_exact(&mut reply),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(&reply, b"ping");
    close_tx.send(()).unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(8), stream.read(&mut reply))
        .await
        .unwrap();
    assert!(
        matches!(result, Ok(0) | Err(_)),
        "active stream stayed open after world-watch loss"
    );
    guest.stop().await;
    host.stop().await;
    server.await.unwrap();
    echo.abort();
}
