//! Test-only client: capability is supplied on stdin, never via arguments or output.
use anyhow::{ensure, Result};
use std::{
    io::{self, BufRead, Read},
    net::{Ipv4Addr, SocketAddr},
};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let bridge_port: u16 = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing bridge port"))?
        .parse()?;
    let target_port: u16 = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing target port"))?
        .parse()?;
    ensure!(args.next().is_none(), "Unexpected argument");
    let mut secret = String::new();
    io::stdin().lock().take(66).read_line(&mut secret)?;
    let bytes = hex::decode(secret.trim())?;
    let token: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid capability"))?;
    blockops_coordination::local_bridge::request_join(
        bridge_port,
        &token,
        SocketAddr::from((Ipv4Addr::LOCALHOST, target_port)),
    )
    .await
}
