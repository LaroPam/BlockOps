use anyhow::{ensure, Result};
use libp2p::PeerId;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub(crate) struct Access(Arc<Mutex<HashMap<[u8; 32], Grant>>>);
struct State {
    peer: Option<PeerId>,
    expires: u64,
    revoked: CancellationToken,
}
/// Revocation is irreversible; a copied capability cannot change its bound Noise peer.
#[derive(Clone)]
pub struct Grant(Arc<Mutex<State>>);
impl Grant {
    pub fn bind(&self, peer: PeerId) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        ensure!(
            !state.revoked.is_cancelled() && state.expires > crate::now(),
            "Grant expired or revoked"
        );
        ensure!(
            state.peer.is_none() || state.peer == Some(peer),
            "Grant is already bound to another peer"
        );
        state.peer = Some(peer);
        Ok(())
    }
    pub fn revoke(&self) {
        self.0.lock().unwrap().revoked.cancel();
    }
}
impl Access {
    pub fn issue(&self, token: [u8; 32], expires: u64) -> Result<Grant> {
        let mut grants = self.0.lock().unwrap();
        grants.retain(|_, g| {
            let s = g.0.lock().unwrap();
            !s.revoked.is_cancelled() && s.expires > crate::now()
        });
        ensure!(grants.len() < 32, "Maximum 32 member grants per host");
        let grant = Grant(Arc::new(Mutex::new(State {
            peer: None,
            expires,
            revoked: CancellationToken::new(),
        })));
        grants.insert(token, grant.clone());
        Ok(grant)
    }
    pub fn authorize(&self, token: &[u8; 32], peer: PeerId) -> Option<CancellationToken> {
        let grants = self.0.lock().unwrap();
        let grant = grants.get(token)?;
        let state = grant.0.lock().unwrap();
        if state.peer == Some(peer) && state.expires > crate::now() && !state.revoked.is_cancelled()
        {
            Some(state.revoked.clone())
        } else {
            None
        }
    }
}
