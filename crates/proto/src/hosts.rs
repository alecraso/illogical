//! Federation (M4): the home daemon's list of other daemons ("hosts").
//! Clients fetch it from the daemon they were loaded from and then connect
//! to each host directly; nothing is relayed. Routes are in [`crate::api`].

use serde::{Deserialize, Serialize};

/// How a client reaches a host. Only the tailnet so far (M4b adds provider
/// tunnels). A loopback URL works too, which is what the tests use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    #[default]
    Tailnet,
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
