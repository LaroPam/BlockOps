//! Install, update and remove the BlockOps Fabric mod in a launcher profile's game folder.
//! Only jars whose `fabric.mod.json` declares the `blockops` id are ever replaced or
//! removed; nothing else in the game folder is written.
use crate::minecraft_profiles::{self, DiscoveredProfile};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt, fs,
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
};

pub const MOD_ID: &str = "blockops";
/// The mod declares `minecraft = 1.21.1`; Fabric Loader refuses to start any other version.
pub const MINECRAFT_VERSION: &str = "1.21.1";
const MAX_BUNDLED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_MOD_FILES: usize = 4096;

#[derive(Deserialize)]
struct FabricManifest {
    id: String,
    version: String,
}

/// The `fabric.mod.json` of a jar, or `None` when it is not a readable Fabric mod.
fn manifest(reader: impl Read + Seek) -> Option<FabricManifest> {
    let mut archive = zip::ZipArchive::new(reader).ok()?;
    let entry = archive.by_name("fabric.mod.json").ok()?;
    let mut bytes = Vec::new();
    entry
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
}

/// The mod shipped with this BlockOps build.
pub struct BundledMod {
    bytes: Vec<u8>,
    pub version: String,
    sha256: [u8; 32],
}

impl BundledMod {
    pub fn load(path: &Path) -> Result<Self> {
        let file = fs::File::open(path).context("Cannot open the bundled mod")?;
        ensure!(file.metadata()?.is_file(), "Bundled mod is not a file");
        let mut bytes = Vec::new();
        file.take(MAX_BUNDLED_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_BUNDLED_BYTES,
            "Bundled mod is too large"
        );
        let manifest =
            manifest(io::Cursor::new(&bytes)).context("Bundled file is not a Fabric mod")?;
        ensure!(
            manifest.id == MOD_ID && valid_version(&manifest.version),
            "Bundled file is not the BlockOps mod"
        );
        Ok(Self {
            sha256: Sha256::digest(&bytes).into(),
            bytes,
            version: manifest.version,
        })
    }

    fn file_name(&self) -> String {
        format!("blockops-fabric-client-{}.jar", self.version)
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ModStatus {
    /// Version shipped with this BlockOps build; `None` when the build carries no mod.
    pub bundled: Option<String>,
    /// Versions of the BlockOps mod present in the folder's `mods` directory.
    pub installed: Vec<String>,
    /// Exactly one BlockOps mod is present and it is the bundled file.
    pub current: bool,
}

#[derive(Debug)]
pub enum InstallError {
    /// No Fabric 1.21.1 launcher profile runs from this folder.
    NotFabric,
    /// Another launcher profile runs from this folder and the mod would stop it starting.
    SharedWith(String),
    /// A mod file could not be replaced, usually because Minecraft is running.
    InUse,
    Other(anyhow::Error),
}
impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFabric => write!(f, "No Fabric {MINECRAFT_VERSION} profile uses this folder"),
            Self::SharedWith(name) => write!(f, "Profile {name:?} also uses this folder"),
            Self::InUse => write!(f, "A mod file is in use"),
            Self::Other(error) => write!(f, "{error}"),
        }
    }
}
impl<E: Into<anyhow::Error>> From<E> for InstallError {
    fn from(error: E) -> Self {
        Self::Other(error.into())
    }
}

fn in_use(error: io::Error) -> InstallError {
    // Windows reports a file held open by the game as a sharing violation or denial.
    if error.kind() == io::ErrorKind::PermissionDenied
        || matches!(error.raw_os_error(), Some(32 | 33))
    {
        InstallError::InUse
    } else {
        InstallError::Other(error.into())
    }
}

fn game_folder(game_dir: &Path) -> Result<PathBuf> {
    ensure!(
        game_dir.is_absolute() && game_dir.is_dir(),
        "Choose an existing absolute Minecraft game folder"
    );
    Ok(game_dir.canonicalize()?)
}

struct Installed {
    path: PathBuf,
    version: String,
    sha256: [u8; 32],
}

/// BlockOps jars directly in `mods`, identified by their manifest rather than file name.
fn installed(mods: &Path) -> Result<Vec<Installed>> {
    let entries = match fs::read_dir(mods) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut found = Vec::new();
    for entry in entries.take(MAX_MOD_FILES) {
        let entry = entry?;
        let path = entry.path();
        // Symbolic links and folders are never followed, replaced or removed.
        if !entry.file_type()?.is_file()
            || !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
        {
            continue;
        }
        let Ok(file) = fs::File::open(&path) else {
            continue;
        };
        let Some(manifest) = manifest(io::BufReader::new(file)) else {
            continue;
        };
        if manifest.id != MOD_ID {
            continue;
        }
        let mut hasher = Sha256::new();
        io::copy(&mut fs::File::open(&path)?, &mut hasher)?;
        found.push(Installed {
            path,
            version: manifest.version,
            sha256: hasher.finalize().into(),
        });
    }
    Ok(found)
}

pub fn status(game_dir: &Path, bundled: Option<&BundledMod>) -> Result<ModStatus> {
    let game_dir = game_folder(game_dir)?;
    let found = installed(&game_dir.join("mods"))?;
    Ok(ModStatus {
        bundled: bundled.map(|bundled| bundled.version.clone()),
        current: bundled
            .is_some_and(|bundled| found.len() == 1 && found[0].sha256 == bundled.sha256),
        installed: found.into_iter().map(|mod_file| mod_file.version).collect(),
    })
}

fn runs_safely(profile: &DiscoveredProfile) -> bool {
    let fabric = profile.loader == "fabric"
        && profile.metadata_complete
        && profile.minecraft_version.as_deref() == Some(MINECRAFT_VERSION);
    // The official launcher's built-in profiles name no version folder but run Vanilla,
    // which ignores the mods folder.
    let latest = matches!(
        profile.version_id.as_deref(),
        Some("latest-release" | "latest-snapshot")
    );
    fabric || profile.loader == "vanilla" || latest
}

/// Every launcher profile that runs from this folder must be Fabric 1.21.1 or Vanilla: the
/// mod's Minecraft dependency would stop any other Fabric profile from starting, and other
/// loaders or unrecognised versions cannot be vouched for.
fn check_profiles(launcher_root: &Path, game_dir: &Path) -> Result<(), InstallError> {
    let profiles = minecraft_profiles::discover(launcher_root)?;
    let sharing: Vec<_> = profiles
        .iter()
        .filter(|profile| {
            Path::new(&profile.game_dir).canonicalize().ok().as_deref() == Some(game_dir)
        })
        .collect();
    if !sharing
        .iter()
        .any(|profile| profile.loader == "fabric" && runs_safely(profile))
    {
        return Err(InstallError::NotFabric);
    }
    if let Some(other) = sharing.iter().find(|profile| !runs_safely(profile)) {
        return Err(InstallError::SharedWith(other.name.clone()));
    }
    Ok(())
}

/// Re-inspect the launcher profiles, then place the bundled mod as the only BlockOps jar.
pub fn install(
    launcher_root: &Path,
    game_dir: &Path,
    bundled: &BundledMod,
) -> Result<ModStatus, InstallError> {
    let game_dir = game_folder(game_dir)?;
    check_profiles(launcher_root, &game_dir)?;
    let mods = game_dir.join("mods");
    fs::create_dir_all(&mods)?;
    let found = installed(&mods)?;
    if !(found.len() == 1 && found[0].sha256 == bundled.sha256) {
        // A hidden name without the .jar extension: Fabric Loader never loads a partial file.
        let mut staged = tempfile::Builder::new()
            .prefix(".blockops-")
            .suffix(".tmp")
            .tempfile_in(&mods)?;
        staged.write_all(&bundled.bytes)?;
        staged.as_file().sync_all()?;
        let target = mods.join(bundled.file_name());
        // Remove older copies first: two jars with one mod id would stop Fabric Loader.
        for old in found.iter().filter(|old| old.path != target) {
            fs::remove_file(&old.path).map_err(in_use)?;
        }
        staged
            .persist(&target)
            .map_err(|error| in_use(error.error))?;
    }
    Ok(status(&game_dir, Some(bundled))?)
}

/// Remove every BlockOps jar from the folder's `mods` directory, and nothing else.
pub fn remove(game_dir: &Path, bundled: Option<&BundledMod>) -> Result<ModStatus, InstallError> {
    let game_dir = game_folder(game_dir)?;
    for old in installed(&game_dir.join("mods"))? {
        fs::remove_file(&old.path).map_err(in_use)?;
    }
    Ok(status(&game_dir, bundled)?)
}
