//! What daemons and illogical control say to each other to enrol and to
//! route: joining, the account's and team's certificates, who gets in, how
//! the relay is dialled, and what `/control.json` says about control.
//!
//! One type per message, used by both sides, so a field renamed on one side
//! doesn't compile on the other. It isn't `illogical-proto`: that is what
//! browsers and clients speak with a daemon (and generates the web client's
//! types), and control shouldn't depend on it. This crate holds only these
//! messages and the certificates in them (`illogical-e2e`).
//!
//! Two rules for every type here:
//! - **The bytes on the wire don't change.** A field that was `null` stays
//!   `null` (no `skip_serializing_if`); one that was left out when empty
//!   still is. The tests round-trip messages recorded from both sides.
//! - **Be as lenient as either side was.** Deployed daemons and control
//!   don't upgrade together, so a field either one read with
//!   `#[serde(default)]` keeps it, and nothing here is `deny_unknown_fields`.
//!   A field neither side needs to find is not made required.
//!
//! Push, TURN, sandbox and forge messages are still built by hand (#491,
//! #450).

use serde::{Deserialize, Serialize};

pub mod join;
pub mod team;

pub use join::{JoinPoll, JoinProof, JoinRequest, JoinStarted, JoinTeam, PollQuery};
pub use team::{
    AccessList, Features, PeerCerts, PeersQuery, SharedTeamAnswer, TeamAnswer, TeamNone, TeamQuery, TeamsQuery,
    TrustAnswer,
};

/// What control says about itself, to browsers, the CLI and daemons.
pub const CONTROL_JSON: &str = "/control.json";
/// A daemon asks to join (`POST`), [`JoinRequest`], answered [`JoinStarted`].
pub const JOIN: &str = "/api/join";
/// The route a daemon polls for its approval (`GET /api/join/{code}`),
/// answered [`JoinPoll`].
pub const JOIN_POLL: &str = "/api/join/{code}";
/// The account's certificates (`GET`), [`TrustAnswer`].
pub const TRUST: &str = "/api/daemon/trust";
/// A daemon leaves (`POST`, no body).
pub const LEAVE: &str = "/api/daemon/leave";
/// A team machine's team (`GET`), [`TeamAnswer`].
pub const TEAM: &str = "/api/daemon/team";
/// People sessions were shared with (`GET`), [`PeerCerts`] by account.
pub const PEERS: &str = "/api/daemon/peers";
/// Teams sessions were shared with (`GET`), [`SharedTeamAnswer`] by team.
pub const TEAMS: &str = "/api/daemon/teams";
/// Who gets in (`POST`), [`AccessList`].
pub const ACCESS: &str = "/api/daemon/access";
/// The daemon's WebSocket to the relay (`GET`), [`DialQuery`].
pub const RELAY_DIAL: &str = "/api/relay/dial";

/// `GET /control.json`. Read leniently: a daemon needs only `daemon_auth`
/// and `guest_ssh` of it, and an older control is without some of the rest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ControlInfo {
    #[serde(default)]
    pub control: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub github: bool,
    /// Passkeys need a domain name: WebAuthn refuses IP addresses.
    #[serde(default)]
    pub passkeys: bool,
    /// The Web Push application server key.
    #[serde(default)]
    pub vapid: String,
    #[serde(default)]
    pub github_app: Option<String>,
    /// How daemons sign their requests here: 2 takes body hashes and nonces.
    #[serde(default)]
    pub daemon_auth: u64,
    /// The CLI joins with a code and signs its requests (M49).
    #[serde(default)]
    pub cli_join: u64,
    /// The ssh jump host for guests of daemons behind NAT (M65).
    #[serde(default)]
    pub guest_ssh: Option<GuestJump>,
}

/// Just the signature version from `/control.json`, for a daemon deciding
/// how to sign. Read apart from [`ControlInfo`] so that a field it can't
/// take never quietly drops the daemon back to v1 signatures.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct ControlAuth {
    #[serde(default)]
    pub daemon_auth: u64,
}

/// Just the jump host from `/control.json`, read apart from
/// [`ControlInfo`] so an unrelated malformed field doesn't lose it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ControlJump {
    #[serde(default)]
    pub guest_ssh: Option<GuestJump>,
}

/// Control's ssh jump host, for daemons making invites.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestJump {
    pub host: String,
    pub port: u16,
    pub known_hosts: String,
    #[serde(default)]
    pub fingerprint: String,
}

/// An answer with nothing in it: `{}`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {}

/// Control's 410 Gone (#330): this key was removed from its account.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RemovedAnswer {
    /// Control's words.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub removed: Option<RemovedAt>,
}

/// When a key was removed and, to the key's own holder only, by which
/// device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemovedAt {
    /// When it was removed (ms since the epoch).
    pub at: u64,
    /// The removing device's name; left out when control isn't telling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

/// `GET /api/relay/dial?urls=`: the daemon's direct URLs, for the directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DialQuery {
    /// JSON list of the daemon's direct URLs.
    #[serde(default)]
    pub urls: Option<String>,
}

impl DialQuery {
    /// What a daemon sends for its direct URLs.
    pub fn new(urls: &[String]) -> Self {
        // A list of strings always serializes.
        Self { urls: serde_json::to_string(urls).ok() }
    }

    /// The URLs, if it sent a list control can read.
    pub fn direct_urls(&self) -> Option<Vec<String>> {
        self.urls.as_deref().and_then(|u| serde_json::from_str(u).ok())
    }
}

#[cfg(test)]
mod tests;
