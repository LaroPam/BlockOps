use anyhow::{bail, ensure, Context, Result};
use reqwest::{Client, Url};
use serde::Deserialize;
use sha1::Digest;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
pub const VERSION: &str = "1.21.1";
/// Explicitly supported release versions whose server metadata requires Java 21.
pub const SUPPORTED_VERSIONS: &[&str] = &["1.21.1", "1.21.4"];
pub fn supported_version(version: &str) -> bool {
    SUPPORTED_VERSIONS.contains(&version)
}
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

pub fn official_url(url: &str) -> Result<Url> {
    let url = Url::parse(url)?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443),
        "Only official HTTPS downloads are allowed"
    );
    ensure!(
        matches!(
            url.host_str(),
            Some(
                "piston-meta.mojang.com"
                    | "piston-data.mojang.com"
                    | "api.adoptium.net"
                    | "github.com"
                    | "release-assets.githubusercontent.com"
            )
        ),
        "Download host is not allowed"
    );
    Ok(url)
}
fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent("BlockOps/0.2 (open-source Minecraft launcher)")
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() > 5 || official_url(attempt.url().as_str()).is_err() {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()?)
}
async fn bytes(url: &str, max: u64) -> Result<Vec<u8>> {
    let mut response = client()?
        .get(official_url(url)?)
        .send()
        .await?
        .error_for_status()?;
    ensure!(
        response.status().is_success(),
        "Download redirect was rejected"
    );
    if let Some(size) = response.content_length() {
        ensure!(size <= max, "Download too large");
    }
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            (data.len() as u64) + chunk.len() as u64 <= max,
            "Download too large"
        );
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}
#[derive(Clone, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub sha1: String,
    pub size: u64,
}
#[derive(Deserialize)]
struct Manifest {
    versions: Vec<Version>,
}
#[derive(Deserialize)]
struct Version {
    id: String,
    url: String,
    sha1: String,
    #[serde(rename = "type")]
    kind: String,
}
#[derive(Deserialize)]
struct Metadata {
    id: String,
    downloads: Downloads,
    #[serde(rename = "javaVersion")]
    java: JavaVersion,
}
#[derive(Deserialize)]
struct Downloads {
    server: Artifact,
}
#[derive(Deserialize)]
struct JavaVersion {
    #[serde(rename = "majorVersion")]
    major: u32,
}

pub fn verify(data: &[u8], hash: &str, sha256: bool) -> Result<()> {
    let actual = if sha256 {
        hex::encode(sha2::Sha256::digest(data))
    } else {
        hex::encode(sha1::Sha1::digest(data))
    };
    ensure!(
        actual == hash.to_ascii_lowercase(),
        "Artifact checksum mismatch; refusing installation"
    );
    Ok(())
}
pub async fn vanilla_version(requested: &str) -> Result<Artifact> {
    ensure!(
        supported_version(requested),
        "Unsupported Minecraft version"
    );
    let manifest: Manifest = serde_json::from_slice(&bytes(MANIFEST, 8 * 1024 * 1024).await?)?;
    let version = manifest
        .versions
        .iter()
        .find(|v| v.id == requested && v.kind == "release")
        .context("Minecraft version missing from official manifest")?;
    let data = bytes(&version.url, 8 * 1024 * 1024).await?;
    verify(&data, &version.sha1, false)?;
    let metadata: Metadata = serde_json::from_slice(&data)?;
    ensure!(
        metadata.id == requested && metadata.java.major == 21,
        "Unexpected Minecraft/Java compatibility metadata"
    );
    Ok(metadata.downloads.server)
}
pub async fn download_server(artifact: &Artifact, path: &Path) -> Result<()> {
    ensure!(artifact.size <= MAX_DOWNLOAD, "Server artifact too large");
    let data = bytes(&artifact.url, artifact.size).await?;
    ensure!(
        data.len() as u64 == artifact.size,
        "Incomplete server download"
    );
    verify(&data, &artifact.sha1, false)?;
    tokio::fs::write(path, data).await?;
    Ok(())
}

#[derive(Deserialize)]
struct Runtime {
    binary: Binary,
}
#[derive(Deserialize)]
struct Binary {
    package: Package,
}
#[derive(Deserialize)]
struct Package {
    link: String,
    checksum: String,
    size: u64,
}

pub async fn java21(root: &Path) -> Result<PathBuf> {
    let dest = root.join("java21");
    let executable = dest.join(if cfg!(windows) {
        "bin/java.exe"
    } else {
        "bin/java"
    });
    if executable.exists() {
        check_java(&executable).await?;
        return Ok(executable);
    }
    ensure!(
        cfg!(target_arch = "x86_64"),
        "Automatic Java installation currently supports x86_64 only"
    );
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        bail!("Automatic Java installation currently supports Windows/Linux only")
    };
    let url = format!("https://api.adoptium.net/v3/assets/latest/21/hotspot?architecture=x64&image_type=jre&os={os}&vendor=eclipse");
    let assets: Vec<Runtime> = serde_json::from_slice(&bytes(&url, 2 * 1024 * 1024).await?)?;
    let package = &assets
        .first()
        .context("No official Java runtime for this platform")?
        .binary
        .package;
    ensure!(package.size <= MAX_DOWNLOAD, "Java download too large");
    let data = bytes(&package.link, package.size).await?;
    ensure!(
        data.len() as u64 == package.size,
        "Incomplete Java download"
    );
    verify(&data, &package.checksum, true)?;
    let root = root.to_owned();
    let staged = tokio::task::spawn_blocking(move || -> Result<tempfile::TempDir> {
        let staged = tempfile::tempdir_in(&root)?;
        extract(&data, staged.path(), cfg!(windows))?;
        Ok(staged)
    })
    .await??;
    let dirs: Vec<_> = std::fs::read_dir(staged.path())?.collect::<std::io::Result<Vec<_>>>()?;
    ensure!(
        dirs.len() == 1 && dirs[0].file_type()?.is_dir(),
        "Unexpected Java archive layout"
    );
    let candidate = dirs[0].path().join(if cfg!(windows) {
        "bin/java.exe"
    } else {
        "bin/java"
    });
    check_java(&candidate).await?;
    tokio::fs::rename(dirs[0].path(), &dest).await?;
    Ok(executable)
}
pub async fn check_java(java: &Path) -> Result<()> {
    let mut command = tokio::process::Command::new(java);
    for name in [
        "JAVA_TOOL_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "_JAVA_OPTIONS",
        "CLASSPATH",
    ] {
        command.env_remove(name);
    }
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        command.arg("-version").kill_on_drop(true).output(),
    )
    .await??;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    ensure!(
        output.status.success() && text.contains("version \"21."),
        "Java 21 is required"
    );
    Ok(())
}
fn safe_relative(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty()
            && path.components().all(|c| matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )),
        "Unsafe archive path"
    );
    ensure!(
        !path.components().any(|c| match c {
            std::path::Component::Normal(v) =>
                v.to_string_lossy().contains(':') || v.to_string_lossy().contains('\\'),
            _ => false,
        }),
        "Unsafe archive component"
    );
    Ok(())
}
/// Reject path escapes, special files and oversized expanded archives. Link targets must exist inside staging.
pub fn extract(data: &[u8], root: &Path, is_zip: bool) -> Result<()> {
    let mut total = 0u64;
    let mut count = 0;
    if is_zip {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(data))?;
        ensure!(archive.len() <= 20000, "Too many archive entries");
        for i in 0..archive.len() {
            let file = archive.by_index(i)?;
            let relative = file.enclosed_name().context("Unsafe ZIP path")?;
            safe_relative(&relative)?;
            ensure!(
                file.unix_mode().unwrap_or(0) & 0o170000 != 0o120000,
                "ZIP symlinks are not allowed"
            );
            total = total
                .checked_add(file.size())
                .context("Archive size overflow")?;
            ensure!(
                total <= 2 * 1024 * 1024 * 1024,
                "Expanded Java archive too large"
            );
            let dest = root.join(relative);
            if file.is_dir() {
                std::fs::create_dir_all(dest)?;
            } else {
                std::fs::create_dir_all(dest.parent().context("Invalid path")?)?;
                let mut out = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(dest)?;
                let size = file.size();
                let copied = std::io::copy(&mut file.take(size + 1), &mut out)?;
                ensure!(copied == size, "Archive entry size mismatch");
                out.flush()?;
            }
        }
    } else {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(data));
        let mut links = Vec::new();
        for entry in archive.entries()? {
            let mut entry = entry?;
            count += 1;
            ensure!(count <= 20000, "Too many archive entries");
            let path = entry.path()?.into_owned();
            safe_relative(&path)?;
            total = total
                .checked_add(entry.size())
                .context("Archive size overflow")?;
            ensure!(
                total <= 2 * 1024 * 1024 * 1024,
                "Expanded Java archive too large"
            );
            let kind = entry.header().entry_type();
            if kind.is_symlink() {
                let target = entry
                    .link_name()?
                    .context("Missing link target")?
                    .into_owned();
                ensure!(!target.is_absolute(), "Absolute archive link");
                links.push((root.join(path), target));
            } else {
                ensure!(kind.is_file() || kind.is_dir(), "Unsupported archive entry");
                ensure!(entry.unpack_in(root)?, "Unsafe TAR path");
            }
        }
        #[cfg(not(unix))]
        ensure!(links.is_empty(), "TAR links unsupported on this platform");
        #[cfg(unix)]
        for (dest, target) in links {
            let resolved = dest
                .parent()
                .context("Invalid link")?
                .join(&target)
                .canonicalize()?;
            ensure!(
                resolved.starts_with(root.canonicalize()?),
                "Archive link escapes destination"
            );
            std::os::unix::fs::symlink(target, dest)?;
        }
    }
    Ok(())
}

pub fn verify_file(path: &Path, sha1: &str) -> Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = sha1::Sha1::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    ensure!(
        hex::encode(hash.finalize()) == sha1,
        "Installed server checksum mismatch"
    );
    Ok(())
}
