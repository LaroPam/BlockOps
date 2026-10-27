use crate::{Claim, Create, Created, Redeem};
use anyhow::{ensure, Context, Result};
use blockops_tunnel::{Invite, Session};
use serde::Serialize;
use std::{net::SocketAddr, time::Duration};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Client {
    base: reqwest::Url,
    http: reqwest::Client,
}
impl Client {
    pub fn new(base: &str) -> Result<Self> {
        let base = reqwest::Url::parse(base)?;
        ensure!(
            base.username().is_empty()
                && base.password().is_none()
                && base.query().is_none()
                && base.fragment().is_none()
                && base.path() == "/",
            "Coordinator must be an origin URL without credentials/path/query"
        );
        ensure!(
            base.scheme() == "https"
                || base.scheme() == "http"
                    && matches!(base.host_str(), Some("127.0.0.1" | "[::1]")),
            "Coordinator requires HTTPS; HTTP is allowed only for literal loopback tests"
        );
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self { base, http })
    }
    fn url(&self, path: &str) -> reqwest::Url {
        self.base.join(path).expect("static API path")
    }
    pub async fn publish(&self, session: &Session, ttl: u64) -> Result<HostedInvite> {
        let (invite, grant) = session.issue_invite()?;
        let result = async {
            Ok::<_, anyhow::Error>(
                self.http
                    .post(self.url("v1/invites"))
                    .json(&Create {
                        invite,
                        code_ttl: ttl,
                    })
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<Created>()
                    .await?,
            )
        }
        .await;
        let created = match result {
            Ok(c) => c,
            Err(e) => {
                grant.revoke();
                return Err(e);
            }
        };
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let worker_grant = grant.clone();
        let client = self.clone();
        let id = created.id.clone();
        let token = created.management_token;
        let initial = CodeStatus {
            id: id.clone(),
            code: created.code,
            expires_at: created.expires_at,
            phase: "waiting".into(),
            peer: None,
            error: None,
        };
        let (tx, status) = watch::channel(initial);
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            let result:Result<()>=async{
                loop {
                    tokio::select!{biased; _=stop.cancelled()=>break, _=interval.tick()=>{}}
                    let response=tokio::select!{biased; _=stop.cancelled()=>break, r=client.http.get(client.url(&format!("v1/invites/{id}"))).bearer_auth(&token).timeout(Duration::from_secs(3)).send()=>r?};
                    if response.status()==reqwest::StatusCode::NOT_FOUND {tx.send_modify(|s|s.phase="revoked".into());break;}
                    let claim=response.error_for_status()?.json::<Claim>().await?;
                    if let Some(peer)=claim.peer {
                        worker_grant.bind(peer.parse().context("Invalid guest identity")?)?;
                        if !claim.active { client.http.post(client.url(&format!("v1/invites/{id}/activate"))).bearer_auth(&token).timeout(Duration::from_secs(3)).send().await?.error_for_status()?; }
                        tx.send_modify(|s|{s.phase="active".into();s.peer=Some(peer);});
                    }
                }Ok(())
            }.await;
            worker_grant.revoke();
            if let Err(e) = result {
                tx.send_modify(|s| {
                    s.phase = "failed".into();
                    s.error = Some(e.to_string());
                });
            } else {
                tx.send_modify(|s| s.phase = "revoked".into());
            }
            // Best effort remote cleanup. Local revocation has already taken effect.
            let _ = client
                .http
                .delete(client.url(&format!("v1/invites/{id}")))
                .bearer_auth(&token)
                .timeout(Duration::from_secs(3))
                .send()
                .await;
        });
        Ok(HostedInvite {
            status,
            grant,
            cancel,
            task: Some(task),
        })
    }
    pub async fn redeem(&self, code: &str, peer: libp2p::PeerId) -> Result<Invite> {
        ensure!(code.len() <= 32, "Invalid invitation code");
        Ok(self
            .http
            .post(self.url("v1/redeem"))
            .json(&Redeem {
                code: code.into(),
                peer: peer.to_string(),
            })
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
    pub async fn join(&self, code: &str, bind: SocketAddr) -> Result<Session> {
        ensure!(bind.ip().is_loopback(), "Guest listener must bind loopback");
        // Reserve the actual socket before consuming the one-use code; holding it across
        // the handshake also avoids a check-then-bind race with other local processes.
        let listener = tokio::net::TcpListener::bind(bind).await?;
        let key = libp2p::identity::Keypair::generate_ed25519();
        let invite = self.redeem(code, key.public().to_peer_id()).await?;
        blockops_tunnel::join_with_listener(invite, listener, key).await
    }
}
#[derive(Clone, Serialize)]
pub struct CodeStatus {
    pub id: String,
    pub code: String,
    pub expires_at: u64,
    pub phase: String,
    pub peer: Option<String>,
    pub error: Option<String>,
}
pub struct HostedInvite {
    pub status: watch::Receiver<CodeStatus>,
    grant: blockops_tunnel::access::Grant,
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for HostedInvite {
    fn drop(&mut self) {
        self.grant.revoke();
        self.cancel.cancel();
    }
}
impl HostedInvite {
    pub async fn revoke(mut self) {
        self.grant.revoke();
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
