//! Federation (M4): the home daemon's list of other daemons ("hosts").
//! Clients fetch it from the daemon they were loaded from and then connect
//! to each host directly; nothing is relayed. Routes are in [`crate::api`].

use serde::{Deserialize, Serialize};

/// How a client reaches a host. A loopback URL works for `tailnet` too,
/// which is what the tests use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// Straight to its URLs.
    #[default]
    Tailnet,
    /// The host dials the home daemon (M4c) and is reached through it, at
    /// `<home>/h/<name>/ws` and `<home>/h/<name>/api/...`. It has no URLs of
    /// its own.
    DialOut,
}

/// Another daemon, in the home daemon's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    pub name: String,
    /// Where its page, API and WebSocket are (`https://box.tailnet.ts.net`),
    /// best first.
    pub urls: Vec<String>,
    #[serde(default)]
    pub transport: Transport,
    #[serde(default)]
    pub added_ms: u64,
    /// When the home daemon last reached it.
    #[serde(default)]
    pub last_seen_ms: Option<u64>,
}

/// `GET /api/hosts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostList {
    /// The name of the daemon answering, which isn't in `hosts`.
    pub this: String,
    pub hosts: Vec<Host>,
}

/// `POST /api/hosts`, and what a sandbox sends to `join`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddHost {
    pub name: String,
    pub urls: Vec<String>,
    #[serde(default)]
    pub transport: Transport,
}

/// `GET /api/host`: who this daemon is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    pub name: String,
    pub version: String,
}

/// `POST /api/hosts/invite`: a one-time token that lets a sandbox add itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    pub token: String,
    pub expires_ms: u64,
}

/// `POST /api/hosts/join`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequest {
    pub token: String,
    pub host: AddHost,
}

/// What `join` answers: the entry, and the login the home daemon lets in,
/// which the joining daemon should let in too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Joined {
    pub host: Host,
    pub owner: Option<String>,
    /// For a `dial_out` host: its per-host token (see [`HostToken`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// `POST /api/hosts/NAME/token`: a per-host token, minted by the home
/// daemon for a host without tailnet identity. It lets that host, and only
/// it, dial in (`illogicald --peer … --peer-token-file …`) and push its log
/// segments (`--sync`). The home daemon keeps only its hash; minting another
/// replaces it, and `DELETE /api/hosts/NAME/token` revokes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostToken {
    pub name: String,
    pub token: String,
}
