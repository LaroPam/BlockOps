use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
};
use tokio::sync::Mutex;

type ReceivedHeaders = Arc<Mutex<Option<(String, String)>>>;

#[tokio::test]
async fn local_client_sends_only_the_selected_loopback_port_and_capability() {
    let seen = Arc::new(Mutex::new(None::<(String, String)>));
    let app = Router::new()
        .route(
            "/v1/join",
            post(
                |State(state): State<ReceivedHeaders>,
                 headers: HeaderMap,
                 body: String| async move {
                    assert!(body.is_empty());
                    *state.lock().await = Some((
                        headers["authorization"].to_str().unwrap().into(),
                        headers["x-blockops-port"].to_str().unwrap().into(),
                    ));
                    StatusCode::ACCEPTED
                },
            ),
        )
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let token = [42u8; 32];
    let public: SocketAddr = "192.0.2.1:25565".parse().unwrap();
    assert!(
        blockops_coordination::local_bridge::request_join(port, &token, public)
            .await
            .is_err()
    );
    assert!(seen.lock().await.is_none());
    blockops_coordination::local_bridge::request_join(
        port,
        &token,
        (Ipv4Addr::LOCALHOST, 32123).into(),
    )
    .await
    .unwrap();
    let received = seen.lock().await.take().unwrap();
    assert_eq!(received.0, format!("Bearer {}", hex::encode(token)));
    assert_eq!(received.1, "32123");
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

#[tokio::test]
async fn world_port_requires_a_valid_bounded_numeric_response() {
    let response = Arc::new(Mutex::new("54321".to_owned()));
    let app = Router::new()
        .route(
            "/v1/world",
            post(
                |State(value): State<Arc<Mutex<String>>>| async move { value.lock().await.clone() },
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
    let code = format!("{port}:{}", "ab".repeat(32));
    assert_eq!(
        blockops_coordination::local_bridge::request_world_port(&code)
            .await
            .unwrap(),
        54321
    );
    for value in ["0", "65536", "12x", "123456", "", "127.0.0.1:25565"] {
        *response.lock().await = value.to_owned();
        assert!(
            blockops_coordination::local_bridge::request_world_port(&code)
                .await
                .is_err()
        );
    }
    task.abort();
}

#[tokio::test]
async fn world_watch_reports_closure_and_stream_failure() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for ending in [
        b"2\r\nC\n\r\n0\r\n\r\n".as_slice(),
        b"3\r\nBAD\r\n0\r\n\r\n".as_slice(),
        b"".as_slice(),
    ] {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let (trigger, wait) = tokio::sync::oneshot::channel::<()>();
        let ending = ending.to_vec();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\nW 43210\n\r\n",
                )
                .await
                .unwrap();
            let _ = wait.await;
            stream.write_all(&ending).await.unwrap();
        });
        let code = format!("{port}:{}", "ab".repeat(32));
        let mut watch = blockops_coordination::local_bridge::watch_world(&code)
            .await
            .unwrap();
        assert_eq!(watch.port, 43210);
        assert!(*watch.alive.borrow());
        trigger.send(()).unwrap();
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
        let _ = socket.read(&mut request).await.unwrap();
        let first = format!("W {}\n", target_addr.port());
        let headers = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n",
            first.len(),
            first
        );
        socket.write_all(headers.as_bytes()).await.unwrap();
        let _ = close_rx.await;
        socket.write_all(b"2\r\nC\n\r\n0\r\n\r\n").await.unwrap();
    });
    let code = format!("{watch_port}:{}", "ab".repeat(32));
    let watch = blockops_coordination::local_bridge::watch_world(&code)
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
