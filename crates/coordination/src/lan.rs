//! Worlds opened to LAN on this computer, found without a mod, in any Java version.
//!
//! Since 1.3 Minecraft announces a world opened to LAN on the multicast group
//! 224.0.2.60:4445 about every 1.5 seconds, as `[MOTD]name[/MOTD][AD]port[/AD]`. Only
//! announcements sent from this computer's own addresses count, and a status ping reads the
//! version from the server itself. Neither proves which program owns the port: like a port
//! typed by hand, the tunnel trusts software on this computer. A world whose announcements
//! stop is treated as closed.
use crate::local_bridge::WorldWatch;
use anyhow::{ensure, Result};
use serde::Serialize;
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 2, 60);
/// The port every Minecraft version announces on.
pub const ANNOUNCE_PORT: u16 = 4445;
const MAX_DATAGRAM: usize = 1024;
/// Announcements arrive every 1.5 s; a world silent this long is considered closed.
const SILENCE: Duration = Duration::from_secs(6);
const MAX_WORLDS: usize = 16;
const MAX_STATUS: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LanWorld {
    pub port: u16,
    /// As the game announces it, usually "player - world", without formatting codes.
    pub name: String,
    /// The Minecraft version the server reports, when a status ping succeeded.
    pub version: Option<String>,
}

struct Seen {
    name: String,
    last: Instant,
    version: Option<String>,
    pinged: Option<Instant>,
}

/// The worlds currently announced from this computer.
#[derive(Clone, Default)]
pub struct LanWorlds(Arc<Mutex<HashMap<u16, Seen>>>);

/// `[MOTD]name[/MOTD][AD]port[/AD]`, the name without colour codes or control characters.
fn parse(datagram: &[u8]) -> Option<(String, u16)> {
    let text = std::str::from_utf8(datagram).ok()?;
    let between = |open: &str, close: &str| {
        let start = text.find(open)? + open.len();
        let end = start + text[start..].find(close)?;
        Some(&text[start..end])
    };
    let advertised = between("[AD]", "[/AD]")?;
    // Old versions may announce "host:port"; the port is what matters.
    let port: u16 = advertised.rsplit(':').next()?.trim().parse().ok()?;
    if port == 0 {
        return None;
    }
    let mut name = String::new();
    let mut chars = between("[MOTD]", "[/MOTD]")?.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else if !c.is_control() && name.chars().count() < 64 {
            name.push(c);
        }
    }
    Some((name, port))
}

/// Whether an address belongs to this computer: only it can be bound locally.
fn is_local(address: IpAddr) -> bool {
    address.is_loopback() || std::net::UdpSocket::bind((address, 0)).is_ok()
}

fn varint(mut value: u32, out: &mut Vec<u8>) {
    loop {
        if value & !0x7f == 0 {
            out.push(value as u8);
            return;
        }
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
}

async fn read_varint(stream: &mut tokio::net::TcpStream) -> Result<u32> {
    let mut value = 0u32;
    for shift in (0..35).step_by(7) {
        let byte = stream.read_u8().await?;
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    anyhow::bail!("VarInt too long")
}

/// The version name a Minecraft server on this computer reports (protocol of 1.7 and later).
pub async fn status_version(port: u16) -> Result<String> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect(address),
    )
    .await??;
    tokio::time::timeout(Duration::from_secs(3), async {
        let host = b"127.0.0.1";
        let mut handshake = vec![0x00];
        varint(u32::MAX, &mut handshake); // Protocol -1: "any", as status queries use.
        varint(host.len() as u32, &mut handshake);
        handshake.extend_from_slice(host);
        handshake.extend_from_slice(&port.to_be_bytes());
        varint(1, &mut handshake); // Next state: status.
        let mut packet = Vec::new();
        varint(handshake.len() as u32, &mut packet);
        packet.extend_from_slice(&handshake);
        packet.extend_from_slice(&[0x01, 0x00]); // Status request.
        stream.write_all(&packet).await?;
        let length = read_varint(&mut stream).await? as usize;
        ensure!(
            length > 0 && length <= MAX_STATUS,
            "Invalid status response"
        );
        ensure!(
            read_varint(&mut stream).await? == 0,
            "Invalid status response"
        );
        let json_length = read_varint(&mut stream).await? as usize;
        ensure!(json_length < length, "Invalid status response");
        let mut json = vec![0; json_length];
        stream.read_exact(&mut json).await?;
        let status: serde_json::Value = serde_json::from_slice(&json)?;
        let name = status["version"]["name"]
            .as_str()
            .filter(|name| !name.is_empty() && name.len() <= 64)
            .ok_or_else(|| anyhow::anyhow!("Status without a version"))?;
        Ok(name.chars().filter(|c| !c.is_control()).collect())
    })
    .await?
}

impl LanWorlds {
    /// Listen on the standard announcement port for as long as the application runs.
    pub fn listen() -> Result<Self> {
        Self::listen_on(ANNOUNCE_PORT)
    }

    /// As [`LanWorlds::listen`], on another port for tests.
    pub fn listen_on(port: u16) -> Result<Self> {
        use socket2::{Domain, Protocol, Socket, Type};
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        // Minecraft's own server list listens on the same port; both receive every datagram.
        socket.set_reuse_address(true)?;
        socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
        socket.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED)?;
        socket.set_nonblocking(true)?;
        let socket = tokio::net::UdpSocket::from_std(socket.into())?;
        let worlds = Self::default();
        let seen = worlds.clone();
        tokio::spawn(async move {
            let mut buffer = [0u8; MAX_DATAGRAM];
            let mut local: HashMap<IpAddr, bool> = HashMap::new();
            loop {
                let Ok((length, source)) = socket.recv_from(&mut buffer).await else {
                    continue;
                };
                let trusted = *local
                    .entry(source.ip())
                    .or_insert_with(|| is_local(source.ip()));
                if !trusted {
                    continue;
                }
                if let Some((name, announced)) = parse(&buffer[..length]) {
                    seen.announced(name, announced);
                }
            }
        });
        Ok(worlds)
    }

    fn announced(&self, name: String, port: u16) {
        let ping = {
            let mut worlds = self.0.lock().unwrap();
            let now = Instant::now();
            worlds.retain(|_, seen| now.duration_since(seen.last) < SILENCE);
            if worlds.len() >= MAX_WORLDS && !worlds.contains_key(&port) {
                return;
            }
            let seen = worlds.entry(port).or_insert(Seen {
                name: String::new(),
                last: now,
                version: None,
                pinged: None,
            });
            seen.name = name;
            seen.last = now;
            let due = seen.version.is_none()
                && seen
                    .pinged
                    .is_none_or(|at| now.duration_since(at) > Duration::from_secs(10));
            if due {
                seen.pinged = Some(now);
            }
            due
        };
        if ping {
            let worlds = self.clone();
            tokio::spawn(async move {
                if let Ok(version) = status_version(port).await {
                    if let Some(seen) = worlds.0.lock().unwrap().get_mut(&port) {
                        seen.version = Some(version);
                    }
                }
            });
        }
    }

    /// Worlds announced within the last few seconds, by port.
    pub fn current(&self) -> Vec<LanWorld> {
        let now = Instant::now();
        let mut worlds: Vec<_> = self
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, seen)| now.duration_since(seen.last) < SILENCE)
            .map(|(port, seen)| LanWorld {
                port: *port,
                name: seen.name.clone(),
                version: seen.version.clone(),
            })
            .collect();
        worlds.sort_by_key(|world| world.port);
        worlds
    }

    /// Follow one world; `alive` turns false once its announcements stop.
    pub fn watch(&self, port: u16) -> WorldWatch {
        let (tx, alive) = tokio::sync::watch::channel(true);
        let cancel = tokio_util::sync::CancellationToken::new();
        let stop = cancel.clone();
        let worlds = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = tick.tick() => {
                        if !worlds.current().iter().any(|world| world.port == port) {
                            break;
                        }
                    }
                }
            }
            tx.send_replace(false);
        });
        WorldWatch::new(port, alive, cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcements_are_parsed_strictly() {
        assert_eq!(
            parse(b"[MOTD]Steve - \xc2\xa7aNew World[/MOTD][AD]41234[/AD]"),
            Some(("Steve - New World".into(), 41234))
        );
        assert_eq!(
            parse(b"[MOTD]Old[/MOTD][AD]192.168.1.5:25565[/AD]"),
            Some(("Old".into(), 25565))
        );
        for invalid in [
            b"[MOTD]x[/MOTD][AD]0[/AD]".as_slice(),
            b"[MOTD]x[/MOTD][AD]70000[/AD]".as_slice(),
            b"[MOTD]x[/MOTD]".as_slice(),
            b"[AD]25565[/AD]".as_slice(),
            b"\xff\xfe".as_slice(),
        ] {
            assert_eq!(parse(invalid), None);
        }
    }

    #[test]
    fn only_this_computers_addresses_are_local() {
        assert!(is_local(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!is_local(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))));
    }
}
