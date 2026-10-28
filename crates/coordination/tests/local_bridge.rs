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
