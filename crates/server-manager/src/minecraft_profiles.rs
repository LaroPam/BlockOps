//! Read-only discovery of official Minecraft Launcher profiles and loader metadata.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PROFILES: usize = 512;
const MAX_INHERITANCE: usize = 32;

#[derive(Deserialize)]
struct LauncherProfiles {
    #[serde(default)]
    profiles: BTreeMap<String, Profile>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    name: Option<String>,
    last_version_id: Option<String>,
    game_dir: Option<String>,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionMetadata {
    id: Option<String>,
    inherits_from: Option<String>,
    #[serde(default)]
    libraries: Vec<Library>,
}
#[derive(Deserialize)]
struct Library {
    name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DiscoveredProfile {
    pub name: String,
    pub game_dir: String,
    pub version_id: Option<String>,
    pub minecraft_version: Option<String>,
    /// Unknown means metadata is insufficient; it does not imply Vanilla.
    pub loader: &'static str,
    pub metadata_complete: bool,
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let file = fs::File::open(path)?;
    ensure!(file.metadata()?.is_file(), "Metadata is not a regular file");
    let mut bytes = Vec::new();
    file.take(MAX_METADATA_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_METADATA_BYTES,
        "Metadata file is too large"
    );
    Ok(serde_json::from_slice(&bytes)?)
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
}
fn is_release(id: &str) -> bool {
    let parts: Vec<_> = id.split('.').collect();
    (2..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn inspect_version(root: &Path, id: Option<&str>) -> (&'static str, Option<String>, bool) {
    let mut current = id.map(str::to_owned);
    let mut visited = HashSet::new();
    let mut found = HashSet::new();
    let mut minecraft = None;
    let mut complete = true;
    for _ in 0..MAX_INHERITANCE {
        let Some(version_id) = current.take() else {
            break;
        };
        if !valid_id(&version_id) || !visited.insert(version_id.clone()) {
            complete = false;
            break;
        }
        let path = root
            .join("versions")
            .join(&version_id)
            .join(format!("{version_id}.json"));
        let meta: VersionMetadata = match read_json(&path) {
            Ok(m) => m,
            Err(_) => {
                complete = false;
                break;
            }
        };
        // The directory and metadata must identify the same version, including renamed profiles.
        if meta.id.as_deref() != Some(&version_id) {
            complete = false;
            break;
        }
        if is_release(&version_id) {
            minecraft = Some(version_id.clone());
        }
        for library in meta.libraries {
            let Some(name) = library.name else { continue };
            let mut pieces = name.split(':');
            let (Some(group), Some(artifact)) = (pieces.next(), pieces.next()) else {
                continue;
            };
            match (group, artifact) {
                ("net.fabricmc", "fabric-loader") => {
                    found.insert("fabric");
                }
                ("net.minecraftforge", "forge") => {
                    found.insert("forge");
                }
                ("net.neoforged", "neoforge") => {
                    found.insert("neoforge");
                }
                _ => {}
            }
        }
        current = meta.inherits_from;
    }
    if current.is_some() || visited.is_empty() {
        complete = false;
    }
    let loader = if found.len() > 1 {
        "ambiguous"
    } else if let Some(loader) = found.into_iter().next() {
        loader
    }
    // Only call a complete, recognised release Vanilla; unknown launchers may use custom metadata.
    else if complete && id == minecraft.as_deref() && minecraft.is_some() {
        "vanilla"
    } else {
        "unknown"
    };
    (loader, minecraft, complete)
}

/// Inspect one user-selected Minecraft root. No writes, launches or installation occur.
pub fn discover(root: &Path) -> Result<Vec<DiscoveredProfile>> {
    ensure!(
        root.is_dir(),
        "Minecraft folder does not exist or is not a directory"
    );
    let root = root
        .canonicalize()
        .context("Cannot access the Minecraft folder")?;
    let launcher: LauncherProfiles = read_json(&root.join("launcher_profiles.json"))
        .context("Cannot read launcher_profiles.json from the chosen folder")?;
    ensure!(
        launcher.profiles.len() <= MAX_PROFILES,
        "Too many launcher profiles"
    );
    Ok(launcher
        .profiles
        .into_iter()
        .map(|(id, profile)| {
            let (loader, minecraft_version, metadata_complete) =
                inspect_version(&root, profile.last_version_id.as_deref());
            let game_dir = match profile.game_dir.filter(|s| !s.trim().is_empty()) {
                Some(dir) => {
                    let path = PathBuf::from(dir);
                    if path.is_absolute() {
                        path
                    } else {
                        root.join(path)
                    }
                }
                None => root.clone(),
            };
            DiscoveredProfile {
                name: profile.name.filter(|s| !s.trim().is_empty()).unwrap_or(id),
                game_dir: game_dir.to_string_lossy().into_owned(),
                version_id: profile.last_version_id,
                minecraft_version,
                loader,
                metadata_complete,
            }
        })
        .collect())
}
