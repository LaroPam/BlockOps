//! A user-confirmed preference, not proof of compatibility or permission to install.
use crate::Manager;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::Path,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportedLoader {
    Fabric,
    Forge,
    Neoforge,
    None,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MinecraftSelection {
    pub game_dir: String,
    pub loader: ReportedLoader,
}

impl Manager {
    /// The selection is only a preference. Re-inspect and obtain consent before writing mods.
    pub fn save_minecraft_selection(
        &self,
        selection: MinecraftSelection,
    ) -> Result<MinecraftSelection> {
        ensure!(
            selection.game_dir.len() <= 4096 && !selection.game_dir.chars().any(char::is_control),
            "Invalid Minecraft folder"
        );
        let path = Path::new(&selection.game_dir);
        ensure!(
            path.is_absolute() && path.is_dir(),
            "Choose an existing absolute Minecraft game folder"
        );
        let data = serde_json::to_vec(&selection)?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
        temporary.write_all(&data)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.root.join("minecraft-selection.json"))
            .map_err(|e| e.error)?;
        Ok(selection)
    }

    pub fn minecraft_selection(&self) -> Result<Option<MinecraftSelection>> {
        let path = self.root.join("minecraft-selection.json");
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut data = Vec::new();
        file.take(16 * 1024 + 1).read_to_end(&mut data)?;
        ensure!(data.len() <= 16 * 1024, "Minecraft selection is too large");
        Ok(Some(
            serde_json::from_slice(&data).context("Saved Minecraft selection is invalid")?,
        ))
    }
}
