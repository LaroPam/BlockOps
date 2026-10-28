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
