//! Offline backups of the managed `world` directory. Archives are never extracted over
//! the live world. Checksums detect accidental corruption, not a maliciously replaced archive.
use crate::{lock, Manager};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

const MAX_ENTRIES: usize = 50000;
const MAX_BYTES: u64 = 20 * 1024 * 1024 * 1024;
const MAX_MANIFEST: u64 = 8 * 1024 * 1024;
const MARKER: &str = "restore.pending.json";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
    path: String,
    size: u64,
    sha256: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u8,
    id: String,
    server_id: String,
    minecraft_version: String,
    created_at: u64,
    dirs: Vec<String>,
    files: Vec<FileEntry>,
}
#[derive(Clone, Serialize)]
pub struct BackupInfo {
    pub id: String,
    pub created_at: u64,
    pub size_bytes: u64,
    pub minecraft_version: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    id: String,
}

fn uuid(value: &str) -> Result<()> {
    ensure!(
        Uuid::parse_str(value)?.to_string() == value,
        "Invalid backup ID"
    );
    Ok(())
}
fn validate_path(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 1024
            && !value.contains('\\')
            && !value.contains(':')
            && !value.chars().any(char::is_control),
        "Unsafe backup path"
    );
    for component in Path::new(value).components() {
        let Component::Normal(s) = component else {
            bail!("Unsafe backup path");
        };
        let s = s.to_str().context("Non-Unicode backup path")?;
        ensure!(!s.ends_with(['.', ' ']), "Non-portable backup path");
        let base = s.split('.').next().unwrap_or("").to_ascii_uppercase();
        ensure!(
            !matches!(
                base.as_str(),
                "CON"
                    | "PRN"
                    | "AUX"
                    | "NUL"
                    | "COM1"
                    | "COM2"
                    | "COM3"
                    | "COM4"
                    | "COM5"
                    | "COM6"
                    | "COM7"
                    | "COM8"
                    | "COM9"
                    | "LPT1"
                    | "LPT2"
                    | "LPT3"
                    | "LPT4"
                    | "LPT5"
                    | "LPT6"
                    | "LPT7"
                    | "LPT8"
                    | "LPT9"
            ),
            "Non-portable backup path"
        );
    }
    Ok(())
}
fn regular(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    !meta.file_type().is_symlink() && (meta.is_dir() || meta.is_file())
}
pub(crate) fn marker_present(dir: &Path) -> Result<bool> {
    match fs::symlink_metadata(dir.join(MARKER)) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn checked_directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        regular(&meta) && meta.is_dir(),
        "Backup directory must be a regular directory"
    );
    Ok(())
}
fn create_checked_directory(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    checked_directory(path)
}
fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}
fn read_manifest(
    archive: &mut ZipArchive<File>,
    server_id: &str,
    backup_id: &str,
) -> Result<Manifest> {
    ensure!(
        archive.len() <= MAX_ENTRIES + 1,
        "Backup has too many entries"
    );
    let mut file = archive
        .by_name("manifest.json")
        .context("Backup manifest missing")?;
    ensure!(file.size() <= MAX_MANIFEST, "Backup manifest is too large");
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_MANIFEST,
        "Backup manifest is too large"
    );
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    ensure!(
        manifest.format == 1 && manifest.server_id == server_id && manifest.id == backup_id,
        "Backup belongs to another server or has invalid metadata"
    );
    ensure!(
        manifest.files.len() + manifest.dirs.len() <= MAX_ENTRIES,
        "Backup has too many entries"
    );
    Ok(manifest)
}
fn backup_info(id: &str, manifest: &Manifest, size: u64) -> BackupInfo {
    BackupInfo {
        id: id.into(),
        created_at: manifest.created_at,
        size_bytes: size,
        minecraft_version: manifest.minecraft_version.clone(),
    }
}
fn entries(world: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut pending = vec![PathBuf::new()];
    let mut names = HashSet::new();
    let mut total = 0u64;
    while let Some(relative) = pending.pop() {
        for item in fs::read_dir(world.join(&relative))? {
            let item = item?;
            let path = relative.join(item.file_name());
            let text = path
                .to_str()
                .context("Non-Unicode world path")?
                .replace(std::path::MAIN_SEPARATOR, "/");
            validate_path(&text)?;
            ensure!(
                names.insert(text.to_ascii_lowercase()),
                "World has paths that collide on Windows"
            );
            let metadata = fs::symlink_metadata(item.path())?;
            ensure!(
                regular(&metadata),
                "World contains a symbolic link or special file"
            );
            if metadata.is_dir() {
                dirs.push(text);
                pending.push(path);
            } else {
                total = total
                    .checked_add(metadata.len())
                    .context("World size overflow")?;
                ensure!(total <= MAX_BYTES, "World exceeds backup size limit");
                files.push(text);
            }
            ensure!(
                dirs.len() + files.len() <= MAX_ENTRIES,
                "World has too many entries"
            );
        }
    }
    dirs.sort();
    files.sort();
    ensure!(
        files.iter().any(|p| p == "level.dat"),
        "World does not contain level.dat"
    );
    Ok((dirs, files))
}
fn check_manifest(manifest: &Manifest) -> Result<()> {
    ensure!(
        crate::artifacts::supported_version(&manifest.minecraft_version),
        "Backup Minecraft version is unsupported"
    );
    let mut seen = HashSet::new();
    let mut dirs = BTreeSet::new();
    let mut sum = 0u64;
    for path in &manifest.dirs {
        validate_path(path)?;
        ensure!(
            seen.insert(path.to_ascii_lowercase()),
            "Duplicate backup path"
        );
        dirs.insert(path.as_str());
    }
    for file in &manifest.files {
        validate_path(&file.path)?;
        ensure!(
            seen.insert(file.path.to_ascii_lowercase()),
            "Duplicate backup path"
        );
        ensure!(
            file.sha256.len() == 64 && file.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid file checksum"
        );
        sum = sum.checked_add(file.size).context("Backup size overflow")?;
        ensure!(sum <= MAX_BYTES, "Backup exceeds size limit");
        for parent in Path::new(&file.path)
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            ensure!(
                dirs.contains(parent.to_str().context("Invalid backup path")?),
                "Backup directory is missing"
            );
        }
    }
    for dir in &manifest.dirs {
        for parent in Path::new(dir)
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            ensure!(
                dirs.contains(parent.to_str().context("Invalid backup path")?),
                "Backup directory is missing"
            );
        }
    }
    ensure!(
        manifest.files.iter().any(|f| f.path == "level.dat"),
        "Backup lacks level.dat"
    );
    Ok(())
}
fn extract(archive: &mut ZipArchive<File>, manifest: &Manifest, staging: &Path) -> Result<()> {
    check_manifest(manifest)?;
    let dirs: HashSet<String> = manifest
        .dirs
        .iter()
        .map(|p| format!("world/{p}/"))
        .collect();
    let files: HashMap<String, &FileEntry> = manifest
        .files
        .iter()
        .map(|f| (format!("world/{}", f.path), f))
        .collect();
    ensure!(
        archive.len() == dirs.len() + files.len() + 1,
        "Unexpected backup entry count"
    );
    let mut seen = HashSet::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_owned();
        ensure!(seen.insert(name.clone()), "Duplicate archive entry");
        if name == "manifest.json" {
            continue;
        }
        ensure!(
            entry.unix_mode().unwrap_or(0) & 0o170000 != 0o120000,
            "Archive contains a symlink"
        );
        ensure!(entry.enclosed_name().is_some(), "Unsafe archive entry");
        if dirs.contains(&name) {
            ensure!(entry.is_dir(), "Invalid directory entry");
            continue;
        }
        let expected = files.get(&name).context("Unexpected archive entry")?;
        ensure!(
            !entry.is_dir() && entry.size() == expected.size,
            "Backup file size mismatch"
        );
        let path = staging.join(&expected.path);
        fs::create_dir_all(path.parent().context("Invalid file path")?)?;
        let mut out = OpenOptions::new().write(true).create_new(true).open(path)?;
        let mut hash = Sha256::new();
        let mut count = 0u64;
        let mut buf = [0u8; 65536];
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            count = count.checked_add(n as u64).context("Size overflow")?;
            ensure!(
                count <= expected.size,
                "Backup file exceeded its declared size"
            );
            hash.update(&buf[..n]);
            out.write_all(&buf[..n])?;
        }
        ensure!(
            count == expected.size
                && hex::encode(hash.finalize()) == expected.sha256.to_ascii_lowercase(),
            "Backup file checksum mismatch"
        );
        out.sync_all()?;
    }
    for dir in &manifest.dirs {
        fs::create_dir_all(staging.join(dir))?;
    }
    for dir in manifest.dirs.iter().rev() {
        sync_dir(&staging.join(dir))?;
    }
    sync_dir(staging)?;
    Ok(())
}

impl Manager {
    fn server_dir(&self, id: &str) -> Result<PathBuf> {
        self.get(id)?;
        let parent = self.root.join("servers");
        checked_directory(&parent)?;
        let path = parent.join(id);
        checked_directory(&path)?;
        Ok(path)
    }
    fn backup_dir(&self, id: &str) -> Result<PathBuf> {
        self.get(id)?;
        let parent = self.root.join("backups");
        create_checked_directory(&parent)?;
        let path = parent.join(id);
        create_checked_directory(&path)?;
        Ok(path)
    }
    pub fn create_backup(&self, server_id: &str) -> Result<BackupInfo> {
        let server = self.get(server_id)?;
        let dir = self.server_dir(server_id)?;
        let _lock = lock(&dir.join("process.lock"))?;
        ensure!(
            !marker_present(&dir)?,
            "Interrupted restore; recover it before creating a backup"
        );
        let world = dir.join("world");
        let meta =
            fs::symlink_metadata(&world).context("World does not exist; start the server first")?;
        ensure!(
            regular(&meta) && meta.is_dir(),
            "World must be a regular directory"
        );
        let (dirs, files) = entries(&world)?;
        let backup_dir = self.backup_dir(server_id)?;
        let id = Uuid::new_v4().to_string();
        let created_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let mut temp = tempfile::NamedTempFile::new_in(&backup_dir)?;
        let mut manifest = Manifest {
            format: 1,
            id: id.clone(),
            server_id: server_id.into(),
            minecraft_version: server.version,
            created_at,
            dirs: dirs.clone(),
            files: Vec::new(),
        };
        {
            let mut zip = ZipWriter::new(temp.as_file_mut());
            let options =
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            for path in dirs {
                zip.add_directory(format!("world/{path}/"), options)?;
            }
            let mut total = 0u64;
            for path in files {
                let source = world.join(&path);
                let meta = fs::symlink_metadata(&source)?;
                ensure!(
                    regular(&meta) && meta.is_file(),
                    "World changed during backup"
                );
                zip.start_file(format!("world/{path}"), options)?;
                let mut input = File::open(source)?;
                let mut hash = Sha256::new();
                let mut size = 0u64;
                let mut buf = [0u8; 65536];
                loop {
                    let n = input.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    size = size.checked_add(n as u64).context("Size overflow")?;
                    total = total.checked_add(n as u64).context("Size overflow")?;
                    ensure!(total <= MAX_BYTES, "World exceeds backup size limit");
                    hash.update(&buf[..n]);
                    zip.write_all(&buf[..n])?;
                }
                ensure!(size == meta.len(), "World changed during backup");
                manifest.files.push(FileEntry {
                    path,
                    size,
                    sha256: hex::encode(hash.finalize()),
                });
            }
            let json = serde_json::to_vec(&manifest)?;
            ensure!(
                json.len() as u64 <= MAX_MANIFEST,
                "Backup manifest is too large"
            );
            zip.start_file("manifest.json", options)?;
            zip.write_all(&json)?;
            zip.finish()?;
        }
        temp.as_file_mut().sync_all()?;
        let target = backup_dir.join(format!("{id}.zip"));
        temp.persist_noclobber(&target).map_err(|e| e.error)?;
        sync_dir(&backup_dir)?;
        Ok(backup_info(&id, &manifest, fs::metadata(target)?.len()))
    }
    pub fn list_backups(&self, server_id: &str) -> Result<Vec<BackupInfo>> {
        let dir = self.backup_dir(server_id)?;
        let mut result = Vec::new();
        for item in fs::read_dir(dir)? {
            let item = item?;
            let path = item.path();
            if path.extension().and_then(|s| s.to_str()) != Some("zip") {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .context("Invalid backup filename")?;
            uuid(id)?;
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                regular(&metadata) && metadata.is_file(),
                "Backup must be a regular file"
            );
            let mut archive = ZipArchive::new(File::open(&path)?)?;
            let manifest = read_manifest(&mut archive, server_id, id)?;
            check_manifest(&manifest)?;
            result.push(backup_info(id, &manifest, metadata.len()));
        }
        result.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
        Ok(result)
    }
    pub fn restore_backup(&self, server_id: &str, backup_id: &str) -> Result<()> {
        uuid(backup_id)?;
        let dir = self.server_dir(server_id)?;
        let _lock = lock(&dir.join("process.lock"))?;
        ensure!(
            !marker_present(&dir)?,
            "Interrupted restore; recover it before restoring"
        );
        let world = dir.join("world");
        let metadata = fs::symlink_metadata(&world).context("Current world does not exist")?;
        ensure!(
            regular(&metadata) && metadata.is_dir(),
            "World must be a regular directory"
        );
        let path = self.backup_dir(server_id)?.join(format!("{backup_id}.zip"));
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            regular(&metadata) && metadata.is_file(),
            "Backup must be a regular file"
        );
        let mut archive = ZipArchive::new(File::open(path)?)?;
        let manifest = read_manifest(&mut archive, server_id, backup_id)?;
        ensure!(
            manifest.minecraft_version == self.get(server_id)?.version,
            "Backup version does not match the server"
        );
        let operation = Uuid::new_v4().to_string();
        let staging = dir.join(format!("restore-staging-{operation}"));
        let previous = dir.join(format!("world-before-restore-{operation}"));
        fs::create_dir(&staging)?;
        if let Err(error) = extract(&mut archive, &manifest, &staging) {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        // The marker is durable before any change to the world. On interruption startup is blocked.
        let marker = dir.join(MARKER);
        let mut pending = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)?;
        pending.write_all(&serde_json::to_vec(&Pending { id: operation })?)?;
        pending.sync_all()?;
        sync_dir(&dir)?;
        fs::rename(&world, &previous)?;
        sync_dir(&dir)?;
        fs::rename(&staging, &world)?;
        sync_dir(&dir)?;
        fs::remove_file(marker)?;
        sync_dir(&dir)?;
        Ok(())
    }
    /// Rolls back interrupted swaps, or finishes an already-completed swap. Preserves old world.
    pub fn recover_restore(&self, server_id: &str) -> Result<()> {
        let dir = self.server_dir(server_id)?;
        let _lock = lock(&dir.join("process.lock"))?;
        let marker = dir.join(MARKER);
        let metadata = fs::symlink_metadata(&marker)?;
        ensure!(
            regular(&metadata) && metadata.is_file() && metadata.len() <= 4096,
            "Invalid recovery marker"
        );
        let pending: Pending = serde_json::from_slice(&fs::read(&marker)?)?;
        uuid(&pending.id)?;
        let world = dir.join("world");
        let staging = dir.join(format!("restore-staging-{}", pending.id));
        let previous = dir.join(format!("world-before-restore-{}", pending.id));
        for path in [&world, &staging, &previous] {
            if let Ok(meta) = fs::symlink_metadata(path) {
                ensure!(regular(&meta) && meta.is_dir(), "Unsafe recovery path");
            }
        }
        match (world.exists(), staging.exists(), previous.exists()) {
            (true, true, false) => {
                fs::remove_dir_all(&staging)?;
            }
            (false, true, true) => {
                fs::rename(&previous, &world)?;
                sync_dir(&dir)?;
                fs::remove_dir_all(&staging)?;
            }
            (true, false, true) => {}
            _ => bail!("Ambiguous restore state; preserve files for manual recovery"),
        }
        fs::remove_file(marker)?;
        sync_dir(&dir)?;
        Ok(())
    }
}
