pub mod artifacts;
pub mod backups;
pub mod java;
pub mod minecraft_profiles;
pub mod minecraft_selection;
pub mod process;
mod settings_update;
pub mod stop_states;
pub mod whitelist;
use anyhow::{ensure, Context, Result};
use fs2::FileExt;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub const EULA_URL: &str = "https://www.minecraft.net/eula";
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Difficulty {
    Peaceful,
    Easy,
    Normal,
    Hard,
}
impl Difficulty {
    fn value(&self) -> &'static str {
        match self {
            Self::Peaceful => "peaceful",
            Self::Easy => "easy",
            Self::Normal => "normal",
            Self::Hard => "hard",
        }
    }
}
/// None means the property was not managed by older BlockOps versions.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GameMode {
    Survival,
    Creative,
    Adventure,
    Spectator,
}
impl GameMode {
    fn value(&self) -> &'static str {
        match self {
            Self::Survival => "survival",
            Self::Creative => "creative",
            Self::Adventure => "adventure",
            Self::Spectator => "spectator",
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub name: String,
    pub memory_mb: u32,
    pub port: u16,
    pub max_players: u8,
    pub difficulty: Difficulty,
    #[serde(default)]
    pub whitelist: bool,
    #[serde(default)]
    pub game_mode: Option<GameMode>,
    #[serde(default)]
    pub pvp: Option<bool>,
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.trim().is_empty()
                && self.name.len() <= 100
                && !self.name.chars().any(char::is_control),
            "Server name must be 1..100 bytes with no control characters"
        );
        ensure!(
            (1024..=16384).contains(&self.memory_mb),
            "RAM must be between 1024 and 16384 MiB"
        );
        ensure!(
            self.port >= 1024,
            "Choose a TCP port between 1024 and 65535"
        );
        ensure!(
            (1..=100).contains(&self.max_players),
            "Player limit must be 1..100"
        );
        Ok(())
    }
    pub fn properties(&self) -> Result<String> {
        self.validate()?;
        let mut properties = format!("server-ip=127.0.0.1\nserver-port={}\nonline-mode=true\nenforce-secure-profile=true\nenable-rcon=false\nenable-query=false\nwhite-list={}\nlevel-name=world\nmotd=BlockOps Vanilla\ndifficulty={}\nmax-players={}\n",self.port,self.whitelist,self.difficulty.value(),self.max_players);
        if let Some(mode) = &self.game_mode {
            properties.push_str(&format!("gamemode={}\n", mode.value()));
        }
        if let Some(pvp) = self.pvp {
            properties.push_str(&format!("pvp={pvp}\n"));
        }
        Ok(properties)
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Server {
    pub id: String,
    pub settings: Settings,
    pub version: String,
    pub jar_sha1: String,
    pub eula_accepted_at: u64,
}
#[derive(Clone)]
pub struct Manager {
    root: PathBuf,
}
fn lock(path: &Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.try_lock_exclusive()
        .context("Another operation or server process already owns this resource")?;
    Ok(file)
}
impl Manager {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        std::fs::create_dir_all(root.as_ref())?;
        let manager = Self {
            root: root.as_ref().canonicalize()?,
        };
        std::fs::create_dir_all(manager.root.join("servers"))?;
        std::fs::create_dir_all(manager.root.join("runtimes"))?;
        manager.db()?.execute_batch(
            "CREATE TABLE IF NOT EXISTS servers(id TEXT PRIMARY KEY, metadata TEXT NOT NULL);",
        )?;
        Ok(manager)
    }
    fn db(&self) -> Result<Connection> {
        let db = Connection::open(self.root.join("blockops.sqlite3"))?;
        db.busy_timeout(Duration::from_secs(5))?;
        Ok(db)
    }
    pub fn list(&self) -> Result<Vec<Server>> {
        let db = self.db()?;
        let mut statement = db.prepare("SELECT metadata FROM servers ORDER BY id")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn get(&self, id: &str) -> Result<Server> {
        let parsed = Uuid::parse_str(id).context("Invalid server ID")?;
        ensure!(parsed.to_string() == id, "Invalid server ID");
        let text: String = self
            .db()?
            .query_row("SELECT metadata FROM servers WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .context("Server not found")?;
        let server: Server = serde_json::from_str(&text)?;
        ensure!(
            server.id == id && artifacts::supported_version(&server.version),
            "Unsupported server metadata"
        );
        server.settings.validate()?;
        Ok(server)
    }
    /// Acceptance is never inferred; UI/CLI must obtain an explicit decision before calling.
    pub async fn install(&self, settings: Settings, accept_eula: bool) -> Result<Server> {
        self.install_version(settings, accept_eula, artifacts::VERSION)
            .await
    }
    pub async fn install_version(
        &self,
        settings: Settings,
        accept_eula: bool,
        version: &str,
    ) -> Result<Server> {
        ensure!(
            accept_eula,
            "Explicit acceptance of the Minecraft EULA is required before installation"
        );
        settings.validate()?;
        ensure!(
            artifacts::supported_version(version),
            "Unsupported Minecraft version"
        );
        let _lock = lock(&self.root.join("install.lock"))?;
        let artifact = artifacts::vanilla_version(version)
            .await
            .context("Cannot resolve official Minecraft metadata")?;
        artifacts::java21(&self.root.join("runtimes"))
            .await
            .context("Cannot install verified Java 21")?;
        let staged = tempfile::tempdir_in(self.root.join("servers"))?;
        artifacts::download_server(&artifact, &staged.path().join("server.jar")).await?;
        let server = Server {
            id: Uuid::new_v4().to_string(),
            settings,
            version: version.into(),
            jar_sha1: artifact.sha1,
            eula_accepted_at: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        };
        std::fs::write(
            staged.path().join("server.properties"),
            server.settings.properties()?,
        )?;
        std::fs::write(
            staged.path().join("eula.txt"),
            format!(
                "# Explicit acceptance recorded at {} for {}\n# {}\neula=true\n",
                server.eula_accepted_at, server.version, EULA_URL
            ),
        )?;
        let dest = self.root.join("servers").join(&server.id);
        let db = self.db()?;
        let metadata = serde_json::to_string(&server)?;
        std::fs::rename(staged.path(), &dest)?;
        if let Err(e) = db.execute(
            "INSERT INTO servers(id,metadata) VALUES (?1,?2)",
            params![server.id, metadata],
        ) {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(e.into());
        }
        Ok(server)
    }
    pub async fn start(&self, id: &str) -> Result<process::Process> {
        let server = self.get(id)?;
        ensure!(
            server.eula_accepted_at > 0,
            "Minecraft EULA has not been accepted"
        );
        let dir = self.root.join("servers").join(id);
        let lock = lock(&dir.join("process.lock"))?;
        ensure!(
            !crate::backups::marker_present(&dir)?,
            "Interrupted world restore; recover it before starting"
        );
        self.recover_settings_locked(id)?;
        // Re-read under the process lock: settings may have changed since the first lookup.
        let server = self.get(id)?;
        let eula = std::fs::read_to_string(dir.join("eula.txt"))?;
        ensure!(
            eula.lines().any(|l| l.trim() == "eula=true")
                && !eula.lines().any(|l| l.trim() == "eula=false"),
            "Minecraft EULA acceptance was revoked"
        );
        artifacts::verify_file(&dir.join("server.jar"), &server.jar_sha1)?;
        // Never silently launch on LAN or with online authentication disabled after a manual edit.
        ensure!(
            properties_match(
                &std::fs::read_to_string(dir.join("server.properties"))?,
                &server.settings.properties()?
            )?,
            "Managed properties changed; restore the saved safe settings before starting"
        );
        let java = self.root.join("runtimes/java21").join(if cfg!(windows) {
            "bin/java.exe"
        } else {
            "bin/java"
        });
        artifacts::check_java(&java).await?;
        let probe =
            tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, server.settings.port))
                .await
                .context("Server port is already occupied")?;
        drop(probe); // Java must bind the actual port; subsequent conflicts surface in its logs.
        let mut command = tokio::process::Command::new(java);
        crate::java::configure(&mut command)?;
        command.current_dir(dir).args([
            "-Xms512M",
            &format!("-Xmx{}M", server.settings.memory_mb),
            "-jar",
            "server.jar",
            "nogui",
        ]);
        process::spawn(command, lock, Duration::from_secs(30))
    }
}

/// Minecraft rewrites properties with additional defaults. Compare semantic managed keys,
/// rejecting ambiguous escapes/duplicates instead of relying on file order.
pub fn properties_match(actual: &str, expected: &str) -> Result<bool> {
    let mut values = std::collections::HashMap::new();
    for line in actual.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .context("Unsupported properties syntax")?;
        ensure!(
            !key.contains('\\')
                && !key.contains(':')
                && !key.chars().any(char::is_whitespace)
                && value.chars().rev().take_while(|c| *c == '\\').count() % 2 == 0,
            "Ambiguous properties escape"
        );
        ensure!(
            values.insert(key.trim(), value.trim()).is_none(),
            "Duplicate property"
        );
    }
    for line in expected.lines() {
        let (key, value) = line.split_once('=').context("Invalid managed property")?;
        if values.get(key).copied() != Some(value) {
            return Ok(false);
        }
    }
    Ok(true)
}
