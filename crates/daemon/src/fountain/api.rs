//! Fountain's HTTP API, read-only (M43): the account's agents, its
//! environments' names, its self-hosted runners and its sandboxes.
//!
//! Shapes are Fountain's OpenAPI (`<base_url>/api/openapi.json`, CLI
//! v0.21.0): every list is `{data: [...]}`, an agent by id is `{data:
//! {...}}`. Requests carry the person's key as a bearer token (memory only,
//! never logged) and Arugula's own User-Agent: managoat.com refuses some
//! default ones (S24 saw Python's get a 403).
//!
//! The types keep what Arugula reads and pass the rest through
//! (`extra`), so MCP's `fountain_agent` returns the whole recipe. Fountain never
//! returns a secret's value: an MCP server's header or env holds a
//! `${VAR}` reference, or a value someone typed in.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

/// What went wrong talking to Fountain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// 401 or 403: the key is wrong, expired, or lacks the scope.
    Denied(String),
    NotFound(String),
    Http(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Denied(e) => write!(f, "Fountain refused the key: {e}"),
            Error::NotFound(e) => write!(f, "not found on Fountain: {e}"),
            Error::Http(e) => write!(f, "{e}"),
        }
    }
}

/// One skill: inline (`{name, content}`, a whole SKILL.md) or from GitHub
/// (`{source, ref?, name?}`: one skill of the repository, or all of them).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Skill {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// `owner/repo` on GitHub.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Skill {
    /// How a card names it: its name, else every skill of its repository
    /// (`example-org/agent-skills/*`).
    pub fn label(&self) -> String {
        match (&self.name, &self.source) {
            (Some(n), _) => n.clone(),
            (None, Some(s)) => format!("{s}/*"),
            (None, None) => "?".into(),
        }
    }

    #[allow(dead_code)] // M44 writes inline skills out.
    pub fn inline(&self) -> bool {
        self.content.is_some()
    }
}

/// One MCP server, as Claude Code's config writes it: `http`/`sse` with a
/// `url` and `headers`, or `stdio` with a `command`, `args` and `env`. Any
/// string may hold `${VAR}`s.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct McpServer {
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, deserialize_with = "string_map", skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, deserialize_with = "string_list", skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, deserialize_with = "string_map", skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// What stands in for a value typed in literally (not a `${VAR}`
/// reference) when a recipe is shown.
pub const REDACTED: &str = "<redacted>";

impl McpServer {
    /// As it may be shown: a header or env value that isn't (or doesn't
    /// hold) a `${VAR}` reference may be a secret someone typed in, and
    /// becomes [`REDACTED`].
    pub fn redacted(&self) -> Self {
        let hide = |m: &BTreeMap<String, String>| {
            m.iter().map(|(k, v)| (k.clone(), if is_reference(v) { v.clone() } else { REDACTED.to_owned() })).collect()
        };
        Self { headers: hide(&self.headers), env: hide(&self.env), ..self.clone() }
    }
}

/// Whether a value holds a `${VAR}` reference.
pub fn is_reference(v: &str) -> bool {
    v.find("${").is_some_and(|at| v[at..].contains('}'))
}

/// An agent: a whole recipe (`GET /api/agents`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// `provider/model` (`anthropic/claude-opus-5`); empty when it has
    /// none (an acp agent's).
    #[serde(default, deserialize_with = "null_default")]
    pub model: String,
    /// claude, codex, gemini, opencode or acp.
    #[serde(default, deserialize_with = "null_default")]
    pub runtime: String,
    /// The system prompt.
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub skills: Vec<Skill>,
    #[serde(default, deserialize_with = "null_default")]
    pub mcp_servers: BTreeMap<String, McpServer>,
    #[serde(default, deserialize_with = "null_default")]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub environment_id: Option<String>,
    /// null: the instance's default (sprites on hosted Fountain).
    #[serde(default)]
    pub sandbox_provider: Option<String>,
    /// ephemeral or persistent.
    #[serde(default)]
    pub sandbox_mode: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub conversation_count: u64,
    #[serde(default)]
    pub inserted_at: Option<String>,
    /// RFC 3339; M44 caches a worn bundle by it.
    #[serde(default)]
    pub updated_at: Option<String>,
    /// The rest (permission policy, session config, ids of what it may
    /// attach), passed through to MCP's `fountain_agent`.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Agent {
    /// As it may be shown ([`McpServer::redacted`] for each server).
    pub fn redacted(&self) -> Self {
        let mcp_servers = self.mcp_servers.iter().map(|(k, v)| (k.clone(), v.redacted())).collect();
        Self { mcp_servers, ..self.clone() }
    }
}

fn null_default<'de, D: serde::Deserializer<'de>, T: Default + Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// A map of strings: null is empty, a null value is left out, a number or
/// boolean is its text.
fn string_map<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<String, String>, D::Error> {
    let m = Option::<Map<String, Value>>::deserialize(d)?.unwrap_or_default();
    Ok(m.into_iter().filter_map(|(k, v)| text(v).map(|v| (k, v))).collect())
}

/// A list of strings, as [`string_map`].
fn string_list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let l = Option::<Vec<Value>>::deserialize(d)?.unwrap_or_default();
    Ok(l.into_iter().filter_map(text).collect())
}

fn text(v: Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s),
        v => Some(v.to_string()),
    }
}

/// An environment, by its name only: the rest (its variables, setup) is
/// Fountain's business.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    pub id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub name: String,
}

/// A self-hosted runner (`GET /api/runners`; M45 shows them).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Runner {
    pub id: String,
    /// The `--name` it connected with.
    #[serde(default, deserialize_with = "null_default")]
    pub name: String,
    #[serde(default, deserialize_with = "null_default")]
    pub online: bool,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    /// Its CLI's version.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    /// The directory on its machine that holds its sandboxes.
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub last_seen_at: Option<String>,
    #[serde(default)]
    pub connected_at: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

/// Where a runner sandbox lives (`provider: runner`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SandboxRunner {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub online: bool,
    /// Its directory on the machine (`<root>/<name>`).
    #[serde(default)]
    pub path: Option<String>,
}

/// A conversation on a sandbox, newest first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SandboxConversation {
    pub id: String,
    /// pending, running, idle, failed or terminated.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub mid_turn: bool,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub inserted_at: Option<String>,
}

/// A sandbox and its conversations (`GET /api/sandboxes`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sandbox {
    pub id: String,
    /// Its name on the provider (`runner-<runner id>-<short>` on a runner).
    pub sprite_name: String,
    /// pending, starting, ready, suspended, terminated or failed.
    #[serde(default)]
    pub status: String,
    /// sprites, e2b, daytona or runner.
    #[serde(default)]
    pub provider: Option<String>,
    /// ephemeral or persistent.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub vault_id: Option<String>,
    #[serde(default)]
    pub runner: Option<SandboxRunner>,
    #[serde(default, deserialize_with = "null_default")]
    pub conversations: Vec<SandboxConversation>,
    #[serde(default)]
    pub inserted_at: Option<String>,
    #[serde(default)]
    pub last_resumed_at: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Deserialize)]
struct Data<T> {
    data: T,
}

/// A list as read: the rows that parsed, and how many didn't (one bad row
/// never empties the list).
#[derive(Debug, Clone, PartialEq)]
pub struct Listing<T> {
    pub items: Vec<T>,
    pub unreadable: usize,
}

impl<T> Default for Listing<T> {
    fn default() -> Self {
        Self { items: vec![], unreadable: 0 }
    }
}

/// Each row of `rows` that parses as a `T`.
pub fn rows<T: DeserializeOwned>(rows: Vec<Value>) -> Listing<T> {
    let mut out = Listing::default();
    for r in rows {
        match serde_json::from_value(r) {
            Ok(t) => out.items.push(t),
            Err(e) => {
                tracing::warn!(error = %e, "a Fountain row that doesn't parse");
                out.unreadable += 1;
            }
        }
    }
    out
}

/// A client for one account: its base URL and key.
#[derive(Clone)]
pub struct Client {
    base: String,
    key: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("base", &self.base).finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(base: &str, key: &str) -> Self {
        Self { base: base.trim_end_matches('/').to_owned(), key: key.to_owned(), http: crate::forge::http() }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        let url = format!("{}{path}", self.base);
        let res = self
            .http
            .get(&url)
            .bearer_auth(&self.key)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| Error::Http(format!("can't reach Fountain at {}: {e}", self.base)))?;
        let status = res.status();
        let body = res.bytes().await.map_err(|e| Error::Http(format!("{path}: {e}")))?;
        if !status.is_success() {
            let why = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v["error"]["message"].as_str().or(v["error"].as_str()).or(v["message"].as_str()).map(str::to_owned)
                })
                .unwrap_or_else(|| status.to_string());
            return Err(match status.as_u16() {
                401 | 403 => Error::Denied(why),
                404 => Error::NotFound(format!("{path}: {why}")),
                _ => Error::Http(format!("{path}: {why}")),
            });
        }
        serde_json::from_slice::<Data<T>>(&body)
            .map(|d| d.data)
            .map_err(|e| Error::Http(format!("{path}: Fountain said something else: {e}")))
    }

    /// Every agent on the account, whole.
    pub async fn agents(&self) -> Result<Listing<Agent>, Error> {
        Ok(rows(self.get::<Vec<Value>>("/api/agents").await?))
    }

    /// One agent, by id.
    pub async fn agent(&self, id: &str) -> Result<Agent, Error> {
        self.get(&format!("/api/agents/{id}")).await
    }

    pub async fn environments(&self) -> Result<Vec<Environment>, Error> {
        Ok(rows(self.get::<Vec<Value>>("/api/environments").await?).items)
    }

    /// The account's vaults, by name only (M44: a worn agent's secrets).
    pub async fn vaults(&self) -> Result<Vec<Environment>, Error> {
        Ok(rows(self.get::<Vec<Value>>("/api/vaults").await?).items)
    }

    pub async fn runners(&self) -> Result<Listing<Runner>, Error> {
        Ok(rows(self.get::<Vec<Value>>("/api/runners").await?))
    }

    /// Sandboxes, with their conversations; `status`: comma-separated
    /// states to keep.
    pub async fn sandboxes(&self, status: Option<&str>) -> Result<Listing<Sandbox>, Error> {
        let path = match status {
            Some(s) => format!("/api/sandboxes?status={s}"),
            None => "/api/sandboxes".to_owned(),
        };
        Ok(rows(self.get::<Vec<Value>>(&path).await?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_as_fountain_sends_them() {
        let body = include_str!("../../tests/fixtures/fountain/agents.json");
        let v: Data<Vec<Value>> = serde_json::from_str(body).unwrap();
        let l: Listing<Agent> = rows(v.data);
        assert_eq!(l.unreadable, 0);
        let agents = l.items;
        assert!(agents.len() > 100);
        let games = agents.iter().find(|a| a.name == "games").unwrap();
        assert_eq!(games.runtime, "claude");
        let skills: Vec<String> = games.skills.iter().map(Skill::label).collect();
        assert_eq!(skills, ["love2d", "pixijs", "screenshots-in-prs"]);
        assert!(games.skills.iter().all(Skill::inline));
        let pr = agents.iter().find(|a| a.name == "pr-reviewer").unwrap();
        assert_eq!(pr.metadata["managed-by"], "chant");
        assert!(pr.skills.iter().any(|s| s.label() == "example-org/agent-skills/*"));
        assert_eq!(pr.mcp_servers.keys().collect::<Vec<_>>(), ["context7", "github", "mem0"]);
        assert_eq!(pr.mcp_servers["github"].kind.as_deref(), Some("http"));
        // What isn't typed comes through.
        assert!(pr.extra.contains_key("permission_policy"));
        let back = serde_json::to_value(pr).unwrap();
        assert_eq!(back["mcp_servers"]["context7"]["url"], "https://mcp.context7.com/mcp");
    }

    #[test]
    fn odd_rows_dont_empty_the_list() {
        let odd: Data<Vec<Value>> =
            serde_json::from_str(include_str!("../../tests/fixtures/fountain/odd-agents.json")).unwrap();
        let l: Listing<Agent> = rows(odd.data);
        assert_eq!(l.unreadable, 1, "the garbage row");
        let nulls = l.items.iter().find(|a| a.name == "fixture-nulls").unwrap();
        let s = &nulls.mcp_servers["tool"];
        assert!(s.headers.is_empty() && s.env.is_empty() && s.args.is_empty());
        assert!(nulls.skills.is_empty() && nulls.model.is_empty());
        let lit = l.items.iter().find(|a| a.name == "fixture-literal-header").unwrap();
        assert_eq!(lit.mcp_servers["tool"].headers["X-Count"], "3", "a number is its text");
        let r: Listing<Runner> =
            rows(vec![serde_json::json!({ "id": "r", "name": null, "online": null }), serde_json::json!(7)]);
        assert_eq!((r.items.len(), r.unreadable, r.items[0].online), (1, 1, false));
        let e: Listing<Environment> = rows(vec![serde_json::json!({ "id": "e", "name": null })]);
        assert_eq!(e.items[0].name, "");
    }

    #[test]
    fn literal_values_are_redacted_when_shown() {
        let s: McpServer = serde_json::from_value(serde_json::json!({
            "type": "http", "url": "https://mcp-1.example.com/mcp",
            "headers": { "Authorization": "Bearer ${X}", "X-Api-Key": "typed-in-literally", "X-Mixed": "k=${Y};v=1" },
            "env": { "TOKEN": "also-literal", "REF": "${Z}" }
        }))
        .unwrap();
        let r = s.redacted();
        assert_eq!(r.headers["Authorization"], "Bearer ${X}");
        assert_eq!(r.headers["X-Api-Key"], REDACTED);
        assert_eq!(r.headers["X-Mixed"], "k=${Y};v=1");
        assert_eq!((r.env["TOKEN"].as_str(), r.env["REF"].as_str()), (REDACTED, "${Z}"));
        assert_eq!(s.headers["X-Api-Key"], "typed-in-literally", "kept as read, for M44");
        assert!(!is_reference("${unclosed") && is_reference("a${B}c"));
    }

    #[test]
    fn runners_and_sandboxes() {
        let r: Data<Vec<Runner>> =
            serde_json::from_str(include_str!("../../tests/fixtures/fountain/runners.json")).unwrap();
        assert!(r.data.iter().any(|r| r.online));
        assert!(r.data.iter().all(|r| !r.name.is_empty() && r.last_seen_at.is_some()));
        let s: Data<Vec<Sandbox>> = serde_json::from_value(serde_json::json!({ "data": [{
            "id": "s1", "sprite_name": "runner-r1-abc", "status": "ready", "provider": "runner", "mode": "ephemeral",
            "runner": { "id": "r1", "name": "geek", "online": true, "path": "/home/fountain/sandboxes/runner-r1-abc" },
            "conversations": [{ "id": "c1", "status": "idle", "mid_turn": false, "runtime": "claude" }]
        }, { "id": "s2", "sprite_name": "x", "status": "terminated", "runner": null, "conversations": null }] }))
        .unwrap();
        assert_eq!(s.data[0].runner.as_ref().unwrap().path.as_deref(), Some("/home/fountain/sandboxes/runner-r1-abc"));
        assert_eq!(s.data[0].conversations[0].status, "idle");
        assert!(s.data[1].conversations.is_empty());
    }
}
