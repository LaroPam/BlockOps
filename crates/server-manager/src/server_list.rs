//! Minecraft's multiplayer server list (`servers.dat`), edited to add one entry.
//!
//! The file is uncompressed NBT and has kept its shape since 1.3: a root compound with a
//! `servers` list of compounds holding `name` and `ip`. Everything else, such as server
//! icons and tags newer versions add, is read and written back unchanged, byte for byte.
use anyhow::{bail, ensure, Context, Result};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

const MAX_FILE: u64 = 16 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const COMPOUND: u8 = 10;
const STRING: u8 = 8;

/// An NBT value. Strings keep Java's modified UTF-8 bytes; numbers keep their raw bits.
#[derive(Clone, Debug, PartialEq)]
enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(u32),
    Double(u64),
    ByteArray(Vec<u8>),
    String(Vec<u8>),
    List(u8, Vec<Tag>),
    Compound(Vec<(Vec<u8>, Tag)>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Tag {
    fn id(&self) -> u8 {
        match self {
            Tag::Byte(_) => 1,
            Tag::Short(_) => 2,
            Tag::Int(_) => 3,
            Tag::Long(_) => 4,
            Tag::Float(_) => 5,
            Tag::Double(_) => 6,
            Tag::ByteArray(_) => 7,
            Tag::String(_) => STRING,
            Tag::List(..) => 9,
            Tag::Compound(_) => COMPOUND,
            Tag::IntArray(_) => 11,
            Tag::LongArray(_) => 12,
        }
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        ensure!(n <= self.0.len(), "Truncated server list");
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into()?))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into()?))
    }
    fn length(&mut self, element: usize) -> Result<usize> {
        let length = self.i32()?;
        ensure!(length >= 0, "Negative length in server list");
        let length = length as usize;
        ensure!(
            length.saturating_mul(element) <= self.0.len(),
            "Truncated server list"
        );
        Ok(length)
    }
    fn string(&mut self) -> Result<Vec<u8>> {
        let length = self.u16()? as usize;
        Ok(self.take(length)?.to_vec())
    }
    fn payload(&mut self, id: u8, depth: usize) -> Result<Tag> {
        ensure!(depth <= MAX_DEPTH, "Server list nested too deeply");
        Ok(match id {
            1 => Tag::Byte(self.u8()? as i8),
            2 => Tag::Short(self.u16()? as i16),
            3 => Tag::Int(self.i32()?),
            4 => Tag::Long(self.i64()?),
            5 => Tag::Float(self.i32()? as u32),
            6 => Tag::Double(self.i64()? as u64),
            7 => {
                let length = self.length(1)?;
                Tag::ByteArray(self.take(length)?.to_vec())
            }
            STRING => Tag::String(self.string()?),
            9 => {
                let element = self.u8()?;
                let length = self.length(1)?;
                ensure!(element != 0 || length == 0, "Invalid list in server list");
                let mut items = Vec::with_capacity(length.min(1024));
                for _ in 0..length {
                    items.push(self.payload(element, depth + 1)?);
                }
                Tag::List(element, items)
            }
            COMPOUND => {
                let mut entries = Vec::new();
                loop {
                    let id = self.u8()?;
                    if id == 0 {
                        break;
                    }
                    let name = self.string()?;
                    entries.push((name, self.payload(id, depth + 1)?));
                }
                Tag::Compound(entries)
            }
            11 => {
                let length = self.length(4)?;
                Tag::IntArray((0..length).map(|_| self.i32()).collect::<Result<_>>()?)
            }
            12 => {
                let length = self.length(8)?;
                Tag::LongArray((0..length).map(|_| self.i64()).collect::<Result<_>>()?)
            }
            _ => bail!("Unknown tag in server list"),
        })
    }
}

fn write_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn write_payload(out: &mut Vec<u8>, tag: &Tag) {
    match tag {
        Tag::Byte(v) => out.push(*v as u8),
        Tag::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Float(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Double(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::ByteArray(v) => {
            out.extend_from_slice(&(v.len() as i32).to_be_bytes());
            out.extend_from_slice(v);
        }
        Tag::String(v) => write_string(out, v),
        Tag::List(element, items) => {
            out.push(*element);
            out.extend_from_slice(&(items.len() as i32).to_be_bytes());
            for item in items {
                write_payload(out, item);
            }
        }
        Tag::Compound(entries) => {
            for (name, value) in entries {
                out.push(value.id());
                write_string(out, name);
                write_payload(out, value);
            }
            out.push(0);
        }
        Tag::IntArray(v) => {
            out.extend_from_slice(&(v.len() as i32).to_be_bytes());
            for item in v {
                out.extend_from_slice(&item.to_be_bytes());
            }
        }
        Tag::LongArray(v) => {
            out.extend_from_slice(&(v.len() as i32).to_be_bytes());
            for item in v {
                out.extend_from_slice(&item.to_be_bytes());
            }
        }
    }
}

/// Java's modified UTF-8 for a name or address: equal to UTF-8 for these characters.
fn java_string(text: &str) -> Result<Vec<u8>> {
    ensure!(
        text.chars().all(|c| c != '\0' && (c as u32) < 0x10000) && text.len() < 1024,
        "Unsupported characters for the server list"
    );
    Ok(text.as_bytes().to_vec())
}

fn parse(bytes: &[u8]) -> Result<(Vec<u8>, Tag)> {
    let mut reader = Reader(bytes);
    ensure!(
        reader.u8()? == COMPOUND,
        "Server list does not start with a compound"
    );
    let name = reader.string()?;
    let root = reader.payload(COMPOUND, 0)?;
    ensure!(reader.0.is_empty(), "Trailing data in server list");
    Ok((name, root))
}

fn serialize(name: &[u8], root: &Tag) -> Vec<u8> {
    let mut out = vec![COMPOUND];
    write_string(&mut out, name);
    write_payload(&mut out, root);
    out
}

/// Whether the list already holds an entry for `address`.
fn contains(root: &Tag, address: &[u8]) -> bool {
    let Tag::Compound(entries) = root else {
        return false;
    };
    entries.iter().any(|(key, value)| {
        key == b"servers"
            && matches!(value, Tag::List(COMPOUND, servers) if servers.iter().any(|server| {
                matches!(server, Tag::Compound(fields) if fields.iter().any(|(field, value)| {
                    field == b"ip" && matches!(value, Tag::String(ip) if ip == address)
                }))
            }))
    })
}

/// Put a server first in the list unless its address is already there. Returns whether the
/// file changed. The previous file is kept as `servers.dat_old`, as Minecraft itself does.
pub fn ensure_entry(game_dir: &Path, name: &str, address: &str) -> Result<bool> {
    ensure!(game_dir.is_dir(), "Minecraft game folder does not exist");
    let path = game_dir.join("servers.dat");
    let address = java_string(address)?;
    let (root_name, mut root) = match fs::File::open(&path) {
        Ok(file) => {
            ensure!(file.metadata()?.is_file(), "servers.dat is not a file");
            let mut bytes = Vec::new();
            file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
            ensure!(bytes.len() as u64 <= MAX_FILE, "servers.dat is too large");
            parse(&bytes).context("servers.dat is not a valid server list")?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (Vec::new(), Tag::Compound(Vec::new()))
        }
        Err(error) => return Err(error.into()),
    };
    if contains(&root, &address) {
        return Ok(false);
    }
    let entry = Tag::Compound(vec![
        (b"ip".to_vec(), Tag::String(address)),
        (b"name".to_vec(), Tag::String(java_string(name)?)),
    ]);
    let Tag::Compound(entries) = &mut root else {
        unreachable!("parse returns a compound root")
    };
    match entries.iter_mut().find(|(key, _)| key == b"servers") {
        Some((_, Tag::List(element, servers))) if *element == COMPOUND || servers.is_empty() => {
            *element = COMPOUND;
            servers.insert(0, entry);
        }
        Some(_) => bail!("servers.dat holds an unexpected server list"),
        None => entries.push((b"servers".to_vec(), Tag::List(COMPOUND, vec![entry]))),
    }
    let mut staged = tempfile::NamedTempFile::new_in(game_dir)?;
    staged.write_all(&serialize(&root_name, &root))?;
    staged.as_file().sync_all()?;
    if path.exists() {
        fs::copy(&path, game_dir.join("servers.dat_old"))?;
    }
    staged.persist(&path).map_err(|error| error.error)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A list as a recent version writes it: an icon, a hidden flag and texture choice.
    fn sample() -> Vec<u8> {
        let server = Tag::Compound(vec![
            (b"ip".to_vec(), Tag::String(b"mc.example.org".to_vec())),
            (b"name".to_vec(), Tag::String("Сервер".as_bytes().to_vec())),
            (b"icon".to_vec(), Tag::String(b"iVBORw0KGgo=".to_vec())),
            (b"hidden".to_vec(), Tag::Byte(0)),
            (b"acceptTextures".to_vec(), Tag::Byte(1)),
        ]);
        let extra = Tag::Compound(vec![
            (b"f".to_vec(), Tag::Float(1.5f32.to_bits())),
            (b"d".to_vec(), Tag::Double((-2.25f64).to_bits())),
            (b"s".to_vec(), Tag::Short(-7)),
            (b"l".to_vec(), Tag::Long(i64::MIN)),
            (b"b".to_vec(), Tag::ByteArray(vec![1, 2, 3])),
            (b"i".to_vec(), Tag::IntArray(vec![-1, 2])),
            (b"L".to_vec(), Tag::LongArray(vec![3])),
            (b"n".to_vec(), Tag::Int(42)),
        ]);
        serialize(
            b"",
            &Tag::Compound(vec![
                (b"servers".to_vec(), Tag::List(COMPOUND, vec![server])),
                (b"future".to_vec(), extra),
            ]),
        )
    }

    #[test]
    fn every_tag_round_trips_byte_for_byte() {
        let bytes = sample();
        let (name, root) = parse(&bytes).unwrap();
        assert_eq!(serialize(&name, &root), bytes);
        for truncated in 1..bytes.len() {
            assert!(
                parse(&bytes[..truncated]).is_err(),
                "accepted {truncated} bytes"
            );
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(parse(&trailing).is_err());
    }

    #[test]
    fn the_entry_goes_first_once_and_the_rest_is_kept() {
        let folder = tempfile::tempdir().unwrap();
        fs::write(folder.path().join("servers.dat"), sample()).unwrap();
        assert!(ensure_entry(folder.path(), "BlockOps — мир друга", "127.0.0.1:25595").unwrap());
        assert_eq!(
            fs::read(folder.path().join("servers.dat_old")).unwrap(),
            sample()
        );
        let changed = fs::read(folder.path().join("servers.dat")).unwrap();
        let (_, root) = parse(&changed).unwrap();
        let Tag::Compound(entries) = &root else {
            panic!()
        };
        let Tag::List(COMPOUND, servers) = &entries[0].1 else {
            panic!()
        };
        assert_eq!(servers.len(), 2);
        assert_eq!(
            servers[0],
            Tag::Compound(vec![
                (b"ip".to_vec(), Tag::String(b"127.0.0.1:25595".to_vec())),
                (
                    b"name".to_vec(),
                    Tag::String("BlockOps — мир друга".as_bytes().to_vec())
                ),
            ])
        );
        // The original server and the unknown tags are untouched.
        let (_, original) = parse(&sample()).unwrap();
        let Tag::Compound(original_entries) = &original else {
            panic!()
        };
        let Tag::List(_, original_servers) = &original_entries[0].1 else {
            panic!()
        };
        assert_eq!(servers[1], original_servers[0]);
        assert_eq!(entries[1], original_entries[1]);
        // A second call changes nothing.
        assert!(!ensure_entry(folder.path(), "BlockOps — мир друга", "127.0.0.1:25595").unwrap());
        assert_eq!(
            fs::read(folder.path().join("servers.dat")).unwrap(),
            changed
        );
    }

    #[test]
    fn a_missing_list_is_created_and_a_broken_one_is_left_alone() {
        let folder = tempfile::tempdir().unwrap();
        assert!(ensure_entry(folder.path(), "BlockOps", "127.0.0.1:25595").unwrap());
        let (_, root) = parse(&fs::read(folder.path().join("servers.dat")).unwrap()).unwrap();
        assert!(contains(&root, b"127.0.0.1:25595"));
        assert!(!folder.path().join("servers.dat_old").exists());

        let broken = tempfile::tempdir().unwrap();
        fs::write(broken.path().join("servers.dat"), b"\x0a\x00\x00\x09").unwrap();
        assert!(ensure_entry(broken.path(), "BlockOps", "127.0.0.1:25595").is_err());
        assert_eq!(
            fs::read(broken.path().join("servers.dat")).unwrap(),
            b"\x0a\x00\x00\x09"
        );
    }
}
