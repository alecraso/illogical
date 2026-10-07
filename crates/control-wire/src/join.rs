//! Enrolment: a daemon asks to join, shows the code, and polls until a
//! signed-in device approves it.

use arugula_e2e::{Cert, Revocation, Trust};
use serde::{Deserialize, Serialize, Serializer, ser::SerializeStruct};

/// `POST /api/join`. The CLI joins the same way, with only `cert` and
/// `proof`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    pub cert: Cert,
    #[serde(default)]
    pub urls: Vec<String>,
    /// A machine that belongs to a team (M19), not a person.
    #[serde(default)]
    pub team: Option<String>,
    /// A hosted sandbox's one-time ticket (M20).
    #[serde(default)]
    pub ticket: Option<String>,
    /// What the daemon understands, comma-separated (older ones send none).
    #[serde(default)]
    pub features: String,
    /// Its signature with the key it asks with (0.17 and newer).
    #[serde(default)]
    pub proof: Option<JoinProof>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinProof {
    pub ms: u64,
    pub sig: String,
}

/// Control's answer to a join: the code to show, and a token to poll with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinStarted {
    pub code: String,
    pub poll: String,
    pub expires_in_secs: u64,
    /// The team `--team` named, by name.
    #[serde(default)]
    pub team_name: Option<String>,
}

/// `GET /api/join/{code}?poll=`: where a join stands. Waiting is just
/// `approved: false`, turned down adds `rejected`, and approved is the rest;
/// each is serialized as control always has (see [`JoinPoll::serialize`]).
#[derive(Debug, Clone, Deserialize)]
pub struct JoinPoll {
    pub approved: bool,
    /// Turned down, on this device (#100).
    #[serde(default)]
    pub rejected: Option<String>,
    pub cert: Option<Cert>,
    pub trust: Option<Trust>,
    #[serde(default)]
    pub team: Option<JoinTeam>,
    #[serde(default)]
    pub certs: Vec<Cert>,
    #[serde(default)]
    pub revocations: Vec<Revocation>,
}

/// The token the daemon polls with: `GET /api/join/{code}?poll=`.
#[derive(Debug, Clone, Deserialize)]
pub struct PollQuery {
    pub poll: String,
}

impl JoinPoll {
    /// The path to poll: [`crate::JOIN_POLL`] for `code`, with its token.
    pub fn path(code: &str, poll: &str) -> String {
        format!("{}?poll={poll}", crate::JOIN_POLL.replace("{code}", code))
    }

    /// Nobody has approved or turned it down yet.
    pub fn waiting() -> Self {
        Self {
            approved: false,
            rejected: None,
            cert: None,
            trust: None,
            team: None,
            certs: vec![],
            revocations: vec![],
        }
    }

    /// Turned down on `device`.
    pub fn turned_down(device: String) -> Self {
        Self { rejected: Some(device), ..Self::waiting() }
    }
}

impl Serialize for JoinPoll {
    /// Control has sent `{approved: false}` while waiting, that and
    /// `rejected` once turned down, and everything (a `null` team and trust
    /// included) once approved.
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if !self.approved {
            let n = 1 + usize::from(self.rejected.is_some());
            let mut o = s.serialize_struct("JoinPoll", n)?;
            o.serialize_field("approved", &self.approved)?;
            if let Some(on) = &self.rejected {
                o.serialize_field("rejected", on)?;
            }
            return o.end();
        }
        let mut o = s.serialize_struct("JoinPoll", 6)?;
        o.serialize_field("approved", &self.approved)?;
        o.serialize_field("cert", &self.cert)?;
        o.serialize_field("trust", &self.trust)?;
        o.serialize_field("certs", &self.certs)?;
        o.serialize_field("revocations", &self.revocations)?;
        o.serialize_field("team", &self.team)?;
        o.end()
    }
}

/// The team an approved machine joins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinTeam {
    pub team: String,
    pub founder: String,
    pub founder_root: String,
    #[serde(default)]
    pub name: String,
    /// The approving device's signature over the team pin's join body
    /// (`TeamPin::join_body`).
    #[serde(default)]
    pub sig: Option<String>,
}
