//! Run a benign `list` command against an already installed test server and stop it.
use anyhow::{ensure, Context, Result};
use blockops_server_manager::Manager;
use std::{path::PathBuf, time::Duration};
#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let root = PathBuf::from(args.next().context("Missing data directory")?);
    let id = args
        .next()
        .context("Missing server ID")?
        .into_string()
        .map_err(|_| anyhow::anyhow!("Invalid ID"))?;
    let process = Manager::open(root)?.start(&id).await?;
    let mut status = process.status.clone();
    let result = async {
        tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let phase = status.borrow().phase.clone();
                if phase == "running" {
                    break Ok(());
                }
                ensure!(phase != "failed", "Minecraft failed during startup");
                status.changed().await?;
            }
        })
        .await??;
        process.console().send("list").await?;
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if status
                    .borrow()
                    .logs
                    .iter()
                    .any(|line| line.contains("players online"))
                {
                    break Ok(());
                }
                ensure!(
                    status.borrow().phase == "running",
                    "Minecraft exited before command result"
                );
                status.changed().await?;
            }
        })
        .await??;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    process.stop().await;
    result?;
    ensure!(
        status.borrow().phase == "stopped",
        "Minecraft did not stop cleanly"
    );
    println!(
        "PASS: Minecraft console list command returned player list and server stopped cleanly"
    );
    Ok(())
}
