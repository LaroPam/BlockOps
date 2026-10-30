//! Friends who come back without a new code, and this computer's lasting identity.
//!
//! When a guest proves a code, the host grants it a capability bound to the guest's peer id.
//! A host that remembers the friend keeps that capability and grants it again in later
//! sessions; the guest keeps it with the host's peer id and the host's secret meeting point,
//! where the host announces itself to its friends. The capability alone opens nothing: the
//! connection must also come from the friend's own key. The file holds this computer's
//! private key, so it is written for the owner only.
use crate::{direct::FriendWorld, now, pake::RendezvousKey};
use anyhow::{ensure, Context, Result};
use libp2p::{identity::Keypair, PeerId};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path};

/// Friends a host lets in without a code; with codes they share 32 grants per session.
pub const MAX_FRIENDS: usize = 16;
pub const MAX_WORLDS: usize = 32;
const MAX_NAME: usize = 64;

/// Someone this host lets in without a code.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Friend {
    pub peer: String,
    token: String,
    /// The Minecraft name seen when the friend was remembered, or "Друг".
    pub name: String,
    pub added: u64,
}

/// A friend's world this computer can enter without a code.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct World {
    pub host: String,
    token: String,
    meeting: String,
    /// What the host called the world, e.g. "Steve - New World".
    pub label: String,
    pub added: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Book {
    version: u8,
    key: String,
    meeting: String,
    pub friends: Vec<Friend>,
    pub worlds: Vec<World>,
}

fn bytes32(text: &str) -> Result<[u8; 32]> {
    hex::decode(text)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .context("Invalid stored secret")
}

/// Text from the network or the game, kept short and printable.
pub fn clean_label(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Where a host announces itself to its friends: secret, so the DHT cannot tell whose it is.
pub fn meeting_key(meeting: &[u8; 32]) -> RendezvousKey {
    let digest = Sha256::new()
        .chain_update(b"blockops-friends-v1\n")
        .chain_update(meeting)
        .finalize();
    let mut key = [0u8; 34];
    key[0] = 0x12;
    key[1] = 0x20;
    key[2..].copy_from_slice(&digest);
    key
}

impl Book {
    fn new() -> Self {
        let mut meeting = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut meeting);
        let key = Keypair::generate_ed25519()
            .to_protobuf_encoding()
            .expect("ed25519 keys encode");
        Self {
            version: 1,
            key: hex::encode(key),
            meeting: hex::encode(meeting),
            friends: Vec::new(),
            worlds: Vec::new(),
        }
    }

    /// Read the book, or start one with a new identity when there is none. A damaged book is
    /// an error rather than replaced, so friends are not silently lost.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(data) => {
                ensure!(data.len() <= 1024 * 1024, "Friends file is too large");
                let book: Self =
                    serde_json::from_slice(&data).context("Friends file is damaged")?;
                ensure!(book.version == 1, "Friends file is from a newer BlockOps");
                book.keypair()?;
                book.meeting()?;
                Ok(book)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let book = Self::new();
                book.save(path)?;
                Ok(book)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Replace the file atomically, readable by the owner only.
    pub fn save(&self, path: &Path) -> Result<()> {
        let folder = path.parent().context("Invalid friends file path")?;
        std::fs::create_dir_all(folder)?;
        let mut staged = tempfile::NamedTempFile::new_in(folder)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            staged
                .as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        staged.write_all(&serde_json::to_vec_pretty(self)?)?;
        staged.as_file().sync_all()?;
        staged.persist(path).map_err(|error| error.error)?;
        Ok(())
    }

    pub fn keypair(&self) -> Result<Keypair> {
        Keypair::from_protobuf_encoding(&hex::decode(&self.key)?).context("Invalid stored key")
    }

    pub fn meeting(&self) -> Result<[u8; 32]> {
        bytes32(&self.meeting)
    }

    /// Let `peer` in without a code from the next session on; replaces an earlier entry.
    pub fn remember_friend(&mut self, peer: PeerId, token: [u8; 32], name: &str) -> Result<()> {
        let peer = peer.to_string();
        self.friends.retain(|friend| friend.peer != peer);
        ensure!(
            self.friends.len() < MAX_FRIENDS,
            "Можно запомнить не больше {MAX_FRIENDS} друзей"
        );
        let name = clean_label(name);
        self.friends.push(Friend {
            peer,
            token: hex::encode(token),
            name: if name.is_empty() {
                "Друг".into()
            } else {
                name
            },
            added: now(),
        });
        Ok(())
    }

    pub fn forget_friend(&mut self, peer: &str) -> bool {
        let before = self.friends.len();
        self.friends.retain(|friend| friend.peer != peer);
        self.friends.len() != before
    }

    /// The grants a host session starts with. Entries that do not parse are skipped.
    pub fn friend_grants(&self) -> Vec<(PeerId, [u8; 32])> {
        self.friends
            .iter()
            .filter_map(|friend| Some((friend.peer.parse().ok()?, bytes32(&friend.token).ok()?)))
            .collect()
    }

    /// Keep a host's world for later, newest first; replaces an earlier entry for the host.
    pub fn remember_world(&mut self, world: &FriendWorld) {
        let host = world.host.to_string();
        self.worlds.retain(|known| known.host != host);
        self.worlds.insert(
            0,
            World {
                host,
                token: hex::encode(world.token),
                meeting: hex::encode(world.meeting),
                label: clean_label(&world.label),
                added: now(),
            },
        );
        self.worlds.truncate(MAX_WORLDS);
    }

    pub fn forget_world(&mut self, host: &str) -> bool {
        let before = self.worlds.len();
        self.worlds.retain(|world| world.host != host);
        self.worlds.len() != before
    }

    /// What a guest needs to enter a remembered world.
    pub fn world(&self, host: &str) -> Result<FriendWorld> {
        let world = self
            .worlds
            .iter()
            .find(|world| world.host == host)
            .context("Этот мир не запомнен")?;
        Ok(FriendWorld {
            host: world.host.parse().context("Invalid stored peer id")?,
            token: bytes32(&world.token)?,
            meeting: bytes32(&world.meeting)?,
            label: world.label.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_and_friends_survive_a_restart() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("data").join("friends.json");
        let mut book = Book::load_or_create(&path).unwrap();
        let peer = book.keypair().unwrap().public().to_peer_id();
        let friend = Keypair::generate_ed25519().public().to_peer_id();
        book.remember_friend(friend, [7; 32], "Steve\n").unwrap();
        let host = Keypair::generate_ed25519().public().to_peer_id();
        book.remember_world(&FriendWorld {
            host,
            token: [1; 32],
            meeting: [2; 32],
            label: "Alex - World".into(),
        });
        book.save(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let again = Book::load_or_create(&path).unwrap();
        assert_eq!(again.keypair().unwrap().public().to_peer_id(), peer);
        assert_eq!(again.meeting().unwrap(), book.meeting().unwrap());
        assert_eq!(again.friend_grants(), vec![(friend, [7; 32])]);
        assert_eq!(again.friends[0].name, "Steve");
        let world = again.world(&host.to_string()).unwrap();
        assert_eq!((world.token, world.meeting), ([1; 32], [2; 32]));
        assert_eq!(world.label, "Alex - World");
    }

    #[test]
    fn entries_are_replaced_forgotten_and_limited() {
        let mut book = Book::new();
        let friend = Keypair::generate_ed25519().public().to_peer_id();
        book.remember_friend(friend, [1; 32], "").unwrap();
        book.remember_friend(friend, [2; 32], "Steve").unwrap();
        assert_eq!(book.friend_grants(), vec![(friend, [2; 32])]);
        assert_eq!(book.friends[0].name, "Steve");
        assert!(book.forget_friend(&friend.to_string()));
        assert!(!book.forget_friend(&friend.to_string()));
        assert!(book.friend_grants().is_empty());
        for _ in 0..MAX_FRIENDS {
            let peer = Keypair::generate_ed25519().public().to_peer_id();
            book.remember_friend(peer, [3; 32], "x").unwrap();
        }
        let one_more = Keypair::generate_ed25519().public().to_peer_id();
        assert!(book.remember_friend(one_more, [3; 32], "x").is_err());

        let host = Keypair::generate_ed25519().public().to_peer_id();
        let world = FriendWorld {
            host,
            token: [1; 32],
            meeting: [2; 32],
            label: "Old".into(),
        };
        book.remember_world(&world);
        book.remember_world(&FriendWorld {
            label: "New".into(),
            ..world
        });
        assert_eq!(book.worlds.len(), 1);
        assert_eq!(book.worlds[0].label, "New");
        assert!(book.forget_world(&host.to_string()));
        assert!(book.world(&host.to_string()).is_err());
    }

    #[test]
    fn a_damaged_book_is_not_replaced() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("friends.json");
        std::fs::write(&path, b"{\"version\":1").unwrap();
        assert!(Book::load_or_create(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"version\":1");
    }

    #[test]
    fn meeting_points_differ_per_host_and_from_codes() {
        let key = meeting_key(&[1; 32]);
        assert_eq!(&key[..2], &[0x12, 0x20]);
        assert_ne!(key, meeting_key(&[2; 32]));
        assert_ne!(key, crate::pake::rendezvous_key("0000000000000000"));
        assert_eq!(clean_label("  a\u{7}b  "), "ab");
    }
}
