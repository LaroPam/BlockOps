use crate::{Claim, Create, Created, Redeem};
use axum::{
    extract::{ConnectInfo, DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use blockops_tunnel::{now, Invite};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::Notify;

type Error = StatusCode;
struct Record {
    invite: Invite,
    code_hash: [u8; 32],
    code_expiry: u64,
    management: [u8; 32],
    peer: Option<String>,
    claim_deadline: Option<Instant>,
    active: bool,
    changed: Arc<Notify>,
}
#[derive(Default)]
struct Inner {
    records: HashMap<String, Record>,
    rates: HashMap<std::net::IpAddr, (Instant, u32)>,
}
#[derive(Clone, Default)]
pub struct Store(Arc<Mutex<Inner>>);
fn secret() -> String {
    let mut bytes = [0; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}
fn hash(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}
/// Crockford decoding: case, dashes and spaces do not matter, and the letters the alphabet
/// omits are read as the digits people confuse them with.
fn normalize(code: &str) -> String {
    code.chars()
        .filter(|c| *c != '-' && !c.is_ascii_whitespace())
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect()
}
fn code() -> String {
    const ABC: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"; // 32 symbols, 60 random bits.
    let mut bytes = [0; 12];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let raw: String = bytes
        .iter()
        .map(|b| ABC[(b & 31) as usize] as char)
        .collect();
    format!("{}-{}-{}", &raw[..4], &raw[4..8], &raw[8..])
}
impl Store {
    fn rate(&self, ip: std::net::IpAddr) -> Result<(), Error> {
        let mut inner = self.0.lock().unwrap();
        inner
            .rates
            .retain(|_, (t, _)| t.elapsed() < Duration::from_secs(60));
        if !inner.rates.contains_key(&ip) && inner.rates.len() >= 4096 {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        let (_, count) = inner.rates.entry(ip).or_insert((Instant::now(), 0));
        *count = count.saturating_add(1);
        if *count > 30 {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        Ok(())
    }
    fn cleanup(inner: &mut Inner) {
        inner.records.retain(|_, r| {
            // A redeemed-but-unconfirmed claim has a bounded acknowledgement window.
            // Do not retain abandoned claims for the entire host session.
            let pending = r
                .claim_deadline
                .is_some_and(|deadline| deadline > Instant::now());
            let unused = r.peer.is_none() && r.code_expiry > now();
            let keep = r.invite.expires_at > now() && (r.active || pending || unused);
            if !keep {
                r.changed.notify_waiters();
            }
            keep
        });
    }
}
fn authorized(headers: &HeaderMap, r: &Record) -> Result<(), Error> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if !bool::from(hash(token).ct_eq(&r.management)) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}
async fn create(
    State(store): State<Store>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(input): Json<Create>,
) -> Result<Json<Created>, Error> {
    store.rate(addr.ip())?;
    if input.code_ttl == 0
        || input.code_ttl > 900
        || input.invite.version != 1
        || input.invite.expires_at <= now()
        || input.invite.expires_at > now() + 86400
        || hex::decode(&input.invite.token).map_or(true, |v| v.len() != 32)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let address: libp2p::Multiaddr = input
        .invite
        .address
        .parse()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if !matches!(
        address.iter().last(),
        Some(libp2p::multiaddr::Protocol::P2p(_))
    ) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut inner = store.0.lock().unwrap();
    Store::cleanup(&mut inner);
    if inner.records.len() >= 2048 {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let id = secret();
    let management_token = secret();
    let code = loop {
        let c = code();
        if !inner
            .records
            .values()
            .any(|r| r.code_hash == hash(&normalize(&c)))
        {
            break c;
        }
    };
    let expires_at = (now() + input.code_ttl).min(input.invite.expires_at);
    inner.records.insert(
        id.clone(),
        Record {
            invite: input.invite,
            code_hash: hash(&normalize(&code)),
            code_expiry: expires_at,
            management: hash(&management_token),
            peer: None,
            claim_deadline: None,
            active: false,
            changed: Arc::new(Notify::new()),
        },
    );
    Ok(Json(Created {
        id,
        code,
        management_token,
        expires_at,
    }))
}
async fn claim(
    State(store): State<Store>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Claim>, Error> {
    let mut inner = store.0.lock().unwrap();
    Store::cleanup(&mut inner);
    let r = inner.records.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    authorized(&headers, r)?;
    Ok(Json(Claim {
        peer: r.peer.clone(),
        active: r.active,
    }))
}
async fn activate(
    State(store): State<Store>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, Error> {
    let mut inner = store.0.lock().unwrap();
    Store::cleanup(&mut inner);
    let r = inner.records.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;
    authorized(&headers, r)?;
    if r.peer.is_none() {
        return Err(StatusCode::CONFLICT);
    }
    r.active = true;
    r.changed.notify_waiters();
    Ok(StatusCode::NO_CONTENT)
}
async fn revoke(
    State(store): State<Store>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, Error> {
    let mut inner = store.0.lock().unwrap();
    let r = inner.records.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    authorized(&headers, r)?;
    r.changed.notify_waiters();
    inner.records.remove(&id);
    Ok(StatusCode::NO_CONTENT)
}
async fn redeem(
    State(store): State<Store>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(input): Json<Redeem>,
) -> Result<Json<Invite>, Error> {
    store.rate(addr.ip())?;
    let peer: libp2p::PeerId = input.peer.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let code = normalize(&input.code);
    if code.len() != 12 {
        return Err(StatusCode::NOT_FOUND);
    }
    let (id, changed) = {
        let mut inner = store.0.lock().unwrap();
        Store::cleanup(&mut inner);
        let (id, r) = inner
            .records
            .iter_mut()
            .find(|(_, r)| r.code_hash == hash(&code) && r.code_expiry > now() && r.peer.is_none())
            .ok_or(StatusCode::NOT_FOUND)?;
        r.peer = Some(peer.to_string());
        r.claim_deadline = Some(Instant::now() + Duration::from_secs(10));
        (id.clone(), r.changed.clone())
    };
    // Consume atomically before waiting. Lost responses require a fresh invitation.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let notified = changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let inner = store.0.lock().unwrap();
                let r = inner.records.get(&id).ok_or(StatusCode::GONE)?;
                if r.invite.expires_at <= now() {
                    return Err(StatusCode::GONE);
                }
                if r.active {
                    return Ok(Json(r.invite.clone()));
                }
            }
            notified.await;
        }
    })
    .await
    .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?
}
pub fn router(store: Store) -> Router {
    Router::new()
        .route("/health", get(|| async { StatusCode::NO_CONTENT }))
        .route("/v1/invites", post(create))
        .route("/v1/redeem", post(redeem))
        .route("/v1/invites/{id}", get(claim).delete(revoke))
        .route("/v1/invites/{id}/activate", post(activate))
        .layer(tower::limit::ConcurrencyLimitLayer::new(128))
        .layer(DefaultBodyLimit::max(8192))
        .with_state(store)
}
pub async fn serve(listener: tokio::net::TcpListener, store: Store) -> std::io::Result<()> {
    axum::serve(
        listener,
        router(store).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_tolerate_case_spacing_and_confusable_letters() {
        assert_eq!(normalize("ABCD-EF01-2345"), "ABCDEF012345");
        assert_eq!(normalize(" abcd ef0l 2345 "), "ABCDEF012345");
        assert_eq!(normalize("abcd-efOI-2345"), "ABCDEF012345");
        // Letters outside the alphabet that are not confusable are left to fail the lookup.
        assert_eq!(normalize("uuuu"), "UUUU");
    }

    fn record() -> Record {
        Record {
            invite: Invite {
                version: 1,
                address: String::new(),
                token: String::new(),
                expires_at: now() + 1000,
            },
            code_hash: [0; 32],
            code_expiry: now().saturating_sub(1),
            management: [0; 32],
            peer: Some("peer".to_string()),
            claim_deadline: Some(Instant::now() + Duration::from_secs(5)),
            active: false,
            changed: Arc::new(Notify::new()),
        }
    }

    #[test]
    fn pending_claim_gets_only_bounded_acknowledgement_grace() {
        let mut inner = Inner::default();
        inner.records.insert("claim".into(), record());
        // A code redeemed before expiry retains its short acknowledgement window.
        Store::cleanup(&mut inner);
        assert!(inner.records.contains_key("claim"));
        inner.records.get_mut("claim").unwrap().claim_deadline =
            Some(Instant::now() - Duration::from_secs(1));
        Store::cleanup(&mut inner);
        assert!(!inner.records.contains_key("claim"));
        let mut active = record();
        active.active = true;
        active.claim_deadline = Some(Instant::now() - Duration::from_secs(1));
        inner.records.insert("active".into(), active);
        Store::cleanup(&mut inner);
        assert!(inner.records.contains_key("active"));
    }
}
