//! Requests to an explicitly paired local Minecraft companion.
//!
//! The capability never crosses the socket: each request carries a fresh nonce and an
//! HMAC-SHA256 over its path, nonce and port, and every success response must carry an
//! HMAC over the path, nonce and payload. A different process that later binds the same
//! loopback port therefore neither learns the capability nor passes as the companion.
use anyhow::{ensure, Result};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

type HmacSha256 = Hmac<Sha256>;

fn mac(token: &[u8; 32], direction: &str, path: &str, nonce: &str, value: &str) -> HmacSha256 {
    let mut mac = HmacSha256::new_from_slice(token).expect("HMAC accepts any key length");
    mac.update(format!("blockops-bridge-{direction}-v1\n{path}\n{nonce}\n{value}").as_bytes());
    mac
}

/// Send one authenticated request and return the response with the nonce it must prove.
async fn send(
    client: &reqwest::Client,
    bridge_port: u16,
    token: &[u8; 32],
    path: &str,
    port: Option<u16>,
) -> Result<(reqwest::Response, String)> {
    ensure!(bridge_port != 0, "Invalid companion port");
    let mut nonce = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut nonce);
    let nonce = hex::encode(nonce);
    let port = port.map(|port| port.to_string()).unwrap_or_default();
    let auth = hex::encode(
        mac(token, "request", path, &nonce, &port)
            .finalize()
            .into_bytes(),
    );
    let mut request = client
        .post(format!("http://127.0.0.1:{bridge_port}{path}"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("BlockOps-HMAC {auth}"),
        )
        .header("X-BlockOps-Nonce", &nonce)
        .header(reqwest::header::CONTENT_LENGTH, "0")
        .body("");
    if !port.is_empty() {
        request = request.header("X-BlockOps-Port", port);
    }
    Ok((request.send().await?, nonce))
}

/// Accept a success response only if it proves knowledge of the capability.
fn verify(
    response: &reqwest::Response,
    token: &[u8; 32],
    path: &str,
    nonce: &str,
    payload: &str,
) -> Result<()> {
    let proof = response
        .headers()
        .get("x-blockops-proof")
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() == 64)
        .and_then(|value| hex::decode(value).ok())
        .ok_or_else(|| anyhow::anyhow!("Local companion did not prove its identity"))?;
    mac(token, "response", path, nonce, payload)
        .verify_slice(&proof)
        .map_err(|_| anyhow::anyhow!("Local companion did not prove its identity"))
}

fn local_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()?)
}
fn pairing(code: &str) -> Result<(u16, [u8; 32])> {
    let (port_text, token_text) = code
        .trim()
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("Invalid pairing code"))?;
    ensure!(
        !port_text.is_empty()
            && port_text.len() <= 5
            && port_text.bytes().all(|b| b.is_ascii_digit()),
        "Invalid pairing code"
    );
    ensure!(
        token_text.len() == 64 && token_text.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid pairing code"
    );
    let port: u16 = port_text
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid pairing code"))?;
    ensure!(port != 0, "Invalid pairing code");
    let bytes = hex::decode(token_text).map_err(|_| anyhow::anyhow!("Invalid pairing code"))?;
    let token: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid pairing code"))?;
    Ok((port, token))
}

/// Acknowledgement means only that the companion accepted the request, not that Minecraft connected.
pub async fn request_join(bridge_port: u16, token: &[u8; 32], endpoint: SocketAddr) -> Result<()> {
    ensure!(bridge_port != 0, "Invalid companion port");
    ensure!(
        endpoint.ip() == IpAddr::V4(Ipv4Addr::LOCALHOST) && endpoint.port() != 0,
        "Minecraft endpoint must be IPv4 loopback"
    );
    let (response, nonce) = send(
        &local_client()?,
        bridge_port,
        token,
        "/v1/join",
        Some(endpoint.port()),
    )
    .await?;
    ensure!(
        response.status() == reqwest::StatusCode::ACCEPTED,
        "Local companion rejected the join request (HTTP {})",
        response.status()
    );
    verify(&response, token, "/v1/join", &nonce, "")
}

pub async fn request_paired_join(code: &str, endpoint: SocketAddr) -> Result<()> {
    let (port, token) = pairing(code)?;
    request_join(port, &token, endpoint).await
}

/// Obtain a snapshot from the paired mod; it is not proof that the world stays open.
pub async fn request_world_port(code: &str) -> Result<u16> {
    let (bridge_port, token) = pairing(code)?;
    let (mut response, nonce) =
        send(&local_client()?, bridge_port, &token, "/v1/world", None).await?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "Local companion did not report an open world"
    );
    if let Some(length) = response.content_length() {
        ensure!(length <= 5, "Invalid world port response");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(body.len() + chunk.len() <= 5, "Invalid world port response");
        body.extend_from_slice(&chunk);
    }
    ensure!(
        !body.is_empty() && body.iter().all(u8::is_ascii_digit),
        "Invalid world port response"
    );
    let text = std::str::from_utf8(&body)?;
    verify(&response, &token, "/v1/world", &nonce, text)?;
    let port: u16 = text.parse()?;
    ensure!(port != 0, "Invalid world port response");
    Ok(port)
}

/// A live monitor, closed when the world changes or the channel fails.
pub struct WorldWatch {
    pub port: u16,
    pub alive: tokio::sync::watch::Receiver<bool>,
    cancel: tokio_util::sync::CancellationToken,
}
impl WorldWatch {
    pub(crate) fn new(
        port: u16,
        alive: tokio::sync::watch::Receiver<bool>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            port,
            alive,
            cancel,
        }
    }
}
impl Drop for WorldWatch {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn read_watch_line(response: &mut reqwest::Response, buffer: &mut Vec<u8>) -> Result<String> {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(index) = buffer.iter().position(|byte| *byte == b'\n') {
                let line: Vec<_> = buffer.drain(..=index).collect();
                return Ok(std::str::from_utf8(&line[..index])?.to_owned());
            }
            let chunk = response
                .chunk()
                .await?
                .ok_or_else(|| anyhow::anyhow!("World watch ended"))?;
            ensure!(
                buffer.len() + chunk.len() <= 128,
                "Invalid world watch message"
            );
            buffer.extend_from_slice(&chunk);
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("World watch timed out"))?
}

/// Keep a one-use authenticated stream open and fail closed if the mod stops reporting.
pub async fn watch_world(code: &str) -> Result<WorldWatch> {
    let (bridge_port, token) = pairing(code)?;
    watch_world_at(bridge_port, &token).await
}

/// As [`watch_world`], for a companion found through private local discovery.
pub async fn watch_world_at(bridge_port: u16, token: &[u8; 32]) -> Result<WorldWatch> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(12 * 60 * 60 + 10))
        .build()?;
    let (mut response, nonce) = tokio::time::timeout(
        Duration::from_secs(8),
        send(&client, bridge_port, token, "/v1/watch-world", None),
    )
    .await??;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "Local companion did not report an open world"
    );
    let mut buffer = Vec::new();
    let first = read_watch_line(&mut response, &mut buffer).await?;
    let port_text = first
        .strip_prefix("W ")
        .ok_or_else(|| anyhow::anyhow!("Invalid world watch message"))?;
    ensure!(
        !port_text.is_empty()
            && port_text.len() <= 5
            && port_text.bytes().all(|b| b.is_ascii_digit()),
        "Invalid world watch port"
    );
    verify(&response, token, "/v1/watch-world", &nonce, port_text)?;
    let port: u16 = port_text.parse()?;
    ensure!(port != 0, "Invalid world watch port");
    let (tx, alive) = tokio::sync::watch::channel(true);
    let cancel = tokio_util::sync::CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        loop {
            let message = tokio::select! {
                _ = stop.cancelled() => break,
                message = read_watch_line(&mut response, &mut buffer) => message,
            };
            match message {
                Ok(line) if line == "A" => {}
                _ => break,
            }
        }
        tx.send_replace(false);
    });
    Ok(WorldWatch {
        port,
        alive,
        cancel,
    })
}

/// Cancel a host tunnel as soon as its authenticated world watch ends.
pub fn cancel_on_world_loss(
    mut alive: tokio::sync::watch::Receiver<bool>,
    cancel: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if !*alive.borrow() {
                cancel.cancel();
                break;
            }
            tokio::select! {
                _ = cancel.cancelled() => break,
                changed = alive.changed() => if changed.is_err() { cancel.cancel(); break; },
            }
        }
    })
}
