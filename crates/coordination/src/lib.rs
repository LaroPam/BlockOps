pub mod client;
pub mod local_bridge;
pub mod service;
use blockops_tunnel::Invite;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub invite: Invite,
    pub code_ttl: u64,
}
#[derive(Serialize, Deserialize)]
pub struct Created {
    pub id: String,
    pub code: String,
    pub management_token: String,
    pub expires_at: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Redeem {
    pub code: String,
    pub peer: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Claim {
    pub peer: Option<String>,
    pub active: bool,
}
