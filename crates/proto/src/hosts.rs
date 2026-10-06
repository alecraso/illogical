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
    /// Through the home daemon's tunnel (`/tunnel/<name>/…`), which reaches
    /// the host's port through its sandbox provider (M4b): for a resident
    /// daemon in a sandbox that sleeps. Its URLs, if any, are tailnet ones
    /// to upgrade to once it's awake.
    Provider,
    /// Over ssh, from each client (M51): `illogical --host NAME` runs the
    /// system `ssh` to [`Host::ssh`]. No URLs; the web and the phone can't
    /// reach it, only a terminal can.
    Ssh,
}

/// Where a resident daemon lives: a sandbox, and its daemon's port there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRef {
    /// Which provider: `wisp`, `sprites`.
    pub provider: String,
    /// The provider's name for the sandbox.
    pub sandbox: String,
    pub port: u16,
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
    /// For [`Transport::Provider`]: where it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderRef>,
    /// For [`Transport::Ssh`]: ssh's destination (`user@box`, or a Host
    /// from the client's `~/.ssh/config`). Nothing else is kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<String>,
    #[serde(default)]
    pub added_ms: u64,
    /// When the home daemon last reached it (for a provider host: last
    /// saw its sandbox running).
    #[serde(default)]
    pub last_seen_ms: Option<u64>,
    /// A provider host's sandbox state as its provider last said
    /// (`running`, `warm`, `cold`, `gone`): asked of the provider, never
    /// by connecting, so asking doesn't wake it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
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
    /// For [`Transport::Ssh`]: its destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<String>,
}

/// `GET /api/host`: who this daemon is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields))]
pub struct HostInfo {
    pub name: String,
    pub version: String,
    /// The app↔daemon protocol it speaks ([`crate::PROTOCOL`], #390).
    /// Absent from daemons older than the number, which speak
    /// [`crate::PROTOCOL_BASELINE`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u32>,
    /// Where `tailscale serve` puts the app, when tailscaled told us this
    /// node's name (#109): `https://NAME.TAILNET.ts.net`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailnet_url: Option<String>,
    /// The owner has come in over the tailnet since the daemon started:
    /// serve works (#110).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>"))]
    pub tailnet_seen: bool,
    /// The control this daemon joined, if any (#110).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    /// The team it joined as, if one (#110).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// This machine is the account's Fountain runner (M45b: it has the
    /// `fountain-runner` unit): what was last read of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fountain_runner: Option<FountainRunnerInfo>,
    /// What this machine is set up for (#171, #180): the menus offer only
    /// these, or say how to turn them on. Absent from older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<HostFeatures>,
}

/// The `control_state` key of `GET /api/host`, sent beside [`HostInfo`]'s
/// keys to the machine's owner only (#325). Read it from the answer's JSON:
/// older daemons, and anyone else, leave it out.
pub const CONTROL_STATE_KEY: &str = "control_state";

/// A machine's standing with illogical control (#325): whether and where
/// it's joined, whether control is reachable, or that control dropped it.
/// The page, the tray and `illogical status` all show this.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ControlState {
    /// `not_joined`, `joined` or `dropped`.
    #[cfg_attr(feature = "ts", ts(type = r#""not_joined" | "joined" | "dropped""#))]
    pub state: String,
    /// The control it's (or was) joined to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub url: Option<String>,
    /// `account` or `team`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = r#""account" | "team""#))]
    pub kind: Option<String>,
    /// The team's name, or the account's login (empty until control says).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub name: Option<String>,
    /// Joined: reachable through control now (its relay socket is up; for a
    /// sandbox behind a provider's proxy, the last refresh worked).
    #[serde(default)]
    pub connected: bool,
    /// When control last answered (ms since the epoch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub seen_ms: Option<u64>,
    /// Joined: what last went wrong talking to control, until it works again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub error: Option<String>,
    /// Dropped: what control said ("not an enrolled daemon (left, or
    /// revoked?)", or that its key was removed, #330).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub said: Option<String>,
    /// Dropped: when this machine first heard it (ms since the epoch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub dropped_ms: Option<u64>,
    /// Dropped: a join waiting for approval, its code (a machine whose key
    /// was removed asks to join again with a new key by itself, #330; or
    /// someone started one), and where a signed-in device approves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub approve: Option<String>,
}

impl ControlState {
    pub fn is_dropped(&self) -> bool {
        self.state == "dropped"
    }

    pub fn is_joined(&self) -> bool {
        self.state == "joined"
    }

    /// Where it is, in words: "the team arugula on control.example",
    /// "lex00's account on …", or "an account on …".
    pub fn place(&self) -> String {
        let host = self.url.as_deref().map(|u| u.trim_start_matches("https://").trim_start_matches("http://"));
        let host = host.unwrap_or("control").trim_end_matches('/');
        let name = self.name.as_deref().unwrap_or_default();
        match (self.kind.as_deref(), name) {
            (Some("team"), n) if !n.is_empty() => format!("the team {n} on {host}"),
            (Some("team"), _) => format!("a team on {host}"),
            (_, "") => format!("an account on {host}"),
            (_, n) => format!("{n}'s account on {host}"),
        }
    }

    /// One line: "In the team arugula on control.example: connected",
    /// "Not joined to illogical control", "Dropped by control: …". What the
    /// tray's control line and `illogical status` say.
    pub fn line(&self) -> String {
        match self.state.as_str() {
            "joined" if self.connected => format!("In {}: connected", self.place()),
            "joined" => match &self.error {
                Some(e) => format!("In {}: not connected ({e})", self.place()),
                None => format!("In {}: connecting", self.place()),
            },
            "dropped" => format!(
                "Dropped by control: no longer in {} (control says: {})",
                self.place(),
                self.said.as_deref().unwrap_or("it doesn't know this machine")
            ),
            _ => "Not joined to illogical control".into(),
        }
    }

    /// From a `GET /api/host` answer: `None` from an older daemon, or for
    /// someone who isn't the owner.
    pub fn of_host(v: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(v.get(CONTROL_STATE_KEY)?.clone()).ok()
    }
}

#[cfg(test)]
mod control_state_tests {
    use super::*;

    #[test]
    fn the_line_says_where_and_how() {
        let mut s = ControlState {
            state: "joined".into(),
            url: Some("https://control.example/".into()),
            kind: Some("team".into()),
            name: Some("arugula".into()),
            connected: true,
            ..Default::default()
        };
        assert_eq!(s.line(), "In the team arugula on control.example: connected");
        s.connected = false;
        assert_eq!(s.line(), "In the team arugula on control.example: connecting");
        s.error = Some("can't reach control's relay".into());
        assert_eq!(s.line(), "In the team arugula on control.example: not connected (can't reach control's relay)");
        s.kind = Some("account".into());
        s.name = Some(String::new());
        assert_eq!(s.place(), "an account on control.example");
        let d = ControlState { state: "dropped".into(), said: Some("left".into()), ..s };
        assert_eq!(d.line(), "Dropped by control: no longer in an account on control.example (control says: left)");
        assert_eq!(ControlState::default().line(), "Not joined to illogical control");
        let v = serde_json::json!({ "name": "a", "version": "1", CONTROL_STATE_KEY: d });
        assert_eq!(ControlState::of_host(&v), Some(d));
        assert_eq!(ControlState::of_host(&serde_json::json!({ "name": "a" })), None);
    }
}

/// The file in a machine's state dir that turns on what a stranger doesn't
/// get: huddles, chat, Fountain, studio, VMs, guest ssh and the swarm's extra
/// views. Present means on, whatever it holds.
pub const LABS_FILE: &str = "labs";

/// Whether `state_dir` has the `labs` file. A `stat` on every call, never
/// cached, so adding or removing the file takes effect with no restart. The
/// daemon, the CLI and the page (through `HostFeatures::labs`) all read it
/// through this one name.
pub fn labs(state_dir: &std::path::Path) -> bool {
    state_dir.join(LABS_FILE).exists()
}

/// The optional parts of a machine, as `GET /api/host` reports them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HostFeatures {
    /// The machine has a `labs` file in its state dir (see [`labs`]): what a
    /// stranger doesn't get is on. Absent from older daemons, and pages
    /// treat that as off.
    #[serde(default)]
    pub labs: bool,
    /// Browser blocks on ports and editor blocks: block sites are on
    /// (`--block-listen`).
    pub blocks: bool,
    /// VM tabs and panes and *Sandboxes…*: a sandbox provider (wisp).
    pub vms: bool,
    /// A Fountain login here: `FOUNTAIN_API_KEY`, or the CLI's
    /// credentials file.
    pub fountain: bool,
    /// A studio is linked (`illogical studio login`).
    pub studio: bool,
    /// Threads on panes and sessions: with `labs`. Older daemons leave it
    /// out, and pages hide threads there.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub threads: bool,
    /// Huddles on sessions, likewise.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub calls: bool,
}

/// A machine's Fountain runner, for its line in the machine panel and the
/// swarm (M45b). Read in the background, never on the request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields))]
pub struct FountainRunnerInfo {
    /// Its name on Fountain (the unit's `--name`).
    pub name: String,
    /// What Fountain says; `None` until read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online: Option<bool>,
    /// The runner's `fountain` version, as Fountain has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `systemctl is-active fountain-runner`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_active: Option<bool>,
    /// How many sandboxes it holds (when a runner view counted them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandboxes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_ms: Option<u64>,
    /// What wants the owner (the runner view's attention), if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
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

/// A sandbox provider's capabilities, as far as a client cares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub name: String,
    /// Output a detached shell keeps for reattaching, in bytes: a shell
    /// opened without a daemon has no more history than this while
    /// nothing follows it.
    pub exec_replay: u64,
    /// Whether a daemon can be made resident there (files and services).
    pub resident: bool,
}

/// `GET /api/sandboxes`: the provider's sandboxes, on the home daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxList {
    pub provider: ProviderInfo,
    pub sandboxes: Vec<SandboxInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxInfo {
    pub name: String,
    pub status: String,
    /// The host in the list whose daemon lives there, if one does.
    #[serde(default)]
    pub host: Option<String>,
}

/// `POST /api/sandboxes/{name}/promote`: copy the static daemon in and keep
/// it running there as a provider service, then add it to the host list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromoteRequest {
    /// Its name in the host list [default: the sandbox's].
    #[serde(default)]
    pub host: Option<String>,
    /// The daemon's port inside the sandbox [default: 7681].
    #[serde(default)]
    pub port: Option<u16>,
}
