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
