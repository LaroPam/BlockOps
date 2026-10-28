//! Send a one-use join request to an explicitly paired local Minecraft companion.
//! Pairing and connection to a real client are not implemented here.
use anyhow::{ensure, Result};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

/// Acknowledgement means only that the companion accepted the request, not that Minecraft connected.
pub async fn request_join(bridge_port: u16, token: &[u8; 32], endpoint: SocketAddr) -> Result<()> {
    ensure!(bridge_port != 0, "Invalid companion port");
    ensure!(
        endpoint.ip() == IpAddr::V4(Ipv4Addr::LOCALHOST) && endpoint.port() != 0,
        "Minecraft endpoint must be IPv4 loopback"
    );
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()?;
    let response = client
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

/// Parse a short-lived pairing code without including its secret in errors.
pub async fn request_paired_join(code: &str, endpoint: SocketAddr) -> Result<()> {
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
    let bytes = hex::decode(token_text).map_err(|_| anyhow::anyhow!("Invalid pairing code"))?;
    let token: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid pairing code"))?;
    request_join(port, &token, endpoint).await
}
