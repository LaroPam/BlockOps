//! Just enough of the Minecraft Java protocol (1.7 and later) to tell a player's login from a
//! server list ping, and to read the name the game logs in with. Both come before encryption
//! and compression. Every byte read is kept so the caller passes it on unchanged.
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

/// How a connection to the world began.
#[derive(Debug, PartialEq, Eq)]
pub enum Opening {
    /// A player logging in, with the name the game sent when it is a valid Minecraft name.
    Login(Option<String>),
    /// The server list asking for the world's status.
    Status,
    /// Not recognised; treated like a player without a name.
    Unknown,
}

const MAX_HANDSHAKE: u32 = 1024;
const SNIFF: Duration = Duration::from_secs(5);

async fn byte(reader: &mut (impl AsyncRead + Unpin), seen: &mut Vec<u8>) -> Option<u8> {
    let byte = reader.read_u8().await.ok()?;
    seen.push(byte);
    Some(byte)
}

async fn varint(reader: &mut (impl AsyncRead + Unpin), seen: &mut Vec<u8>) -> Option<u32> {
    let first = byte(reader, seen).await?;
    varint_from(first, reader, seen).await
}

/// A VarInt whose first byte was already read.
async fn varint_from(
    first: u8,
    reader: &mut (impl AsyncRead + Unpin),
    seen: &mut Vec<u8>,
) -> Option<u32> {
    let mut value = u32::from(first & 0x7f);
    let mut last = first;
    for shift in (7..35).step_by(7) {
        if last & 0x80 == 0 {
            return Some(value);
        }
        last = byte(reader, seen).await?;
        value |= u32::from(last & 0x7f) << shift;
    }
    (last & 0x80 == 0).then_some(value)
}

/// A Minecraft name: 1 to 16 letters, digits or underscores.
fn valid_name(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Read the start of a connection from the game. `seen` receives every byte read, also when
/// the opening is not recognised or the game goes quiet.
pub async fn sniff(reader: &mut (impl AsyncRead + Unpin), seen: &mut Vec<u8>) -> Opening {
    tokio::time::timeout(SNIFF, read_opening(reader, seen))
        .await
        .ok()
        .flatten()
        .unwrap_or(Opening::Unknown)
}

async fn read_opening(
    reader: &mut (impl AsyncRead + Unpin),
    seen: &mut Vec<u8>,
) -> Option<Opening> {
    let first = byte(reader, seen).await?;
    // Versions before 1.7 ping with 0xFE, which newer servers still answer.
    if first == 0xfe {
        return Some(Opening::Status);
    }
    let length = varint_from(first, reader, seen).await?;
    if !(1..=MAX_HANDSHAKE).contains(&length) {
        return None;
    }
    let start = seen.len();
    for _ in 0..length {
        byte(reader, seen).await?;
    }
    let mut packet = &seen[start..];
    if read_varint(&mut packet)? != 0 {
        return None;
    }
    read_varint(&mut packet)?; // Protocol version.
    let address = read_varint(&mut packet)? as usize;
    packet = packet.get(address..)?;
    packet = packet.get(2..)?; // Port.
    match read_varint(&mut packet)? {
        1 => Some(Opening::Status),
        // 3 is a transfer to this server (1.20.5 and later), which logs in the same way.
        2 | 3 => Some(Opening::Login(login_name(reader, seen).await)),
        _ => None,
    }
}

/// Login Start begins with the name in every version; the rest is left to flow on.
async fn login_name(reader: &mut (impl AsyncRead + Unpin), seen: &mut Vec<u8>) -> Option<String> {
    varint(reader, seen).await?; // Packet length.
    if varint(reader, seen).await? != 0 {
        return None;
    }
    let length = varint(reader, seen).await?;
    if !(1..=64).contains(&length) {
        return None;
    }
    let start = seen.len();
    for _ in 0..length {
        byte(reader, seen).await?;
    }
    let name = std::str::from_utf8(&seen[start..]).ok()?;
    valid_name(name).then(|| name.to_owned())
}

fn read_varint(packet: &mut &[u8]) -> Option<u32> {
    let mut value = 0u32;
    for shift in (0..35).step_by(7) {
        let (&byte, rest) = packet.split_first()?;
        *packet = rest;
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint_bytes(mut value: u32, out: &mut Vec<u8>) {
        loop {
            if value & !0x7f == 0 {
                out.push(value as u8);
                return;
            }
            out.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
    }
    fn packet(body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        varint_bytes(body.len() as u32, &mut out);
        out.extend_from_slice(body);
        out
    }
    fn handshake(protocol: u32, address: &str, next: u32) -> Vec<u8> {
        let mut body = vec![0];
        varint_bytes(protocol, &mut body);
        varint_bytes(address.len() as u32, &mut body);
        body.extend_from_slice(address.as_bytes());
        body.extend_from_slice(&25565u16.to_be_bytes());
        varint_bytes(next, &mut body);
        packet(&body)
    }
    fn login_start(name: &str, rest: &[u8]) -> Vec<u8> {
        let mut body = vec![0];
        varint_bytes(name.len() as u32, &mut body);
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(rest);
        packet(&body)
    }
    async fn run(input: &[u8]) -> (Opening, Vec<u8>) {
        let mut reader = input;
        let mut seen = Vec::new();
        let opening = sniff(&mut reader, &mut seen).await;
        assert_eq!(
            &input[..seen.len()],
            seen.as_slice(),
            "bytes must be kept in order"
        );
        (opening, seen)
    }

    #[tokio::test]
    async fn logins_are_told_from_pings_and_keep_every_byte() {
        // 1.21.1 login with the UUID that follows the name since 1.20.2.
        let mut input = handshake(767, "127.0.0.1", 2);
        input.extend(login_start("Steve_01", &[7; 16]));
        let (opening, seen) = run(&input).await;
        assert_eq!(opening, Opening::Login(Some("Steve_01".into())));
        // Only the bytes up to the name were read; the UUID flows on.
        assert_eq!(seen.len(), input.len() - 16);

        // A Forge 1.20.1 client marks the address; 1.12.2 sends the name alone.
        let mut forge = handshake(763, "127.0.0.1\0FML3\0", 2);
        forge.extend(login_start("Alex", &[]));
        assert_eq!(run(&forge).await.0, Opening::Login(Some("Alex".into())));
        let mut old = handshake(340, "localhost", 2);
        old.extend(login_start("notch", &[]));
        assert_eq!(run(&old).await.0, Opening::Login(Some("notch".into())));

        // A transfer logs in the same way.
        let mut transfer = handshake(767, "127.0.0.1", 3);
        transfer.extend(login_start("Steve", &[]));
        assert_eq!(run(&transfer).await.0, Opening::Login(Some("Steve".into())));

        let mut status = handshake(767, "127.0.0.1", 1);
        status.extend(packet(&[0]));
        let (opening, seen) = run(&status).await;
        assert_eq!(opening, Opening::Status);
        assert_eq!(seen.len(), status.len() - 2, "the status request flows on");
        assert_eq!(run(&[0xfe, 0x01, 0xfa]).await.0, Opening::Status);
    }

    #[tokio::test]
    async fn anything_else_is_unknown_or_nameless() {
        // Names that are not Minecraft names are not shown.
        for name in ["", "Steve Jobs", "§cRed", "ThisNameIsWayTooLong", "Игрок"] {
            let mut input = handshake(767, "127.0.0.1", 2);
            input.extend(login_start(name, &[]));
            assert_eq!(run(&input).await.0, Opening::Login(None), "{name:?}");
        }
        // Login cut short.
        assert_eq!(
            run(&handshake(767, "127.0.0.1", 2)).await.0,
            Opening::Login(None)
        );
        for input in [
            Vec::new(),
            handshake(767, "127.0.0.1", 9),
            vec![0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
            packet(&vec![0; 2000]),
            b"GET / HTTP/1.1\r\n\r\n".to_vec(),
            handshake(767, "127.0.0.1", 2)[..5].to_vec(),
        ] {
            assert_eq!(run(&input).await.0, Opening::Unknown, "{input:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_game_is_not_waited_for() {
        let (mut quiet, _writer) = tokio::io::duplex(64);
        let mut seen = Vec::new();
        assert_eq!(sniff(&mut quiet, &mut seen).await, Opening::Unknown);
        assert!(seen.is_empty());
    }
}
