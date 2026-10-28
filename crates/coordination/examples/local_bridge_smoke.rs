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
    let mode = args.next().unwrap_or_else(|| "join".into());
    ensure!(args.next().is_none(), "Unexpected argument");
    let mut secret = String::new();
    io::stdin().lock().take(66).read_line(&mut secret)?;
    let code = format!("{bridge_port}:{}", secret.trim());
    match mode.as_str() {
        "join" => {
            blockops_coordination::local_bridge::request_paired_join(
                &code,
                SocketAddr::from((Ipv4Addr::LOCALHOST, target_port)),
            )
            .await
        }
        "world" => {
            let port = blockops_coordination::local_bridge::request_world_port(&code).await?;
            ensure!(port == target_port, "Unexpected world port");
            Ok(())
        }
        _ => anyhow::bail!("Invalid test mode"),
    }
}
