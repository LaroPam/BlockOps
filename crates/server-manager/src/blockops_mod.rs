//! Install, update and remove the BlockOps mod in a launcher profile's game folder.
//!
//! The installer carries one jar per loader and Minecraft version, each naming its target in
//! `blockops.json`. Only jars identified as BlockOps (by that file, or by `fabric.mod.json`
//! for jars built before it existed) are ever replaced or removed; nothing else in the game
//! folder is written.
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
/// Loaders a bundled jar may target.
const LOADERS: &[&str] = &["fabric", "forge", "neoforge"];
const MAX_BUNDLED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_MOD_FILES: usize = 4096;

/// The loader and Minecraft version a bundled jar is built for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub loader: String,
    pub minecraft: String,
}

#[derive(Deserialize)]
struct Manifest {
    id: String,
    version: String,
    loader: Option<String>,
    minecraft: Option<String>,
}

/// `blockops.json`, or for jars built before it existed `fabric.mod.json`; `None` when the
/// jar is not readable or carries neither.
fn manifest(reader: impl Read + Seek) -> Option<Manifest> {
    let mut archive = zip::ZipArchive::new(reader).ok()?;
    for name in ["blockops.json", "fabric.mod.json"] {
        let Ok(entry) = archive.by_name(name) else {
            continue;
        };
        let mut bytes = Vec::new();
        entry
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return None;
        }
        return serde_json::from_slice(&bytes).ok();
    }
    None
}

fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
}

/// A mod jar shipped with this BlockOps build.
pub struct BundledMod {
    bytes: Vec<u8>,
    pub version: String,
    pub target: Target,
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
            manifest(io::Cursor::new(&bytes)).context("Bundled file is not a BlockOps mod")?;
        let (Some(loader), Some(minecraft)) = (manifest.loader, manifest.minecraft) else {
            anyhow::bail!("Bundled mod does not name its loader and Minecraft version");
        };
        ensure!(
            manifest.id == MOD_ID
                && valid_version(&manifest.version)
                && valid_version(&minecraft)
                && LOADERS.contains(&loader.as_str()),
            "Bundled file is not the BlockOps mod"
        );
        Ok(Self {
            sha256: Sha256::digest(&bytes).into(),
            bytes,
            version: manifest.version,
            target: Target { loader, minecraft },
        })
    }

    fn file_name(&self) -> String {
        format!(
            "blockops-{}-{}-{}.jar",
            self.target.loader, self.target.minecraft, self.version
        )
    }
}

/// Every mod jar shipped with this build, one per target.
#[derive(Default)]
pub struct Catalog(Vec<BundledMod>);

impl Catalog {
    /// The BlockOps jars in a folder; other files are ignored.
    pub fn load(folder: &Path) -> Self {
        let Ok(entries) = fs::read_dir(folder) else {
            return Self::default();
        };
        let mut mods: Vec<BundledMod> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "jar"))
            .filter_map(|path| BundledMod::load(&path).ok())
            .collect();
        mods.sort_by(|a, b| {
            (&a.target.loader, &a.target.minecraft).cmp(&(&b.target.loader, &b.target.minecraft))
        });
        mods.dedup_by(|a, b| a.target == b.target);
        Self(mods)
    }

    pub fn get(&self, target: &Target) -> Option<&BundledMod> {
        self.0.iter().find(|bundled| &bundled.target == target)
    }

    pub fn targets(&self) -> Vec<Target> {
        self.0
            .iter()
            .map(|bundled| bundled.target.clone())
            .collect()
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
    /// No launcher profile of a loader and version this build carries a mod for uses the folder.
    Unsupported,
    /// Another launcher profile runs from this folder and the mod would stop it starting.
    SharedWith(String),
    /// A mod file could not be replaced, usually because Minecraft is running.
    InUse,
    /// The mod's file name is taken by a file not verified as the BlockOps mod.
    NameTaken(String),
    Other(anyhow::Error),
}
impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => write!(f, "No supported loader and version uses this folder"),
            Self::SharedWith(name) => write!(f, "Profile {name:?} also uses this folder"),
            Self::InUse => write!(f, "A mod file is in use"),
            Self::NameTaken(name) => write!(f, "{name} is not the BlockOps mod"),
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

/// The build a profile runs, when its metadata is complete.
pub fn target_of(profile: &DiscoveredProfile) -> Option<Target> {
    profile.metadata_complete.then_some(())?;
    LOADERS.contains(&profile.loader).then_some(())?;
    Some(Target {
        loader: profile.loader.into(),
        minecraft: profile.minecraft_version.clone()?,
    })
}

fn runs_safely(profile: &DiscoveredProfile, target: &Target) -> bool {
    // The official launcher's built-in profiles name no version folder but run Vanilla,
    // which ignores the mods folder.
    let latest = matches!(
        profile.version_id.as_deref(),
        Some("latest-release" | "latest-snapshot")
    );
    target_of(profile).as_ref() == Some(target) || profile.loader == "vanilla" || latest
}

/// Choose the bundled jar for the folder's build. Every other launcher profile that runs from
/// the folder must be the same build or Vanilla: the mod's Minecraft dependency would stop
/// another version from starting, and other loaders or unrecognised builds cannot be vouched
/// for.
fn check_profiles<'a>(
    launcher_root: &Path,
    game_dir: &Path,
    catalog: &'a Catalog,
) -> Result<&'a BundledMod, InstallError> {
    let profiles = minecraft_profiles::discover(launcher_root)?;
    let sharing: Vec<_> = profiles
        .iter()
        .filter(|profile| {
            Path::new(&profile.game_dir).canonicalize().ok().as_deref() == Some(game_dir)
        })
        .collect();
    let bundled = sharing
        .iter()
        .find_map(|profile| target_of(profile).and_then(|target| catalog.get(&target)))
        .ok_or(InstallError::Unsupported)?;
    if let Some(other) = sharing
        .iter()
        .find(|profile| !runs_safely(profile, &bundled.target))
    {
        return Err(InstallError::SharedWith(other.name.clone()));
    }
    Ok(bundled)
}

/// Re-inspect the launcher profiles, then place the matching bundled mod as the only
/// BlockOps jar.
pub fn install(
    launcher_root: &Path,
    game_dir: &Path,
    catalog: &Catalog,
) -> Result<ModStatus, InstallError> {
    let game_dir = game_folder(game_dir)?;
    let bundled = check_profiles(launcher_root, &game_dir, catalog)?;
    let mods = game_dir.join("mods");
    fs::create_dir_all(&mods)?;
    let found = installed(&mods)?;
    if !(found.len() == 1 && found[0].sha256 == bundled.sha256) {
        let target = mods.join(bundled.file_name());
        // Only a verified BlockOps jar may be replaced under this name; checked before any
        // older copy is removed, so a refusal leaves the folder as it was.
        if fs::symlink_metadata(&target).is_ok() {
            let existing = target.canonicalize().ok();
            if existing.is_none()
                || !found
                    .iter()
                    .any(|old| old.path.canonicalize().ok() == existing)
            {
                return Err(InstallError::NameTaken(bundled.file_name()));
            }
        }
        // A hidden name without the .jar extension: no loader ever loads a partial file.
        let mut staged = tempfile::Builder::new()
            .prefix(".blockops-")
            .suffix(".tmp")
            .tempfile_in(&mods)?;
        staged.write_all(&bundled.bytes)?;
        staged.as_file().sync_all()?;
        // Remove older copies first: two jars with one mod id would stop the loader.
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
