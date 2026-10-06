//! What a daemon fetches from control to keep checking who may reach it: the
//! account's certificates, its team's rosters, the people and teams sessions
//! were shared with. And what it tells control: who gets in.

use std::collections::BTreeMap;

use illogical_e2e::{
    Cert, Revocation, Trust,
    team::{AccountCerts, Move, Roster, TeamPin},
};
use serde::{Deserialize, Serialize};

/// `GET /api/daemon/trust`: the account's certificates, to evaluate against
/// the root the daemon pinned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustAnswer {
    /// Not read by daemons: they have the root they pinned.
    #[serde(default)]
    pub trust: Option<Trust>,
    pub certs: Vec<Cert>,
    pub revocations: Vec<Revocation>,
    /// Its last move (#100), for the daemon to check and take.
    #[serde(default)]
    pub moved: Option<Move>,
}

/// `GET /api/daemon/team`: a team machine's team, rosters after the version
/// it has, every member's certificates, and whether it's locked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamAnswer {
    #[serde(default)]
    pub team: Option<TeamPin>,
    pub locked: bool,
    pub rosters: Vec<Roster>,
    pub certs: AccountCerts,
    #[serde(default)]
    pub names: BTreeMap<String, String>,
}

/// What control answers a machine that isn't in a team: `{"team": null}`.
/// A daemon only asks once it has pinned one, and reads this as it always
/// has, as an answer [`TeamAnswer`] can't parse.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TeamNone {
    pub team: Option<TeamPin>,
}

/// `GET /api/daemon/teams`: one of these per team, by id, for each team a
/// session was shared with whose owner is in it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedTeamAnswer {
    #[serde(default)]
    pub team: Option<TeamPin>,
    #[serde(default)]
    pub name: String,
    pub locked: bool,
    pub rosters: Vec<Roster>,
    pub certs: AccountCerts,
    #[serde(default)]
    pub names: BTreeMap<String, String>,
}

/// One account's certificates, from `GET /api/daemon/peers` (by account),
/// or as saved in `control.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerCerts {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub certs: Vec<Cert>,
    #[serde(default)]
    pub revocations: Vec<Revocation>,
}

/// `POST /api/daemon/access`: which accounts a daemon lets in. The daemon
/// decides for itself; control routes only those it would anyway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessList {
    pub accounts: Vec<String>,
    /// Until when (ms) read-only links may reach it through the relay.
    pub links_until: Option<u64>,
}

/// What a daemon says it understands, comma-separated (older ones send
/// none): `GET /api/daemon/trust?features=`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Features {
    #[serde(default)]
    pub features: String,
}

impl Features {
    pub fn path(features: &str) -> String {
        format!("{}?features={features}", crate::TRUST)
    }
}

/// `GET /api/daemon/team?since=&features=`.
#[derive(Debug, Clone, Deserialize)]
pub struct TeamQuery {
    /// The roster version the daemon has.
    #[serde(default)]
    pub since: u64,
    #[serde(default)]
    pub features: String,
}

impl TeamQuery {
    pub fn path(since: u64, features: &str) -> String {
        format!("{}?since={since}&features={features}", crate::TEAM)
    }
}

/// `GET /api/daemon/teams?ids=&features=`.
#[derive(Debug, Clone, Deserialize)]
pub struct TeamsQuery {
    /// Team ids, comma-separated.
    pub ids: String,
    #[serde(default)]
    pub features: String,
}

impl TeamsQuery {
    pub fn path(ids: &[&str], features: &str) -> String {
        format!("{}?ids={}&features={features}", crate::TEAMS, ids.join(","))
    }
}

/// `GET /api/daemon/peers?accounts=`.
#[derive(Debug, Clone, Deserialize)]
pub struct PeersQuery {
    /// Account ids, comma-separated.
    pub accounts: String,
}

impl PeersQuery {
    pub fn path(accounts: &[String]) -> String {
        format!("{}?accounts={}", crate::PEERS, accounts.join(","))
    }
}
