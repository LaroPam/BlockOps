//! Requests to an explicitly paired local Minecraft companion.
use anyhow::{ensure, Result};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

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
    let response = local_client()?
        .post(format!("http://127.0.0.1:{bridge_port}/v1/join"))
        .bearer_auth(hex::encode(token))
        .header("X-BlockOps-Port", endpoint.port().to_string())
        .header(reqwest::header::CONTENT_LENGTH, "0")
        .body("")
        .send()
        .await?;
    ensure!(
        response.status() == reqwest::StatusCode::ACCEPTED,
        "Local companion rejected the join request (HTTP {})",
        response.status()
    );
    Ok(())
}

pub async fn request_paired_join(code: &str, endpoint: SocketAddr) -> Result<()> {
    let (port, token) = pairing(code)?;
    request_join(port, &token, endpoint).await
}

/// Obtain a snapshot from the paired mod; it is not proof that the world stays open.
pub async fn request_world_port(code: &str) -> Result<u16> {
    let (bridge_port, token) = pairing(code)?;
    let mut response = local_client()?
        .post(format!("http://127.0.0.1:{bridge_port}/v1/world"))
        .bearer_auth(hex::encode(token))
        .header(reqwest::header::CONTENT_LENGTH, "0")
        .body("")
        .send()
        .await?;
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
    let port: u16 = std::str::from_utf8(&body)?.parse()?;
    ensure!(port != 0, "Invalid world port response");
    Ok(port)
}

/// A live monitor, closed when the world changes or the channel fails.
pub struct WorldWatch {
    pub port: u16,
    pub alive: tokio::sync::watch::Receiver<bool>,
    cancel: tokio_util::sync::CancellationToken,
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
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(12 * 60 * 60 + 10))
        .build()?;
    let mut response = tokio::time::timeout(
        Duration::from_secs(8),
        client
            .post(format!("http://127.0.0.1:{bridge_port}/v1/watch-world"))
            .bearer_auth(hex::encode(token))
            .header(reqwest::header::CONTENT_LENGTH, "0")
            .body("")
            .send(),
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
