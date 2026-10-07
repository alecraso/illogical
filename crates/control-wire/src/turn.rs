//! ICE servers for huddles (M63): control's TURN credentials, or public STUN.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Cloudflare's public STUN, what a daemon uses without control's TURN.
pub const STUN: &str = "stun:stun.cloudflare.com:3478";

/// `GET /api/daemon/turn`, and the daemon's `GET /api/turn` that passes it
/// on to the web client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IceServers {
    /// As `RTCPeerConnection` takes them.
    #[serde(default)]
    pub ice_servers: Vec<IceServer>,
    /// How long the credentials work (seconds). Left out of a daemon's own
    /// STUN-only answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<u64>,
    /// Whether any of them is a TURN relay.
    #[serde(default)]
    pub turn: bool,
}

/// One `RTCIceServer`. Cloudflare makes these, so whatever else it adds is
/// kept and passed on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IceServer {
    pub urls: Urls,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

/// One URL or a list of them, as `RTCIceServer.urls` takes either.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Urls {
    One(String),
    Many(Vec<String>),
}

impl IceServers {
    /// What a daemon uses when it isn't joined to control or control can't
    /// be reached: public STUN only.
    pub fn stun_only() -> Self {
        Self { ice_servers: vec![IceServer::stun()], ttl: None, turn: false }
    }

    /// What control answers without a TURN key: the same STUN, for `ttl`
    /// seconds.
    pub fn stun(ttl: u64) -> Self {
        Self { ttl: Some(ttl), ..Self::stun_only() }
    }

    /// What control answers with Cloudflare's servers.
    pub fn turn(ice_servers: Vec<IceServer>, ttl: u64) -> Self {
        Self { ice_servers, ttl: Some(ttl), turn: true }
    }
}

impl IceServer {
    fn stun() -> Self {
        Self { urls: Urls::Many(vec![STUN.to_owned()]), username: None, credential: None, other: Map::new() }
    }
}
