use anyhow::Result;
use blockops_tunnel::{host, join, Invite};
use clap::{Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf, time::Duration};
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Host {
        #[arg(long)]
        target: SocketAddr,
        #[arg(long)]
        relay: Option<libp2p::Multiaddr>,
        #[arg(long, default_value_t = 3600)]
        ttl: u64,
    },
    Join {
        #[arg(long)]
        invite_file: PathBuf,
        #[arg(long, default_value = "127.0.0.1:25566")]
        bind: SocketAddr,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    let session = match Args::parse().command {
        Command::Host { target, relay, ttl } => {
            host(target, relay, Duration::from_secs(ttl)).await?
        }
        Command::Join { invite_file, bind } => {
            let data = tokio::fs::read(invite_file).await?;
            let invite: Invite = serde_json::from_slice(&data)?;
            join(invite, bind).await?
        }
    };
    if let Some(invite) = &session.invite {
        println!("{}", serde_json::to_string(invite)?);
    }
    if let Some(endpoint) = session.endpoint {
        println!("Local endpoint: {endpoint} (remote is checked on TCP connect)");
    }
    let mut status = session.status.clone();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            result = status.changed() => { if result.is_err() { break; } eprintln!("{}", serde_json::to_string(&*status.borrow())?); }
        }
    }
    session.stop().await;
    Ok(())
}
