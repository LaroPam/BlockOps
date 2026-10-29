//! One-use code authentication for the serverless mode.
//!
//! The players share only a short code. SPAKE2 turns it into a key that a third party
//! learns nothing about, so an attacker gets one online guess per attempt. Both sides then
//! confirm the key over a transcript that names both Noise-authenticated peer ids, so a
//! party in the middle cannot reuse an exchange with a different peer.
use anyhow::{bail, ensure, Result};
use hmac::{Hmac, Mac};
use libp2p::PeerId;
use rand::RngCore;
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Crockford alphabet without I, L, O and U.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Sixteen symbols, 80 random bits: too many to search for even when the rendezvous key,
/// which is public in the DHT, is known.
pub const CODE_SYMBOLS: usize = 16;
const IDENTITY: &[u8] = b"blockops-direct-v1";
const MESSAGE_LEN: usize = 33;
/// A sha2-256 multihash, the key form public DHT nodes accept for provider records.
pub type RendezvousKey = [u8; 34];

/// Case, dashes and spaces do not matter; O, I and L are read as 0, 1 and 1.
pub fn normalize(code: &str) -> String {
    code.chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect()
}

/// Whether a code is meant for the serverless mode, as opposed to a coordinator code.
pub fn is_direct_code(code: &str) -> bool {
    let code = normalize(code);
    code.len() == CODE_SYMBOLS && code.bytes().all(|b| ALPHABET.contains(&b))
}

pub fn generate_code() -> String {
    let mut bytes = [0u8; CODE_SYMBOLS];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let symbols: Vec<u8> = bytes.iter().map(|b| ALPHABET[(b & 31) as usize]).collect();
    symbols
        .chunks(4)
        .map(|group| std::str::from_utf8(group).expect("ASCII alphabet"))
        .collect::<Vec<_>>()
        .join("-")
}

/// Where the host announces itself for a normalized code.
pub fn rendezvous_key(code: &str) -> RendezvousKey {
    let digest = Sha256::new()
        .chain_update(b"blockops-rendezvous-v1\n")
        .chain_update(code.as_bytes())
        .finalize();
    let mut key = [0u8; 34];
    key[0] = 0x12;
    key[1] = 0x20;
    key[2..].copy_from_slice(&digest);
    key
}

fn derive(
    key: &[u8],
    label: &str,
    host: &PeerId,
    guest: &PeerId,
    guest_message: &[u8],
    host_message: &[u8],
) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    for part in [
        label.as_bytes(),
        &host.to_bytes(),
        &guest.to_bytes(),
        guest_message,
        host_message,
    ] {
        mac.update(&(part.len() as u32).to_be_bytes());
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

fn start(code: &str) -> (Spake2<Ed25519Group>, Vec<u8>) {
    Spake2::<Ed25519Group>::start_symmetric(
        &Password::new(normalize(code).as_bytes()),
        &Identity::new(IDENTITY),
    )
}

/// The guest proves knowledge of `code` to `host` and receives its tunnel capability.
pub(crate) async fn guest<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    code: &str,
    host: PeerId,
    guest: PeerId,
) -> Result<[u8; 32]> {
    let (state, message) = start(code);
    ensure!(
        message.len() == MESSAGE_LEN,
        "Unexpected key exchange message"
    );
    stream.write_all(&rendezvous_key(&normalize(code))).await?;
    stream.write_all(&message).await?;
    let mut host_message = [0u8; MESSAGE_LEN];
    stream.read_exact(&mut host_message).await?;
    let key = state
        .finish(&host_message)
        .map_err(|_| anyhow::anyhow!("Invalid key exchange message"))?;
    let mut confirmation = [0u8; 32];
    stream.read_exact(&mut confirmation).await?;
    let expected = derive(&key, "host", &host, &guest, &message, &host_message);
    ensure!(
        bool::from(expected.ct_eq(&confirmation)),
        "The code does not match this host"
    );
    stream
        .write_all(&derive(
            &key,
            "guest",
            &host,
            &guest,
            &message,
            &host_message,
        ))
        .await?;
    ensure!(stream.read_u8().await? == 1, "The host refused access");
    Ok(derive(
        &key,
        "tunnel",
        &host,
        &guest,
        &message,
        &host_message,
    ))
}

/// What a guest asks for: the code it means, and its key exchange message.
pub(crate) struct Request {
    pub key: RendezvousKey,
    message: [u8; MESSAGE_LEN],
}

pub(crate) async fn read_request<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Request> {
    let mut key = [0u8; 34];
    stream.read_exact(&mut key).await?;
    let mut message = [0u8; MESSAGE_LEN];
    stream.read_exact(&mut message).await?;
    Ok(Request { key, message })
}

pub(crate) enum Outcome {
    /// The guest knows the code; the capability has been granted.
    Granted,
    /// The guest does not know the code: one guess has been used up.
    WrongCode,
}

/// Answer a request for `code`. `grant` binds the derived capability to the guest and
/// runs only after the guest has proved it knows the code.
pub(crate) async fn host<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    request: &Request,
    code: &str,
    host: PeerId,
    guest: PeerId,
    grant: impl FnOnce([u8; 32]) -> Result<()>,
) -> Result<Outcome> {
    let (state, message) = start(code);
    ensure!(
        message.len() == MESSAGE_LEN,
        "Unexpected key exchange message"
    );
    stream.write_all(&message).await?;
    let key = match state.finish(&request.message) {
        Ok(key) => key,
        Err(_) => bail!("Invalid key exchange message"),
    };
    stream
        .write_all(&derive(
            &key,
            "host",
            &host,
            &guest,
            &request.message,
            &message,
        ))
        .await?;
    let mut confirmation = [0u8; 32];
    if stream.read_exact(&mut confirmation).await.is_err() {
        // A guest that could not confirm our key used a wrong code.
        return Ok(Outcome::WrongCode);
    }
    let expected = derive(&key, "guest", &host, &guest, &request.message, &message);
    if !bool::from(expected.ct_eq(&confirmation)) {
        return Ok(Outcome::WrongCode);
    }
    grant(derive(
        &key,
        "tunnel",
        &host,
        &guest,
        &request.message,
        &message,
    ))?;
    stream.write_all(&[1]).await?;
    stream.flush().await?;
    Ok(Outcome::Granted)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn exchange(
        guest_code: &str,
        host_code: &str,
        guest_view_of_host: PeerId,
        host_id: PeerId,
    ) -> (Result<[u8; 32]>, Result<Outcome>, Option<[u8; 32]>) {
        let guest_id = PeerId::random();
        let (mut a, mut b) = tokio::io::duplex(1024);
        let guest_code = guest_code.to_owned();
        let guest_side =
            tokio::spawn(
                async move { guest(&mut a, &guest_code, guest_view_of_host, guest_id).await },
            );
        let request = read_request(&mut b).await.unwrap();
        assert_eq!(request.key, rendezvous_key(&normalize(host_code)));
        let mut granted = None;
        let outcome = host(&mut b, &request, host_code, host_id, guest_id, |token| {
            granted = Some(token);
            Ok(())
        })
        .await;
        drop(b);
        (guest_side.await.unwrap(), outcome, granted)
    }

    #[tokio::test]
    async fn matching_codes_agree_on_one_capability() {
        let host_id = PeerId::random();
        let code = generate_code();
        let typed = code.to_lowercase().replace('-', " ").replace('0', "o");
        let (guest, outcome, granted) = exchange(&typed, &code, host_id, host_id).await;
        assert!(matches!(outcome, Ok(Outcome::Granted)));
        assert_eq!(guest.unwrap(), granted.unwrap());
    }

    #[tokio::test]
    async fn a_wrong_code_grants_nothing_and_is_counted() {
        let host_id = PeerId::random();
        let code = generate_code();
        let wrong = generate_code();
        let (mut a, mut b) = tokio::io::duplex(1024);
        let guest_id = PeerId::random();
        let guest_side =
            tokio::spawn(async move { guest(&mut a, &wrong, host_id, guest_id).await });
        let request = read_request(&mut b).await.unwrap();
        let outcome = host(&mut b, &request, &code, host_id, guest_id, |_| {
            panic!("granted without the code")
        })
        .await;
        drop(b);
        assert!(matches!(outcome, Ok(Outcome::WrongCode)));
        assert!(guest_side.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn the_exchange_is_bound_to_the_authenticated_host() {
        // The guest is connected to a different peer than the one answering: confirmation
        // fails, so an exchange cannot be relayed to another identity.
        let code = generate_code();
        let (guest, outcome, granted) =
            exchange(&code, &code, PeerId::random(), PeerId::random()).await;
        assert!(guest.is_err());
        assert!(matches!(outcome, Ok(Outcome::WrongCode)));
        assert!(granted.is_none());
    }

    #[test]
    fn codes_have_their_own_format_and_rendezvous() {
        let code = generate_code();
        assert_eq!(code.len(), 19);
        assert_eq!(code.matches('-').count(), 3);
        assert!(is_direct_code(&code));
        assert!(
            !is_direct_code("ABCD-EFGH-JKMN"),
            "coordinator code taken as direct"
        );
        assert!(
            !is_direct_code("ABCD-EFGH-JKMN-PQRU"),
            "letter outside the alphabet"
        );
        assert_eq!(
            rendezvous_key(&normalize(&code.to_lowercase())),
            rendezvous_key(&normalize(&code))
        );
        assert_ne!(
            rendezvous_key(&normalize(&generate_code())),
            rendezvous_key(&normalize(&code))
        );
    }
}
