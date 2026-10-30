//! Just enough of the Minecraft Java protocol (1.7 and later) to tell a player's login from a
//! server list ping, and to read the name the game logs in with. Both come before encryption
//! and compression. The parser only looks at a copy of the first bytes: the connection
//! itself is never held back.
use std::time::Duration;

/// How a connection to the world began.
#[derive(Debug, PartialEq, Eq)]
pub enum Opening {
    /// A player logging in, with the name the game sent when it is a valid Minecraft name.
    Login(Option<String>),
    /// The server list asking for the world's status.
    Status,
    /// Not recognised in time; treated like a player without a name.
    Unknown,
}

const MAX_HANDSHAKE: u32 = 1024;
/// Enough for the longest handshake and the start of Login Start up to the name.
const LIMIT: usize = 1200;
/// How long the opening may take before the connection counts as unknown.
pub const SNIFF: Duration = Duration::from_secs(5);

enum Need {
    More,
    Bad,
}

fn varint(rest: &mut &[u8]) -> Result<u32, Need> {
    let mut value = 0u32;
    for shift in (0..35).step_by(7) {
        let (&byte, tail) = rest.split_first().ok_or(Need::More)?;
        *rest = tail;
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(Need::Bad)
}

fn take<'a>(rest: &mut &'a [u8], count: usize) -> Result<&'a [u8], Need> {
    if rest.len() < count {
        return Err(Need::More);
    }
    let (taken, tail) = rest.split_at(count);
    *rest = tail;
    Ok(taken)
}

/// A Minecraft name: 1 to 16 letters, digits or underscores.
fn valid_name(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The next state a complete handshake packet asks for.
fn next_state(mut packet: &[u8]) -> Option<u32> {
    let packet = &mut packet;
    if varint(packet).ok()? != 0 {
        return None;
    }
    varint(packet).ok()?; // Protocol version.
    let address = varint(packet).ok()? as usize;
    take(packet, address).ok()?;
    take(packet, 2).ok()?; // Port.
    varint(packet).ok()
}

/// Login Start begins with the name in every version; nothing after it is needed.
fn login_name(rest: &mut &[u8]) -> Result<Option<String>, Need> {
    varint(rest)?; // Packet length.
    if varint(rest)? != 0 {
        return Ok(None);
    }
    let length = varint(rest)?;
    if !(1..=64).contains(&length) {
        return Ok(None);
    }
    let name = take(rest, length as usize)?;
    Ok(std::str::from_utf8(name)
        .ok()
        .filter(|name| valid_name(name))
        .map(Into::into))
}

fn parse(bytes: &[u8]) -> Result<Opening, Need> {
    let mut rest = bytes;
    match rest.first() {
        None => return Err(Need::More),
        // Versions before 1.7 ping with 0xFE, which newer servers still answer.
        Some(0xfe) => return Ok(Opening::Status),
        Some(_) => {}
    }
    let length = varint(&mut rest)?;
    if !(1..=MAX_HANDSHAKE).contains(&length) {
        return Err(Need::Bad);
    }
    let packet = take(&mut rest, length as usize)?;
    match next_state(packet).ok_or(Need::Bad)? {
        1 => Ok(Opening::Status),
        // 3 is a transfer to this server (1.20.5 and later), which logs in the same way.
        2 | 3 => login_name(&mut rest).map(Opening::Login),
        _ => Err(Need::Bad),
    }
}

/// Watches the first bytes a game sends and tells, once, how the connection began.
#[derive(Default)]
pub struct Sniffer {
    seen: Vec<u8>,
    decided: bool,
}

impl Sniffer {
    pub fn decided(&self) -> bool {
        self.decided
    }

    /// Look at the next bytes sent. Returns the opening as soon as it is known, then never
    /// again.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<Opening> {
        if self.decided {
            return None;
        }
        let room = LIMIT - self.seen.len();
        self.seen.extend_from_slice(&bytes[..bytes.len().min(room)]);
        let opening = match parse(&self.seen) {
            Ok(opening) => opening,
            Err(Need::More) if self.seen.len() < LIMIT => return None,
            Err(_) => Opening::Unknown,
        };
        self.decided = true;
        self.seen = Vec::new();
        Some(opening)
    }

    /// The game went quiet before its opening was complete.
    pub fn give_up(&mut self) -> Option<Opening> {
        (!std::mem::replace(&mut self.decided, true)).then_some(Opening::Unknown)
    }
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
    /// The opening, fed at once and byte by byte, which must agree.
    fn run(input: &[u8]) -> Option<Opening> {
        let mut whole = Sniffer::default();
        let at_once = whole.feed(input);
        let mut sniffer = Sniffer::default();
        let mut found = None;
        for byte in input {
            if let Some(opening) = sniffer.feed(std::slice::from_ref(byte)) {
                assert!(found.is_none(), "decided twice");
                found = Some(opening);
            }
        }
        assert_eq!(found, at_once);
        at_once
    }

    #[test]
    fn logins_are_told_from_pings() {
        // 1.21.1 login with the UUID that follows the name since 1.20.2.
        let mut input = handshake(767, "127.0.0.1", 2);
        input.extend(login_start("Steve_01", &[7; 16]));
        assert_eq!(run(&input), Some(Opening::Login(Some("Steve_01".into()))));

        // A Forge 1.20.1 client marks the address; 1.12.2 sends the name alone.
        let mut forge = handshake(763, "127.0.0.1\0FML3\0", 2);
        forge.extend(login_start("Alex", &[]));
        assert_eq!(run(&forge), Some(Opening::Login(Some("Alex".into()))));
        let mut old = handshake(340, "localhost", 2);
        old.extend(login_start("notch", &[]));
        assert_eq!(run(&old), Some(Opening::Login(Some("notch".into()))));

        // A transfer logs in the same way.
        let mut transfer = handshake(767, "127.0.0.1", 3);
        transfer.extend(login_start("Steve", &[]));
        assert_eq!(run(&transfer), Some(Opening::Login(Some("Steve".into()))));

        let mut status = handshake(767, "127.0.0.1", 1);
        status.extend(packet(&[0]));
        assert_eq!(run(&status), Some(Opening::Status));
        assert_eq!(run(&[0xfe, 0x01, 0xfa]), Some(Opening::Status));
    }

    #[test]
    fn anything_else_is_unknown_or_nameless() {
        // Names that are not Minecraft names are not shown.
        for name in ["", "Steve Jobs", "§cRed", "ThisNameIsWayTooLong", "Игрок"] {
            let mut input = handshake(767, "127.0.0.1", 2);
            input.extend(login_start(name, &[]));
            assert_eq!(run(&input), Some(Opening::Login(None)), "{name:?}");
        }
        for input in [
            handshake(767, "127.0.0.1", 9),
            vec![0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
            packet(&vec![0; 2000]),
            packet(&[5, 0, 0]),
        ] {
            assert_eq!(run(&input), Some(Opening::Unknown), "{input:?}");
        }
        // Too short to tell: the caller gives up after a while.
        for input in [
            Vec::new(),
            b"ping".to_vec(),
            handshake(767, "127.0.0.1", 2),
            handshake(767, "127.0.0.1", 2)[..5].to_vec(),
        ] {
            let mut sniffer = Sniffer::default();
            assert_eq!(sniffer.feed(&input), None, "{input:?}");
            assert_eq!(sniffer.give_up(), Some(Opening::Unknown));
            assert_eq!(sniffer.give_up(), None);
            assert_eq!(sniffer.feed(&input), None);
        }
    }
}
