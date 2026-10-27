use anyhow::{ensure, Result};
use blockops_coordination::{
    client::Client,
    service::{serve, Store},
};
use clap::{Parser, Subcommand};
use std::{net::SocketAddr, time::Duration};
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Bind loopback; put an HTTPS reverse proxy in front for public deployment.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8080")]
        bind: SocketAddr,
    },
    Host {
        #[arg(long)]
        coordinator: String,
        #[arg(long)]
        target: SocketAddr,
        #[arg(long)]
        relay: Option<libp2p::Multiaddr>,
    },
    Join {
        #[arg(long)]
        coordinator: String,
        #[arg(long)]
        code_file: std::path::PathBuf,
        #[arg(long, default_value = "127.0.0.1:25566")]
        bind: SocketAddr,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    match Args::parse().command {
        Action::Serve { bind } => {
            ensure!(
                bind.ip().is_loopback(),
                "Bind loopback and expose via an HTTPS reverse proxy"
            );
            let listener = tokio::net::TcpListener::bind(bind).await?;
            println!("Listening {}", listener.local_addr()?);
            tokio::select! {r=serve(listener,Store::default())=>r?,_=tokio::signal::ctrl_c()=>{}}
        }
        Action::Host {
            coordinator,
            target,
            relay,
        } => {
            let host =
                blockops_tunnel::host_controlled(target, relay, Duration::from_secs(3600)).await?;
            let invite = Client::new(&coordinator)?.publish(&host, 600).await?;
            println!("{}", invite.status.borrow().code);
            tokio::signal::ctrl_c().await?;
            invite.revoke().await;
            host.stop().await;
        }
        Action::Join {
            coordinator,
            code_file,
            bind,
        } => {
            let guest = Client::new(&coordinator)?
                .join(std::fs::read_to_string(code_file)?.trim(), bind)
                .await?;
            println!("Local endpoint: {}", guest.endpoint.unwrap());
            tokio::signal::ctrl_c().await?;
            guest.stop().await;
        }
    }
    Ok(())
}
