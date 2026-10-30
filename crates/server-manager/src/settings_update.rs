//! Crash-recoverable edits to managed properties and SQLite metadata.
use crate::{lock, properties_match, Manager, Server, Settings};
use anyhow::{ensure, Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io::Write, path::Path};

const JOURNAL: &str = "settings.pending.json";
const MAX_JOURNAL_BYTES: usize = 4 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    old: Server,
    new: Server,
    old_properties: String,
    new_properties: String,
}

fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    std::fs::File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
/// NamedTempFile::persist performs same-filesystem atomic replacement on Windows and Unix.
fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().context("Invalid destination")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(contents)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    sync_dir(parent)?;
    Ok(())
}
fn merge_properties(actual: &str, settings: &Settings, version: &str) -> Result<String> {
    let expected = settings.properties(version)?;
    let managed: HashMap<_, _> = expected.lines().filter_map(|l| l.split_once('=')).collect();
    let mut result = String::with_capacity(actual.len());
    for line in actual.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('#') && !trimmed.starts_with('!') {
            if let Some((key, _)) = trimmed.split_once('=') {
                if let Some(value) = managed.get(key.trim()) {
                    result.push_str(key.trim());
                    result.push('=');
                    result.push_str(value);
                    result.push('\n');
                    continue;
                }
            }
        }
        result.push_str(line);
        result.push('\n');
    }
    // New managed keys may not exist on servers created by older versions.
    for (key, value) in &managed {
        if !result.lines().any(|line| {
            line.trim()
                .split_once('=')
                .is_some_and(|(k, _)| k.trim() == *key)
        }) {
            result.push_str(&format!("{key}={value}\n"));
        }
    }
    ensure!(
        properties_match(&result, &expected)?,
        "Unable to update managed properties safely"
    );
    Ok(result)
}
impl Manager {
    /// Change settings only when the server is stopped. Unknown Minecraft properties are preserved.
    /// A durable journal allows a later start/update to recover interrupted transactions.
    pub fn update_settings(&self, id: &str, settings: Settings) -> Result<Server> {
        settings.validate()?;
        // Validate ID and existence before using it in a filesystem path.
        self.get(id)?;
        let dir = self.root.join("servers").join(id);
        let _lock = lock(&dir.join("process.lock"))?;
        ensure!(
            !crate::backups::marker_present(&dir)?,
            "Interrupted world restore; recover it before changing settings"
        );
        self.recover_settings_locked(id)?;
        let old = self.get(id)?;
        let old_properties = std::fs::read_to_string(dir.join("server.properties"))?;
        ensure!(
            properties_match(&old_properties, &old.properties()?)?,
            "Managed properties changed outside BlockOps; restore safe settings before editing"
        );
        let new_properties = merge_properties(&old_properties, &settings, &old.version)?;
        let mut new = old.clone();
        new.settings = settings;
        let journal = Journal {
            old,
            new,
            old_properties,
            new_properties,
        };
        let journal_path = dir.join(JOURNAL);
        let encoded = serde_json::to_vec(&journal)?;
        ensure!(
            encoded.len() <= MAX_JOURNAL_BYTES,
            "Server properties are too large to update safely"
        );
        atomic_write(&journal_path, &encoded)?;
        let result = (|| -> Result<()> {
            atomic_write(
                &dir.join("server.properties"),
                journal.new_properties.as_bytes(),
            )?;
            let changed = self.db()?.execute(
                "UPDATE servers SET metadata = ?1 WHERE id = ?2",
                params![serde_json::to_string(&journal.new)?, id],
            )?;
            ensure!(changed == 1, "Server disappeared while updating settings");
            Ok(())
        })();
        if let Err(e) = result {
            return match self.recover_settings_locked(id) {
                Ok(()) => Err(e.context("Settings update failed; previous values recovered")),
                Err(recovery) => Err(e.context(format!(
                    "Settings update failed; recovery is pending: {recovery:#}"
                ))),
            };
        }
        // If removal fails, the next operation will safely complete this committed update.
        std::fs::remove_file(journal_path)
            .context("Settings committed, but journal cleanup is pending")?;
        sync_dir(&dir)?;
        Ok(journal.new)
    }

    /// The caller must hold this server's process.lock.
    pub(crate) fn recover_settings_locked(&self, id: &str) -> Result<()> {
        let dir = self.root.join("servers").join(id);
        let journal_path = dir.join(JOURNAL);
        match std::fs::symlink_metadata(&journal_path) {
            Ok(metadata) => {
                ensure!(
                    metadata.file_type().is_file() && metadata.len() <= MAX_JOURNAL_BYTES as u64,
                    "Settings recovery journal must be a regular file no larger than 4 MiB"
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        let data = std::fs::read(&journal_path)?;
        ensure!(
            data.len() <= MAX_JOURNAL_BYTES,
            "Settings recovery journal is too large"
        );
        let journal: Journal =
            serde_json::from_slice(&data).context("Settings recovery journal is invalid")?;
        // Do not use a corrupt journal to alter authentication, target or another server.
        ensure!(
            journal.old.id == id
                && journal.new.id == id
                && journal.old.version == journal.new.version
                && journal.old.jar_sha1 == journal.new.jar_sha1
                && journal.old.eula_accepted_at == journal.new.eula_accepted_at,
            "Settings recovery journal does not match the server"
        );
        journal.old.settings.validate()?;
        journal.new.settings.validate()?;
        ensure!(
            properties_match(&journal.old_properties, &journal.old.properties()?)?
                && properties_match(&journal.new_properties, &journal.new.properties()?)?,
            "Unsafe settings recovery journal"
        );
        let current = self.get(id)?;
        let current_json = serde_json::to_value(current)?;
        let (chosen, expected) = if current_json == serde_json::to_value(&journal.old)? {
            (&journal.old_properties, journal.old.properties()?)
        } else if current_json == serde_json::to_value(&journal.new)? {
            (&journal.new_properties, journal.new.properties()?)
        } else {
            anyhow::bail!("Settings recovery journal conflicts with stored metadata");
        };
        let props_path = dir.join("server.properties");
        let actual = std::fs::read_to_string(&props_path).unwrap_or_default();
        if !properties_match(&actual, &expected).unwrap_or(false) || actual != *chosen {
            atomic_write(&props_path, chosen.as_bytes())?;
        }
        std::fs::remove_file(journal_path)?;
        sync_dir(&dir)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Difficulty, Server};
    use fs2::FileExt;
    fn settings() -> Settings {
        Settings {
            name: "Test".into(),
            memory_mb: 2048,
            port: 25565,
            max_players: 10,
            difficulty: Difficulty::Normal,
            whitelist: false,
            game_mode: None,
            pvp: None,
        }
    }
    fn fixture() -> (tempfile::TempDir, Manager, Server) {
        let root = tempfile::tempdir().unwrap();
        let manager = Manager::open(root.path()).unwrap();
        let server = Server {
            id: uuid::Uuid::new_v4().to_string(),
            settings: settings(),
            version: crate::artifacts::VERSION.into(),
            jar_sha1: "00".repeat(20),
            eula_accepted_at: 1,
        };
        let dir = root.path().join("servers").join(&server.id);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("server.properties"),
            format!(
                "# Generated by Minecraft\nview-distance=17\n{}",
                server.properties().unwrap()
            ),
        )
        .unwrap();
        manager
            .db()
            .unwrap()
            .execute(
                "INSERT INTO servers(id,metadata) VALUES (?1,?2)",
                params![server.id, serde_json::to_string(&server).unwrap()],
            )
            .unwrap();
        (root, manager, server)
    }
    fn changed() -> Settings {
        Settings {
            name: "Updated".into(),
            memory_mb: 4096,
            port: 25566,
            max_players: 15,
            difficulty: Difficulty::Hard,
            whitelist: true,
            game_mode: Some(crate::GameMode::Creative),
            pvp: Some(false),
        }
    }
    #[test]
    fn updates_properties_and_database_and_preserves_other_properties() {
        let (root, manager, server) = fixture();
        let updated = manager.update_settings(&server.id, changed()).unwrap();
        assert_eq!(updated.settings.name, "Updated");
        assert_eq!(manager.get(&server.id).unwrap().settings.memory_mb, 4096);
        assert!(manager.get(&server.id).unwrap().settings.whitelist);
        let properties = std::fs::read_to_string(
            root.path()
                .join("servers")
                .join(&server.id)
                .join("server.properties"),
        )
        .unwrap();
        assert!(properties.contains("view-distance=17\n"));
        assert!(properties.contains("online-mode=true\n"));
        assert!(properties.contains("server-ip=127.0.0.1\n"));
        assert!(properties.contains("difficulty=hard\n"));
        assert!(properties.contains("white-list=true\n"));
        assert!(properties.contains("gamemode=creative\n"));
        assert!(properties.contains("pvp=false\n"));
        assert!(properties_match(&properties, &updated.properties().unwrap()).unwrap());
    }
    #[test]
    fn legacy_unmanaged_values_are_preserved_until_explicitly_changed() {
        let (root, manager, server) = fixture();
        let dir = root.path().join("servers").join(&server.id);
        let mut original = std::fs::read_to_string(dir.join("server.properties")).unwrap();
        original.push_str("gamemode=adventure\npvp=false\n");
        std::fs::write(dir.join("server.properties"), &original).unwrap();
        let mut update = changed();
        update.game_mode = None;
        update.pvp = None;
        manager.update_settings(&server.id, update).unwrap();
        let saved = std::fs::read_to_string(dir.join("server.properties")).unwrap();
        assert!(saved.contains("gamemode=adventure\n"));
        assert!(saved.contains("pvp=false\n"));
        manager.update_settings(&server.id, changed()).unwrap();
        let saved = std::fs::read_to_string(dir.join("server.properties")).unwrap();
        assert!(saved.contains("gamemode=creative\n"));
        assert!(saved.contains("pvp=false\n"));
    }
    #[test]
    fn running_lock_and_unsafe_properties_prevent_edit() {
        let (root, manager, server) = fixture();
        let dir = root.path().join("servers").join(&server.id);
        let lockfile = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("process.lock"))
            .unwrap();
        lockfile.try_lock_exclusive().unwrap();
        assert!(manager.update_settings(&server.id, changed()).is_err());
        drop(lockfile);
        let properties = std::fs::read_to_string(dir.join("server.properties"))
            .unwrap()
            .replace("online-mode=true", "online-mode=false");
        std::fs::write(dir.join("server.properties"), properties).unwrap();
        assert!(manager.update_settings(&server.id, changed()).is_err());
        assert_eq!(manager.get(&server.id).unwrap().settings.name, "Test");
    }
    #[test]
    fn journal_recovery_follows_database_commit() {
        for committed in [false, true] {
            let (root, manager, old) = fixture();
            let dir = root.path().join("servers").join(&old.id);
            let old_properties = std::fs::read_to_string(dir.join("server.properties")).unwrap();
            let mut new = old.clone();
            new.settings = changed();
            let new_properties =
                merge_properties(&old_properties, &new.settings, &new.version).unwrap();
            let journal = Journal {
                old: old.clone(),
                new: new.clone(),
                old_properties: old_properties.clone(),
                new_properties: new_properties.clone(),
            };
            atomic_write(&dir.join(JOURNAL), &serde_json::to_vec(&journal).unwrap()).unwrap();
            // Simulate interrupted update: file replacement completed, DB may or may not have committed.
            std::fs::write(dir.join("server.properties"), &new_properties).unwrap();
            if committed {
                manager
                    .db()
                    .unwrap()
                    .execute(
                        "UPDATE servers SET metadata=?1 WHERE id=?2",
                        params![serde_json::to_string(&new).unwrap(), old.id],
                    )
                    .unwrap();
            }
            let _lock = lock(&dir.join("process.lock")).unwrap();
            manager.recover_settings_locked(&old.id).unwrap();
            assert_eq!(
                std::fs::read_to_string(dir.join("server.properties")).unwrap(),
                if committed {
                    new_properties
                } else {
                    old_properties
                }
            );
            assert_eq!(
                manager.get(&old.id).unwrap().settings.name,
                if committed { "Updated" } else { "Test" }
            );
            assert!(!dir.join(JOURNAL).exists());
        }
    }
    #[test]
    fn database_failure_rolls_back_file_and_removes_journal() {
        let (root, manager, server) = fixture();
        let dir = root.path().join("servers").join(&server.id);
        let original = std::fs::read(dir.join("server.properties")).unwrap();
        manager.db().unwrap().execute_batch("CREATE TRIGGER fail_settings BEFORE UPDATE ON servers BEGIN SELECT RAISE(ABORT, 'injected failure'); END;").unwrap();
        assert!(manager.update_settings(&server.id, changed()).is_err());
        assert_eq!(
            std::fs::read(dir.join("server.properties")).unwrap(),
            original
        );
        assert_eq!(manager.get(&server.id).unwrap().settings.name, "Test");
        assert!(!dir.join(JOURNAL).exists());
    }
    #[cfg(unix)]
    #[test]
    fn dangling_journal_symlink_is_rejected() {
        let (root, manager, server) = fixture();
        let dir = root.path().join("servers").join(&server.id);
        std::os::unix::fs::symlink("does-not-exist", dir.join(JOURNAL)).unwrap();
        assert!(manager.update_settings(&server.id, changed()).is_err());
        assert_eq!(manager.get(&server.id).unwrap().settings.name, "Test");
    }
    #[test]
    fn oversize_journal_is_rejected_before_modification() {
        let (root, manager, server) = fixture();
        let dir = root.path().join("servers").join(&server.id);
        let mut properties = std::fs::read_to_string(dir.join("server.properties")).unwrap();
        properties.push_str("description=");
        properties.push_str(&"x".repeat(2 * 1024 * 1024 + 1024));
        properties.push('\n');
        std::fs::write(dir.join("server.properties"), &properties).unwrap();
        assert!(manager.update_settings(&server.id, changed()).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join("server.properties")).unwrap(),
            properties
        );
        assert_eq!(manager.get(&server.id).unwrap().settings.name, "Test");
        assert!(!dir.join(JOURNAL).exists());
    }
    #[test]
    fn corrupt_journal_is_rejected_without_modification() {
        let (root, manager, server) = fixture();
        let dir = root.path().join("servers").join(&server.id);
        let original = std::fs::read(dir.join("server.properties")).unwrap();
        std::fs::write(dir.join(JOURNAL), b"{bad").unwrap();
        assert!(manager.update_settings(&server.id, changed()).is_err());
        assert_eq!(
            std::fs::read(dir.join("server.properties")).unwrap(),
            original
        );
        assert!(dir.join(JOURNAL).exists());
    }
}
