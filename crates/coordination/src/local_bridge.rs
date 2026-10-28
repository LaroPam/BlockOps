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
