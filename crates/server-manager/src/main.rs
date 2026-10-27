use anyhow::Result;
use blockops_server_manager::{Difficulty, Manager, Settings};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
#[derive(Parser)]
struct Args {
    #[arg(long)]
    data_dir: PathBuf,
    #[command(subcommand)]
    command: Action,
}
#[derive(Clone, ValueEnum)]
enum DifficultyArg {
    Peaceful,
    Easy,
    Normal,
    Hard,
}
impl From<DifficultyArg> for Difficulty {
    fn from(value: DifficultyArg) -> Self {
        match value {
            DifficultyArg::Peaceful => Difficulty::Peaceful,
            DifficultyArg::Easy => Difficulty::Easy,
            DifficultyArg::Normal => Difficulty::Normal,
            DifficultyArg::Hard => Difficulty::Hard,
        }
    }
}
#[derive(Subcommand)]
enum Action {
    List,
    /// Downloads a supported Vanilla release and Java 21. Acceptance must be explicit.
    Install {
        #[arg(long)]
        name: String,
        #[arg(long)]
        accept_eula: bool,
        #[arg(long, default_value = "1.21.1")]
        version: String,
        #[arg(long, default_value_t = 2048)]
        memory_mb: u32,
        #[arg(long, default_value_t = 25565)]
        port: u16,
    },
    Run {
        id: String,
    },
    /// Update the settings of a stopped server, preserving other properties.
    UpdateSettings {
        id: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        memory_mb: u32,
        #[arg(long)]
        port: u16,
        #[arg(long)]
        max_players: u8,
        #[arg(long, value_enum)]
        difficulty: DifficultyArg,
    },
    /// Create a backup of a stopped server's world.
    Backup {
        id: String,
    },
    /// List backups of a server.
    ListBackups {
        id: String,
    },
    /// Restore a backup; the previous world is retained on disk.
    Restore {
        id: String,
        backup_id: String,
    },
    /// Recover an interrupted restore operation.
    RecoverRestore {
        id: String,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let manager = Manager::open(args.data_dir)?;
    match args.command {
        Action::List => println!("{}", serde_json::to_string_pretty(&manager.list()?)?),
        Action::Install {
            name,
            accept_eula,
            version,
            memory_mb,
            port,
        } => {
            let server = manager
                .install_version(
                    Settings {
                        name,
                        memory_mb,
                        port,
                        max_players: 10,
                        difficulty: Difficulty::Normal,
                    },
                    accept_eula,
                    &version,
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&server)?);
        }
        Action::UpdateSettings {
            id,
            name,
            memory_mb,
            port,
            max_players,
            difficulty,
        } => {
            let settings = Settings {
                name,
                memory_mb,
                port,
                max_players,
                difficulty: difficulty.into(),
            };
            let server =
                tokio::task::spawn_blocking(move || manager.update_settings(&id, settings))
                    .await??;
            println!("{}", serde_json::to_string_pretty(&server)?);
        }
        Action::Backup { id } => {
            let backup = tokio::task::spawn_blocking(move || manager.create_backup(&id)).await??;
            println!("{}", serde_json::to_string_pretty(&backup)?);
        }
        Action::ListBackups { id } => {
            let backups = tokio::task::spawn_blocking(move || manager.list_backups(&id)).await??;
            println!("{}", serde_json::to_string_pretty(&backups)?);
        }
        Action::Restore { id, backup_id } => {
            tokio::task::spawn_blocking(move || manager.restore_backup(&id, &backup_id)).await??;
            println!("World restored; the previous world was retained.");
        }
        Action::RecoverRestore { id } => {
            tokio::task::spawn_blocking(move || manager.recover_restore(&id)).await??;
            println!("Restore recovery completed.");
        }
        Action::Run { id } => {
            let process = manager.start(&id).await?;
            let mut status = process.status.clone();
            loop {
                tokio::select! {
                    _=tokio::signal::ctrl_c()=>break,
                    result=status.changed()=> { if result.is_err(){break;} let s=status.borrow(); println!("{}",serde_json::to_string(&*s)?); if s.phase=="failed" || s.phase=="stopped" {break;} }
                }
            }
            process.stop().await;
        }
    }
    Ok(())
}
