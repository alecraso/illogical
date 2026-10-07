//! Who may use `/mcp` without the owner's own identity (M16).
//!
//! - **Client tokens** (`arugula mcp token --name N`): for an MCP client
//!   that has no tailnet identity of its own (a tagged node, a client that
//!   can't use serve). Each has a name, which the log and "started by" use
//!   when the client doesn't name itself, and a scope: `full` (everything,
//!   like the owner) or `read` (the read-only tools only). The daemon keeps
//!   only each token's hash, in `mcp/tokens.json`; revoking one cuts it off
//!   at its next request.
//! - **Block tokens**: every agent block gets one, scoped to its tab (see
//!   `mcp::Scope::Block`). They're derived from a key kept in `mcp/key`
//!   (an HMAC of the block's id), so nothing is stored per block and a
//!   block gets the same token after a restart. A block's token stops
//!   working when the block closes.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use arugula_proto::PaneId;
use axum::{
    Json, Router,
    extract::{Path as UrlPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use hkdf::hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tracing::{info, warn};

use crate::{
    hosts::digest,
    server::App,
    store::{now_ms, private_dir, write_atomic},
};

const CLIENT_PREFIX: &str = "ilm_";
const BLOCK_PREFIX: &str = "ilb_";

/// What a client token may do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenScope {
    /// Everything the owner may.
    #[default]
    Full,
    /// The read-only tools.
    Read,
}

#[derive(Clone, Serialize, Deserialize)]
struct Saved {
    name: String,
    digest: String,
    #[serde(default)]
    scope: TokenScope,
    created_ms: u64,
    #[serde(default)]
    used_ms: Option<u64>,
}

/// A client token as listed (never the token itself).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenInfo {
    pub name: String,
    pub scope: TokenScope,
    pub created_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_ms: Option<u64>,
    /// Only when it was just made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// What a bearer token turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bearer {
    Client {
        name: String,
        scope: TokenScope,
    },
    /// An agent block's (still to check: is the block there).
    Block(PaneId),
}

pub struct Tokens {
    path: PathBuf,
    key: [u8; 32],
    saved: Mutex<Vec<Saved>>,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn random<const N: usize>() -> [u8; N] {
    crate::push::random()
}

impl Tokens {
    pub fn open(state_dir: &Path) -> Arc<Self> {
        let dir = state_dir.join("mcp");
        if let Err(e) = private_dir(&dir) {
            warn!(error = %e, "can't make the mcp directory");
        }
        let key_path = dir.join("key");
        let key = match std::fs::read(&key_path) {
            Ok(k) if k.len() == 32 => k.try_into().expect("32 bytes"),
            _ => {
                let k: [u8; 32] = random();
                if let Err(e) = write_atomic(&key_path, &k) {
                    warn!(error = %e, "can't save the mcp key; block tokens change at restart");
                }
                k
            }
        };
        let path = dir.join("tokens.json");
        let saved = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Arc::new(Self { path, key, saved: Mutex::new(saved) })
    }

    fn save(&self, s: &[Saved]) {
        let bytes = serde_json::to_vec_pretty(s).expect("serialize tokens");
        if let Err(e) = write_atomic(&self.path, &bytes) {
            warn!(error = %e, "can't save mcp tokens");
        }
    }

    /// A new client token; a token with the same name is replaced.
    pub fn mint(&self, name: &str, scope: TokenScope) -> Result<TokenInfo, String> {
        let name = name.trim();
        if name.is_empty() || name.len() > 64 || !name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.@".contains(c))
        {
            return Err("a token's name is up to 64 letters, digits and - _ . @".into());
        }
        let token = format!("{CLIENT_PREFIX}{}", hex(&random::<24>()));
        let created_ms = now_ms();
        let mut s = self.saved.lock().unwrap();
        s.retain(|t| t.name != name);
        s.push(Saved { name: name.to_owned(), digest: digest(&token), scope, created_ms, used_ms: None });
        self.save(&s);
        info!(name, ?scope, "mcp token minted");
        Ok(TokenInfo { name: name.to_owned(), scope, created_ms, used_ms: None, token: Some(token) })
    }

    pub fn list(&self) -> Vec<TokenInfo> {
        self.saved
            .lock()
            .unwrap()
            .iter()
            .map(|t| TokenInfo {
                name: t.name.clone(),
                scope: t.scope,
                created_ms: t.created_ms,
                used_ms: t.used_ms,
                token: None,
            })
            .collect()
    }

    pub fn revoke(&self, name: &str) -> bool {
        let mut s = self.saved.lock().unwrap();
        let before = s.len();
        s.retain(|t| t.name != name);
        let gone = s.len() != before;
        if gone {
            self.save(&s);
            info!(name, "mcp token revoked");
        }
        gone
    }

    fn mac(&self, block: PaneId) -> String {
        let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.key).expect("any key length");
        // Frozen (#504): agents started before an update carry tokens made
        // this way (`block_tokens_never_change`).
        m.update(format!("illogical block {block}").as_bytes());
        hex(&m.finalize().into_bytes())
    }

    /// The token an agent block's MCP server carries.
    pub fn block_token(&self, block: PaneId) -> String {
        format!("{BLOCK_PREFIX}{block}_{}", self.mac(block))
    }

    /// What a bearer token is, if it's one of ours.
    pub fn check(&self, token: &str) -> Option<Bearer> {
        if let Some(rest) = token.strip_prefix(BLOCK_PREFIX) {
            let (id, mac) = rest.split_once('_')?;
            let id: PaneId = id.parse().ok()?;
            let want = self.mac(id);
            // Compared without stopping at the first difference.
            let same = want.len() == mac.len() && want.bytes().zip(mac.bytes()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0;
            return same.then_some(Bearer::Block(id));
        }
        let want = digest(token);
        let mut s = self.saved.lock().unwrap();
        let t = s.iter_mut().find(|t| t.digest == want)?;
        // Noted at most once a minute, so a busy client doesn't write the
        // file on every call.
        let now = now_ms();
        let stale = t.used_ms.is_none_or(|u| now.saturating_sub(u) > 60_000);
        t.used_ms = Some(now);
        let found = Bearer::Client { name: t.name.clone(), scope: t.scope };
        if stale {
            let copy = s.clone();
            self.save(&copy);
        }
        Some(found)
    }
}

// ---------------------------------------------------------------- routes

type AppState = State<Arc<App>>;

/// The owner's: minting, listing and revoking client tokens.
pub fn api_routes() -> Router<Arc<App>> {
    Router::new().route("/api/mcp/tokens", get(list).post(mint)).route("/api/mcp/tokens/{name}", delete(revoke))
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

#[derive(Deserialize)]
struct MintRequest {
    name: String,
    #[serde(default)]
    scope: TokenScope,
}

async fn mint(State(app): AppState, Json(req): Json<MintRequest>) -> Response {
    match app.mcp.mint(&req.name, req.scope) {
        Ok(t) => Json(t).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

async fn list(State(app): AppState) -> Response {
    Json(app.mcp.list()).into_response()
}

async fn revoke(State(app): AppState, UrlPath(name): UrlPath<String>) -> Response {
    if app.mcp.revoke(&name) {
        Json(serde_json::json!({})).into_response()
    } else {
        error(StatusCode::NOT_FOUND, format!("no mcp token named {name}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An agent block's token from before an update still checks (#504):
    /// a rename must not change how it's made.
    #[test]
    fn block_tokens_never_change() {
        let t = Tokens { path: PathBuf::new(), key: [b'k'; 32], saved: Mutex::new(vec![]) };
        assert_eq!(t.mac(7), "fc69c1f978ef50871eeb01118749d040388f7ae2c231ac65206a7146184b68bd");
    }

    #[test]
    fn mints_checks_and_revokes() {
        let dir = std::env::temp_dir().join(format!("ilg-mcp-tokens-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let t = Tokens::open(&dir);
        let made = t.mint("laptop", TokenScope::Read).unwrap();
        let token = made.token.unwrap();
        assert!(token.starts_with(CLIENT_PREFIX));
        assert_eq!(t.check(&token), Some(Bearer::Client { name: "laptop".into(), scope: TokenScope::Read }));
        assert_eq!(t.list()[0].token, None, "a list never shows tokens");
        assert!(
            !std::fs::read_to_string(dir.join("mcp/tokens.json")).unwrap().contains(&token),
            "only its hash is kept"
        );
        assert!(t.mint("bad name", TokenScope::Full).is_err());

        // Block tokens: the same after a restart, and only for that block.
        let b = t.block_token(7);
        assert_eq!(t.check(&b), Some(Bearer::Block(7)));
        let again = Tokens::open(&dir);
        assert_eq!(again.block_token(7), b);
        assert_eq!(again.check(&b.replace("ilb_7_", "ilb_8_")), None);
        assert_eq!(again.check("ilb_7_00"), None);

        assert!(again.revoke("laptop"));
        assert_eq!(again.check(&token), None);
        assert!(!again.revoke("laptop"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
