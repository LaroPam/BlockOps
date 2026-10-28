//! Private Windows descriptor discovery of the local Minecraft companion.
#[cfg(any(windows, test))]
use anyhow::ensure;
use anyhow::Result;
#[cfg(any(windows, test))]
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// Bound on directory entries examined, so a cluttered folder cannot stall the caller.
const MAX_ENTRIES: usize = 64;
/// The mod advertises a bridge for under five minutes; anything longer is not trusted.
const MAX_LIFETIME_SECS: u64 = 600;

/// Contains a secret: deliberately not Debug or Serialize.
pub struct Descriptor {
    pub pid: u32,
    pub port: u16,
    pub token: [u8; 32],
    pub expires_at: u64,
}
#[cfg(any(windows, test))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDescriptor {
    version: u8,
    pid: u32,
    port: u16,
    token: String,
    expires_at: u64,
}
#[cfg(any(windows, test))]
fn parse(data: &[u8]) -> Result<Descriptor> {
    ensure!(data.len() <= 4096, "Invalid descriptor");
    let raw: RawDescriptor =
        serde_json::from_slice(data).map_err(|_| anyhow::anyhow!("Invalid descriptor"))?;
    ensure!(
        raw.version == 1 && raw.pid != 0 && raw.port != 0 && raw.expires_at != 0,
        "Invalid descriptor"
    );
    ensure!(
        raw.token.len() == 64 && raw.token.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid descriptor"
    );
    let token: [u8; 32] = hex::decode(&raw.token)
        .map_err(|_| anyhow::anyhow!("Invalid descriptor"))?
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid descriptor"))?;
    Ok(Descriptor {
        pid: raw.pid,
        port: raw.port,
        token,
        expires_at: raw.expires_at,
    })
}

/// Open one file without following reparse points and fail closed unless its owner and
/// every allow ACE match the current Windows user. Does not prove the process or port identity.
#[cfg(windows)]
pub fn read_private(path: &Path) -> Result<Descriptor> {
    let bytes = windows::read_private(path)?;
    parse(&bytes)
}
#[cfg(not(windows))]
pub fn read_private(_path: &Path) -> Result<Descriptor> {
    anyhow::bail!("Private descriptor verification is available only on Windows")
}

/// `%LOCALAPPDATA%\\BlockOps\\discovery`, where the mod publishes its descriptors.
pub fn default_directory() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|base| !base.is_empty())
        .map(|base| PathBuf::from(base).join("BlockOps").join("discovery"))
}

/// Unexpired private descriptors, newest first. Unreadable, foreign or malformed files
/// are skipped silently: they may be stale or hostile, and their contents are secret.
pub fn discover(directory: &Path) -> Vec<Descriptor> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    discover_with(directory, now, |path| {
        let descriptor = read_private(path)?;
        anyhow::ensure!(process_alive(descriptor.pid), "Descriptor owner exited");
        Ok(descriptor)
    })
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    windows::process_alive(pid)
}
#[cfg(not(windows))]
fn process_alive(_pid: u32) -> bool {
    false
}

fn discover_with(
    directory: &Path,
    now: u64,
    read: impl Fn(&Path) -> Result<Descriptor>,
) -> Vec<Descriptor> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut found: Vec<_> = entries
        .take(MAX_ENTRIES)
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("minecraft-") && name.ends_with(".json"))
        })
        .filter_map(|path| read(&path).ok())
        .filter(|descriptor| {
            descriptor.expires_at > now && descriptor.expires_at <= now + MAX_LIFETIME_SECS
        })
        .collect();
    found.sort_by_key(|descriptor| std::cmp::Reverse(descriptor.expires_at));
    found
}

#[cfg(windows)]
mod windows;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_valid_descriptor_and_rejects_extra_fields() {
        let valid = format!(
            r#"{{"version":1,"pid":42,"port":25565,"token":"{}","expires_at":123}}"#,
            "ab".repeat(32)
        );
        let descriptor = parse(valid.as_bytes()).unwrap();
        assert_eq!(descriptor.pid, 42);
        assert_eq!(descriptor.port, 25565);
        assert_eq!(descriptor.token, [0xab; 32]);
        assert_eq!(descriptor.expires_at, 123);
        let extra = valid.replacen("{", "{\"extra\":true,", 1);
        assert!(parse(extra.as_bytes()).is_err());
        assert!(parse(&vec![b' '; 4097]).is_err());
    }
    #[test]
    fn discovery_returns_only_live_named_descriptors() {
        let directory = tempfile::tempdir().unwrap();
        let write = |name: &str, port: u16, expires_at: u64| {
            let json = format!(
                r#"{{"version":1,"pid":7,"port":{port},"token":"{}","expires_at":{expires_at}}}"#,
                "cd".repeat(32)
            );
            std::fs::write(directory.path().join(name), json).unwrap();
        };
        write("minecraft-1.json", 1001, 1100);
        write("minecraft-2.json", 1002, 1200);
        write("minecraft-3.json", 1003, 1000); // expired
        write("minecraft-4.json", 1004, 1000 + MAX_LIFETIME_SECS + 1); // implausible
        write("minecraft-5.tmp", 1005, 1200); // still being written
        write("other.json", 1006, 1200);
        std::fs::write(directory.path().join("minecraft-6.json"), b"{broken").unwrap();
        let read = |path: &Path| parse(&std::fs::read(path)?);
        let found = discover_with(directory.path(), 1000, read);
        let ports: Vec<_> = found.iter().map(|descriptor| descriptor.port).collect();
        assert_eq!(ports, [1002, 1001]);
        assert!(discover_with(&directory.path().join("missing"), 1000, read).is_empty());
    }
    #[test]
    fn malformed_descriptors_do_not_echo_secrets() {
        for bytes in [
            b"{bad secret}".as_slice(),
            br#"{"version":1,"pid":1,"port":2,"token":"secret","expires_at":1}"#.as_slice(),
        ] {
            let error = match parse(bytes) {
                Ok(_) => panic!("invalid descriptor accepted"),
                Err(error) => error.to_string(),
            };
            assert!(!error.contains("secret"));
        }
    }
}
