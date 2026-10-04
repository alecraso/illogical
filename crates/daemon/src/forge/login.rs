//! Which of the person's `tea` logins a PR is read and written with, and
//! its token (M36, S23 §1).
//!
//! - **Logins** come from `tea logins list -o json` (name, url, ssh_host,
//!   user; never a token), run on the block's host with the user's shell
//!   environment (#74).
//! - **Matching a host** (a remote's, or a PR link's): a login whose URL
//!   host or `ssh_host` is that host. If none is, each login is asked for
//!   the repository (`GET repos/O/R`) and kept if its `ssh_url` or
//!   `clone_url` is on that host: one instance often has two names (a
//!   public one for HTTPS, a tailnet one for SSH). The answer is kept for
//!   the host while the daemon runs.
//! - **None, or several:** the block says so and lists the logins to pick
//!   from (`login {name}`, kept in its config).
//! - **The token** is `tea login helper get`'s `password` (tea's git
//!   credential helper, which refreshes an OAuth login), asked for once and
//!   held in memory only: never logged, saved, or sent to a client. It's
//!   asked for again after a 401.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::review::Runner;

/// One `tea` login, as `tea logins list -o json` prints it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Login {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub ssh_host: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub default: bool,
}

impl Login {
    /// The forge's API base for this login.
    pub fn api(&self) -> String {
        format!("{}/api/v1", self.url.trim_end_matches('/'))
    }
}

/// `tea logins list -o json`: `default` is a string there ("false").
pub fn parse_logins(out: &[u8]) -> Result<Vec<Login>, String> {
    let v: Value = serde_json::from_slice(out).map_err(|e| format!("tea said something else: {e}"))?;
    Ok(v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| {
            Some(Login {
                name: l["name"].as_str()?.to_owned(),
                url: l["url"].as_str()?.to_owned(),
                ssh_host: l["ssh_host"].as_str().unwrap_or_default().to_owned(),
                user: l["user"].as_str().unwrap_or_default().to_owned(),
                default: l["default"] == true || l["default"] == "true",
            })
        })
        .collect())
}

/// The host (with a port, if not the scheme's) of a URL.
pub fn url_host(u: &str) -> Option<String> {
    let u = url::Url::parse(u).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    Some(match u.port() {
        Some(p) => format!("{host}:{p}"),
        None => host,
    })
}

/// A git remote's host and repository path (`owner/name`, no `.git`):
/// `ssh://git@host[:port]/o/r.git`, `git@host:o/r.git` or
/// `https://host/o/r`.
pub fn parse_remote(remote: &str) -> Option<(String, String)> {
    let remote = remote.trim();
    let path = |p: &str| {
        let p = p.trim_matches('/').trim_end_matches(".git").trim_end_matches('/');
        (p.split('/').count() >= 2).then(|| p.to_owned())
    };
    if remote.contains("://") {
        let u = url::Url::parse(remote).ok()?;
        let host = u.host_str()?.to_ascii_lowercase();
        // An SSH port isn't the web's: match on the name alone.
        let host = match (u.scheme(), u.port()) {
            ("http" | "https", Some(p)) => format!("{host}:{p}"),
            _ => host,
        };
        return Some((host, path(u.path())?));
    }
    // scp-like: [user@]host:path
    let (left, p) = remote.split_once(':')?;
    if left.contains('/') {
        return None; // a local path with a colon in it
    }
    let host = left.rsplit('@').next()?.to_ascii_lowercase();
    Some((host, path(p)?))
}

/// The logins whose URL host or SSH host is `host`.
pub fn matching<'a>(logins: &'a [Login], host: &str) -> Vec<&'a Login> {
    let bare = host.split(':').next().unwrap_or(host);
    logins
        .iter()
        .filter(|l| {
            let url = url_host(&l.url).unwrap_or_default();
            url == host
                || (!host.contains(':') && url.split(':').next() == Some(bare))
                || l.ssh_host.eq_ignore_ascii_case(bare)
        })
        .collect()
}

/// Running `tea` on the block's host.
#[derive(Clone)]
pub struct Tea {
    pub runner: Runner,
}

const LOGINS: &str = r#"command -v tea >/dev/null 2>&1 || { echo illogical-no-tea; exit 0; }
exec tea logins list -o json 2>/dev/null"#;

/// tea's git credential helper; only the password line is read.
const TOKEN: &str = r#"command -v tea >/dev/null 2>&1 || { echo illogical-no-tea; exit 0; }
printf 'protocol=%s\nhost=%s\n\n' "$1" "$2" | tea login helper get 2>/dev/null"#;

/// What [`Tea::logins`] says when there's no `tea` on the host.
pub const NO_TEA: &str = "no tea here: install tea (Forgejo's CLI) and `tea login add`, then refresh";

impl Tea {
    pub async fn logins(&self) -> Result<Vec<Login>, String> {
        let (out, _) = self.runner.sh(LOGINS, &[]).await?;
        if out.starts_with(b"illogical-no-tea") {
            return Err(NO_TEA.into());
        }
        if out.iter().all(u8::is_ascii_whitespace) {
            return Ok(vec![]);
        }
        parse_logins(&out)
    }

    /// A login's token, from tea (never kept here).
    async fn token(&self, login_url: &str) -> Result<String, String> {
        let u = url::Url::parse(login_url).map_err(|e| format!("{login_url}: {e}"))?;
        let host = url_host(login_url).ok_or("a login with no host")?;
        let (out, _) = self.runner.sh(TOKEN, &[u.scheme().to_owned(), host.clone()]).await?;
        if out.starts_with(b"illogical-no-tea") {
            return Err(NO_TEA.into());
        }
        String::from_utf8_lossy(&out)
            .lines()
            .find_map(|l| l.strip_prefix("password=").map(|p| p.trim().to_owned()))
            .filter(|p| !p.is_empty())
            .ok_or_else(|| format!("tea has no token for {host}: `tea login add` there"))
    }
}

/// Tokens held while the daemon runs, by login URL. Memory only.
static TOKENS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Mutex::default);

/// Which login a host resolved to (by a repo lookup), while the daemon runs.
static HOSTS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Mutex::default);

/// Where a forge adapter gets its token: tea, through the block's host.
#[derive(Clone)]
pub struct TokenSource {
    tea: Arc<Tea>,
    url: String,
}

impl TokenSource {
    pub fn new(tea: Arc<Tea>, login: &Login) -> Self {
        Self { tea, url: login.url.clone() }
    }

    /// The token; `fresh`: the last one was refused, ask tea again.
    pub async fn get(&self, fresh: bool) -> Result<String, String> {
        if !fresh && let Some(t) = TOKENS.lock().unwrap().get(&self.url).cloned() {
            return Ok(t);
        }
        let t = self.tea.token(&self.url).await?;
        TOKENS.lock().unwrap().insert(self.url.clone(), t.clone());
        Ok(t)
    }
}

/// Asks a login for a repository's clone URLs.
pub type Lookup = dyn Fn(Login) -> futures_util::future::BoxFuture<'static, Result<Vec<String>, String>> + Send + Sync;

/// How a host resolved.
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    pub login: Option<Login>,
    /// When it didn't: the logins to pick from.
    pub candidates: Vec<Login>,
    pub error: Option<String>,
}

/// The login for `host` (and repository `repo`), per the rule above.
/// `lookup` asks a login for the repo's clone URLs.
pub async fn resolve(logins: Vec<Login>, host: &str, lookup: &Lookup) -> Resolved {
    if logins.is_empty() {
        return Resolved {
            error: Some(format!("no tea login here: `tea login add` for {host}")),
            ..Default::default()
        };
    }
    let found = matching(&logins, host);
    if let [one] = found.as_slice() {
        return Resolved { login: Some((*one).clone()), ..Default::default() };
    }
    if found.len() > 1 {
        let names: Vec<&str> = found.iter().map(|l| l.name.as_str()).collect();
        return Resolved {
            candidates: found.into_iter().cloned().collect(),
            error: Some(format!("several tea logins for {host} ({}): pick one", names.join(", "))),
            ..Default::default()
        };
    }
    let known = HOSTS.lock().unwrap().get(host).cloned();
    if let Some(l) = known.and_then(|n| logins.iter().find(|l| l.name == n).cloned()) {
        return Resolved { login: Some(l), ..Default::default() };
    }
    let bare = host.split(':').next().unwrap_or(host).to_owned();
    let mut hits = Vec::new();
    for l in &logins {
        if let Ok(urls) = lookup(l.clone()).await
            && urls.iter().any(|u| parse_remote(u).is_some_and(|(h, _)| h.split(':').next() == Some(bare.as_str())))
        {
            hits.push(l.clone());
        }
    }
    match hits.as_slice() {
        [one] => {
            HOSTS.lock().unwrap().insert(host.to_owned(), one.name.clone());
            Resolved { login: Some(one.clone()), ..Default::default() }
        }
        [] => Resolved {
            candidates: logins,
            error: Some(format!("no tea login for {host}: add one (`tea login add`), or use one of these")),
            ..Default::default()
        },
        _ => Resolved {
            error: Some(format!("several tea logins know this repository on {host}: pick one")),
            candidates: hits,
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login(name: &str, url: &str, ssh: &str) -> Login {
        Login { name: name.into(), url: url.into(), ssh_host: ssh.into(), ..Default::default() }
    }

    #[test]
    fn logins_as_tea_prints_them() {
        let out = br#"[{"name":"forgejo","url":"https://git.inevitable.fyi","ssh_host":"git.inevitable.fyi","user":"jhgaylor","default":"false"}]"#;
        let l = parse_logins(out).unwrap();
        assert_eq!(
            l,
            [Login { user: "jhgaylor".into(), ..login("forgejo", "https://git.inevitable.fyi", "git.inevitable.fyi") }]
        );
        assert_eq!(l[0].api(), "https://git.inevitable.fyi/api/v1");
    }

    #[test]
    fn remotes() {
        let r = |s: &str| parse_remote(s).map(|(h, p)| format!("{h} {p}"));
        assert_eq!(
            r("ssh://git@git.tail1234.ts.net/jhgaylor/illogical.git").as_deref(),
            Some("git.tail1234.ts.net jhgaylor/illogical")
        );
        assert_eq!(r("ssh://git@host:2222/o/r.git").as_deref(), Some("host o/r"));
        assert_eq!(r("git@codeberg.org:forgejo/forgejo.git").as_deref(), Some("codeberg.org forgejo/forgejo"));
        assert_eq!(
            r("https://git.inevitable.fyi/jhgaylor/illogical").as_deref(),
            Some("git.inevitable.fyi jhgaylor/illogical")
        );
        assert_eq!(r("http://127.0.0.1:3000/o/r.git").as_deref(), Some("127.0.0.1:3000 o/r"));
        assert_eq!(r("/srv/git/r.git"), None);
        assert_eq!(r("./a:b/c"), None);
    }

    #[test]
    fn matching_by_url_or_ssh_host() {
        let ls =
            [login("pub", "https://git.inevitable.fyi", "git.inevitable.fyi"), login("cb", "https://codeberg.org", "")];
        let names = |h: &str| matching(&ls, h).iter().map(|l| l.name.clone()).collect::<Vec<_>>();
        assert_eq!(names("git.inevitable.fyi"), ["pub"]);
        assert_eq!(names("codeberg.org"), ["cb"]);
        assert!(names("git.tail1234.ts.net").is_empty());
        let local = [login("test", "http://127.0.0.1:3000", "")];
        assert_eq!(matching(&local, "127.0.0.1:3000").len(), 1);
        assert_eq!(matching(&local, "127.0.0.1").len(), 1, "an SSH remote has no web port");
        assert_eq!(matching(&local, "127.0.0.1:4000").len(), 0);
    }

    #[tokio::test]
    async fn resolving_a_host_no_login_names() {
        let ls = vec![
            login("pub", "https://git.inevitable.fyi", "git.inevitable.fyi"),
            login("other", "https://x.example", ""),
        ];
        // The tailnet name serves SSH only; Forgejo says so in ssh_url.
        let r = resolve(ls.clone(), "git.tail1234.ts.net", &|l: Login| {
            Box::pin(async move {
                Ok(if l.name == "pub" {
                    vec!["ssh://git@git.tail1234.ts.net/jhgaylor/illogical.git".into()]
                } else {
                    vec![]
                })
            })
        })
        .await;
        assert_eq!(r.login.map(|l| l.name).as_deref(), Some("pub"));
        // Kept for the host: no lookup the second time.
        let r = resolve(ls.clone(), "git.tail1234.ts.net", &|_| Box::pin(async { Err("not asked".into()) })).await;
        assert_eq!(r.login.map(|l| l.name).as_deref(), Some("pub"));
        let r = resolve(ls.clone(), "nowhere.example", &|_| Box::pin(async { Ok(vec![]) })).await;
        assert!(r.login.is_none());
        assert_eq!(r.candidates.len(), 2);
        assert!(r.error.unwrap().starts_with("no tea login for nowhere.example"));
        let r = resolve(vec![], "h", &|_| Box::pin(async { Ok(vec![]) })).await;
        assert!(r.error.unwrap().starts_with("no tea login here"));
        let two = vec![login("a", "https://h.example", ""), login("b", "https://h.example", "")];
        let r = resolve(two, "h.example", &|_| Box::pin(async { Ok(vec![]) })).await;
        assert_eq!(r.candidates.len(), 2);
    }
}
