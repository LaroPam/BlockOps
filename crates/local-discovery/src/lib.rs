//! Strict primitives for private Windows descriptor discovery; not yet connected to the UI.
#[cfg(any(windows, test))]
use anyhow::ensure;
use anyhow::Result;
#[cfg(any(windows, test))]
use serde::Deserialize;
use std::path::Path;

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
