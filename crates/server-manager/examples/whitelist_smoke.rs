//! Verify whitelist configuration and vanilla's list response on an isolated test server.
use anyhow::{ensure, Context, Result};
use blockops_server_manager::{
    whitelist::{self, Action},
    Manager,
};
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
    let manager = Manager::open(root)?;
    let mut settings = manager.get(&id)?.settings;
    settings.whitelist = true;
    manager.update_settings(&id, settings)?;
    let process = manager.start(&id).await?;
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
        process
            .console()
            .send(&whitelist::command(Action::List, None)?)
            .await?;
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if status
                    .borrow()
                    .logs
                    .iter()
                    .any(|line| line.to_ascii_lowercase().contains("whitelisted players"))
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
        for (action, expected) in [
            (Action::Add, "Added BlockOpsTest to the whitelist"),
            (Action::Remove, "Removed BlockOpsTest from the whitelist"),
        ] {
            process
                .console()
                .send(&whitelist::command(action, Some("BlockOpsTest"))?)
                .await?;
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    if status
                        .borrow()
                        .logs
                        .iter()
                        .any(|line| line.contains(expected))
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
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    process.stop().await;
    result?;
    ensure!(
        status.borrow().phase == "stopped",
        "Minecraft did not stop cleanly"
    );
    println!("PASS: whitelist enabled, list/add/remove responses verified, and Minecraft stopped cleanly");
    Ok(())
}
