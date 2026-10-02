//! Control's database (SQLite). What's in it is metadata only: accounts,
//! sign-in identities, sessions, device certificates (public keys),
//! revocations, pending joins, the directory and byte counts. Nothing a
//! daemon sends inside a channel ever reaches it.

use std::{path::Path, sync::Mutex};

use anyhow::Context;
use illogical_e2e::{Cert, Revocation};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub struct Db {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS accounts (
    id TEXT PRIMARY KEY,
    root TEXT,
    created INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS identities (
    provider TEXT NOT NULL,
    subject TEXT NOT NULL,
    account TEXT NOT NULL,
    login TEXT NOT NULL,
    PRIMARY KEY (provider, subject)
);
CREATE TABLE IF NOT EXISTS sessions (
    token_hash TEXT PRIMARY KEY,
    account TEXT NOT NULL,
    created INTEGER NOT NULL,
    expires INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS devices (
    account TEXT NOT NULL,
    id TEXT NOT NULL,
    kind TEXT NOT NULL,
    cert TEXT NOT NULL,
    approved INTEGER NOT NULL,
    created INTEGER NOT NULL,
    PRIMARY KEY (account, id)
);
CREATE TABLE IF NOT EXISTS revocations (
    account TEXT NOT NULL,
    device TEXT NOT NULL,
    body TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS joins (
    code TEXT PRIMARY KEY,
    cert TEXT NOT NULL,
    poll_hash TEXT NOT NULL,
    urls TEXT NOT NULL,
    created INTEGER NOT NULL,
    account TEXT
);
CREATE TABLE IF NOT EXISTS daemons (
    id TEXT PRIMARY KEY,
    account TEXT NOT NULL,
    name TEXT NOT NULL,
    urls TEXT NOT NULL,
    last_seen INTEGER
);
CREATE TABLE IF NOT EXISTS passkeys (
    id TEXT PRIMARY KEY,
    account TEXT NOT NULL,
    alg INTEGER NOT NULL,
    public BLOB NOT NULL,
    sign_count INTEGER NOT NULL,
    created INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS teams (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    founder TEXT NOT NULL,
    founder_root TEXT NOT NULL,
    locked INTEGER NOT NULL DEFAULT 0,
    created INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS rosters (
    team TEXT NOT NULL,
    version INTEGER NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (team, version)
);
CREATE TABLE IF NOT EXISTS invites (
    code_hash TEXT PRIMARY KEY,
    team TEXT NOT NULL,
    role TEXT NOT NULL,
    expires INTEGER NOT NULL,
    by_account TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS team_requests (
    team TEXT NOT NULL,
    account TEXT NOT NULL,
    root TEXT NOT NULL,
    name TEXT NOT NULL,
    role TEXT NOT NULL,
    created INTEGER NOT NULL,
    PRIMARY KEY (team, account)
);
CREATE TABLE IF NOT EXISTS daemon_access (
    daemon TEXT NOT NULL,
    account TEXT NOT NULL,
    PRIMARY KEY (daemon, account)
);
CREATE TABLE IF NOT EXISTS daemon_links (
    daemon TEXT PRIMARY KEY,
    until INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS usage (
    account TEXT NOT NULL,
    day TEXT NOT NULL,
    relay_bytes INTEGER NOT NULL,
    PRIMARY KEY (account, day)
);
";

/// Columns added after a table first shipped.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let has = |table: &str, col: &str| -> rusqlite::Result<bool> {
        let mut q = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let names: Vec<String> = q.query_map([], |r| r.get(1))?.collect::<Result<_, _>>()?;
        Ok(names.iter().any(|n| n == col))
    };
    if !has("daemons", "team")? {
        conn.execute_batch("ALTER TABLE daemons ADD COLUMN team TEXT")?;
    }
    if !has("joins", "team")? {
        conn.execute_batch("ALTER TABLE joins ADD COLUMN team TEXT")?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub founder: String,
    pub founder_root: String,
    pub locked: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamRequest {
    pub account: String,
    pub root: String,
    pub name: String,
    pub role: String,
    pub created: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Account {
    pub id: String,
    pub root: Option<String>,
    pub login: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DaemonRow {
    pub id: String,
    pub name: String,
    pub urls: Vec<String>,
    pub last_seen: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Passkey {
    pub account: String,
    pub alg: i64,
    pub public: Vec<u8>,
    pub sign_count: u32,
}

#[derive(Debug, Clone)]
pub struct Join {
    pub cert: Cert,
    pub poll_hash: String,
    pub urls: Vec<String>,
    pub created: u64,
    pub account: Option<String>,
    /// A team daemon's team (M19).
    pub team: Option<String>,
}

fn cert_of(s: String) -> rusqlite::Result<Cert> {
    serde_json::from_str(&s)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn memory() -> Self {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        migrate(&conn).unwrap();
        Self { conn: Mutex::new(conn) }
    }

    fn c(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }

    // ---- accounts and sign-in

    /// The account for this provider identity, made on first sign-in.
    pub fn account_for(
        &self,
        provider: &str,
        subject: &str,
        login: &str,
        new_id: &str,
        now: u64,
    ) -> anyhow::Result<String> {
        let c = self.c();
        let found: Option<String> = c
            .query_row(
                "SELECT account FROM identities WHERE provider = ?1 AND subject = ?2",
                params![provider, subject],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(a) = found {
            c.execute(
                "UPDATE identities SET login = ?3 WHERE provider = ?1 AND subject = ?2",
                params![provider, subject, login],
            )?;
            return Ok(a);
        }
        c.execute("INSERT INTO accounts (id, root, created) VALUES (?1, NULL, ?2)", params![new_id, now])?;
        c.execute(
            "INSERT INTO identities (provider, subject, account, login) VALUES (?1, ?2, ?3, ?4)",
            params![provider, subject, new_id, login],
        )?;
        Ok(new_id.to_owned())
    }

    pub fn account(&self, id: &str) -> anyhow::Result<Option<Account>> {
        Ok(self
            .c()
            .query_row(
                "SELECT a.id, a.root, COALESCE((SELECT login FROM identities WHERE account = a.id LIMIT 1), '') FROM accounts a WHERE a.id = ?1",
                params![id],
                |r| Ok(Account { id: r.get(0)?, root: r.get(1)?, login: r.get(2)? }),
            )
            .optional()?)
    }

    pub fn add_session(&self, token_hash: &str, account: &str, now: u64, expires: u64) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO sessions (token_hash, account, created, expires) VALUES (?1, ?2, ?3, ?4)",
            params![token_hash, account, now, expires],
        )?;
        Ok(())
    }

    pub fn session(&self, token_hash: &str, now: u64) -> anyhow::Result<Option<String>> {
        Ok(self
            .c()
            .query_row(
                "SELECT account FROM sessions WHERE token_hash = ?1 AND expires > ?2",
                params![token_hash, now],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn drop_session(&self, token_hash: &str) -> anyhow::Result<()> {
        self.c().execute("DELETE FROM sessions WHERE token_hash = ?1", params![token_hash])?;
        Ok(())
    }

    // ---- passkeys

    pub fn add_passkey(&self, id: &str, account: &str, alg: i64, public: &[u8], now: u64) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO passkeys (id, account, alg, public, sign_count, created) VALUES (?1, ?2, ?3, ?4, 0, ?5)",
            params![id, account, alg, public, now],
        )?;
        Ok(())
    }

    pub fn passkey(&self, id: &str) -> anyhow::Result<Option<Passkey>> {
        Ok(self
            .c()
            .query_row("SELECT account, alg, public, sign_count FROM passkeys WHERE id = ?1", params![id], |r| {
                Ok(Passkey { account: r.get(0)?, alg: r.get(1)?, public: r.get(2)?, sign_count: r.get(3)? })
            })
            .optional()?)
    }

    pub fn passkey_used(&self, id: &str, count: u32) -> anyhow::Result<()> {
        self.c().execute("UPDATE passkeys SET sign_count = ?2 WHERE id = ?1", params![id, count])?;
        Ok(())
    }

    pub fn passkey_count(&self, account: &str) -> anyhow::Result<u64> {
        Ok(self.c().query_row("SELECT COUNT(*) FROM passkeys WHERE account = ?1", params![account], |r| r.get(0))?)
    }

    // ---- devices

    /// Store a device's certificate; the first approved one becomes the
    /// account's root.
    pub fn put_device(&self, cert: &Cert, approved: bool, now: u64) -> anyhow::Result<()> {
        let mut c = self.c();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO devices (account, id, kind, cert, approved, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (account, id) DO UPDATE SET cert = excluded.cert, approved = excluded.approved",
            params![cert.account, cert.device, cert.kind.as_str(), serde_json::to_string(cert)?, approved, now],
        )?;
        if approved && cert.approver == cert.device {
            tx.execute(
                "UPDATE accounts SET root = ?2 WHERE id = ?1 AND root IS NULL",
                params![cert.account, cert.device],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn device(&self, account: &str, id: &str) -> anyhow::Result<Option<(Cert, bool)>> {
        Ok(self
            .c()
            .query_row("SELECT cert, approved FROM devices WHERE account = ?1 AND id = ?2", params![account, id], |r| {
                Ok((cert_of(r.get(0)?)?, r.get(1)?))
            })
            .optional()?)
    }

    /// An approved daemon's certificate, by its id (accounts don't share
    /// daemon keys).
    pub fn daemon_cert(&self, id: &str) -> anyhow::Result<Option<Cert>> {
        Ok(self
            .c()
            .query_row(
                "SELECT cert FROM devices WHERE id = ?1 AND kind = 'daemon' AND approved = 1",
                params![id],
                |r| cert_of(r.get(0)?),
            )
            .optional()?)
    }

    /// Every certificate of an account: (approved ones, pending ones).
    pub fn devices(&self, account: &str) -> anyhow::Result<(Vec<Cert>, Vec<Cert>)> {
        let c = self.c();
        let mut q = c.prepare("SELECT cert, approved FROM devices WHERE account = ?1 ORDER BY created")?;
        let rows = q.query_map(params![account], |r| Ok((cert_of(r.get(0)?)?, r.get::<_, bool>(1)?)))?;
        let (mut yes, mut no) = (Vec::new(), Vec::new());
        for row in rows {
            let (cert, approved) = row?;
            if approved { yes.push(cert) } else { no.push(cert) }
        }
        Ok((yes, no))
    }

    pub fn drop_pending(&self, account: &str, id: &str) -> anyhow::Result<()> {
        self.c()
            .execute("DELETE FROM devices WHERE account = ?1 AND id = ?2 AND approved = 0", params![account, id])?;
        Ok(())
    }

    pub fn add_revocation(&self, r: &Revocation) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO revocations (account, device, body) VALUES (?1, ?2, ?3)",
            params![r.account, r.device, serde_json::to_string(r)?],
        )?;
        Ok(())
    }

    pub fn revocations(&self, account: &str) -> anyhow::Result<Vec<Revocation>> {
        let c = self.c();
        let mut q = c.prepare("SELECT body FROM revocations WHERE account = ?1")?;
        let rows = q.query_map(params![account], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    // ---- joins

    pub fn add_join(
        &self,
        code: &str,
        cert: &Cert,
        poll_hash: &str,
        urls: &[String],
        team: Option<&str>,
        now: u64,
    ) -> anyhow::Result<()> {
        let c = self.c();
        // Old ones go first; a code can be asked for again.
        c.execute("DELETE FROM joins WHERE created < ?1", params![now.saturating_sub(JOIN_TTL_MS)])?;
        c.execute(
            "INSERT OR REPLACE INTO joins (code, cert, poll_hash, urls, created, account, team) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
            params![code, serde_json::to_string(cert)?, poll_hash, serde_json::to_string(urls)?, now, team],
        )?;
        Ok(())
    }

    pub fn join(&self, code: &str, now: u64) -> anyhow::Result<Option<Join>> {
        Ok(self
            .c()
            .query_row(
                "SELECT cert, poll_hash, urls, created, account, team FROM joins WHERE code = ?1 AND created >= ?2",
                params![code, now.saturating_sub(JOIN_TTL_MS)],
                |r| {
                    Ok(Join {
                        cert: cert_of(r.get(0)?)?,
                        poll_hash: r.get(1)?,
                        urls: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                        created: r.get(3)?,
                        account: r.get(4)?,
                        team: r.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    /// Approved: the signed certificate replaces the request.
    pub fn approve_join(&self, code: &str, cert: &Cert) -> anyhow::Result<()> {
        self.c().execute(
            "UPDATE joins SET cert = ?2, account = ?3 WHERE code = ?1",
            params![code, serde_json::to_string(cert)?, cert.account],
        )?;
        Ok(())
    }

    pub fn drop_join(&self, code: &str) -> anyhow::Result<()> {
        self.c().execute("DELETE FROM joins WHERE code = ?1", params![code])?;
        Ok(())
    }

    // ---- the directory

    pub fn put_daemon(&self, account: &str, id: &str, name: &str, urls: &[String]) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO daemons (id, account, name, urls, last_seen) VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT (id) DO UPDATE SET name = excluded.name, urls = excluded.urls",
            params![id, account, name, serde_json::to_string(urls)?],
        )?;
        Ok(())
    }

    pub fn seen(&self, id: &str, urls: Option<&[String]>, now: u64) -> anyhow::Result<()> {
        let c = self.c();
        match urls {
            Some(u) => c.execute(
                "UPDATE daemons SET last_seen = ?2, urls = ?3 WHERE id = ?1",
                params![id, now, serde_json::to_string(u)?],
            )?,
            None => c.execute("UPDATE daemons SET last_seen = ?2 WHERE id = ?1", params![id, now])?,
        };
        Ok(())
    }

    pub fn daemons(&self, account: &str) -> anyhow::Result<Vec<DaemonRow>> {
        let c = self.c();
        let mut q = c.prepare("SELECT id, name, urls, last_seen FROM daemons WHERE account = ?1 ORDER BY name")?;
        let rows = q.query_map(params![account], |r| {
            Ok(DaemonRow {
                id: r.get(0)?,
                name: r.get(1)?,
                urls: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                last_seen: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn daemon_account(&self, id: &str) -> anyhow::Result<Option<String>> {
        Ok(self.c().query_row("SELECT account FROM daemons WHERE id = ?1", params![id], |r| r.get(0)).optional()?)
    }

    pub fn drop_daemon(&self, id: &str) -> anyhow::Result<()> {
        let c = self.c();
        c.execute("DELETE FROM daemons WHERE id = ?1", params![id])?;
        c.execute("DELETE FROM devices WHERE id = ?1 AND kind = 'daemon'", params![id])?;
        Ok(())
    }

    // ---- people and teams (M19)

    /// An account by its sign-in login (to share with a person).
    pub fn account_by_login(&self, login: &str) -> anyhow::Result<Option<Account>> {
        let c = self.c();
        let id: Option<String> = c
            .query_row("SELECT account FROM identities WHERE lower(login) = lower(?1) LIMIT 1", params![login], |r| {
                r.get(0)
            })
            .optional()?;
        drop(c);
        match id {
            Some(id) => self.account(&id),
            None => Ok(None),
        }
    }

    pub fn add_team(&self, t: &Team, roster_version: u64, roster: &str, now: u64) -> anyhow::Result<()> {
        let mut c = self.c();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO teams (id, name, founder, founder_root, locked, created) VALUES (?1, ?2, ?3, ?4, 0, ?5)",
            params![t.id, t.name, t.founder, t.founder_root, now],
        )?;
        tx.execute(
            "INSERT INTO rosters (team, version, body) VALUES (?1, ?2, ?3)",
            params![t.id, roster_version, roster],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn team(&self, id: &str) -> anyhow::Result<Option<Team>> {
        Ok(self
            .c()
            .query_row("SELECT id, name, founder, founder_root, locked FROM teams WHERE id = ?1", params![id], |r| {
                Ok(Team {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    founder: r.get(2)?,
                    founder_root: r.get(3)?,
                    locked: r.get(4)?,
                })
            })
            .optional()?)
    }

    pub fn set_locked(&self, team: &str, locked: bool) -> anyhow::Result<()> {
        self.c().execute("UPDATE teams SET locked = ?2 WHERE id = ?1", params![team, locked])?;
        Ok(())
    }

    pub fn add_roster(&self, team: &str, version: u64, body: &str) -> anyhow::Result<()> {
        let c = self.c();
        c.execute("INSERT INTO rosters (team, version, body) VALUES (?1, ?2, ?3)", params![team, version, body])?;
        if let Some(name) =
            serde_json::from_str::<serde_json::Value>(body).ok().and_then(|v| v["name"].as_str().map(str::to_owned))
        {
            c.execute("UPDATE teams SET name = ?2 WHERE id = ?1", params![team, name])?;
        }
        Ok(())
    }

    /// A team's rosters after `since`, oldest first (the whole chain for 0).
    pub fn rosters(&self, team: &str, since: u64) -> anyhow::Result<Vec<String>> {
        let c = self.c();
        let mut q = c.prepare("SELECT body FROM rosters WHERE team = ?1 AND version > ?2 ORDER BY version")?;
        let rows = q.query_map(params![team, since], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn latest_roster(&self, team: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .c()
            .query_row("SELECT body FROM rosters WHERE team = ?1 ORDER BY version DESC LIMIT 1", params![team], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Teams an account is in, by their latest rosters.
    pub fn teams_of(&self, account: &str) -> anyhow::Result<Vec<String>> {
        let c = self.c();
        let mut q = c.prepare(
            "SELECT r.body FROM rosters r JOIN (SELECT team, MAX(version) v FROM rosters GROUP BY team) m
             ON r.team = m.team AND r.version = m.v",
        )?;
        let rows: Vec<String> = q.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows
            .into_iter()
            .filter(|b| {
                serde_json::from_str::<serde_json::Value>(b).ok().is_some_and(|v| {
                    v["members"].as_array().is_some_and(|ms| ms.iter().any(|m| m["account"] == account))
                })
            })
            .collect())
    }

    pub fn add_invite(&self, code_hash: &str, team: &str, role: &str, expires: u64, by: &str) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO invites (code_hash, team, role, expires, by_account) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![code_hash, team, role, expires, by],
        )?;
        Ok(())
    }

    /// (team, role) for a live invite.
    pub fn invite(&self, code_hash: &str, now: u64) -> anyhow::Result<Option<(String, String)>> {
        Ok(self
            .c()
            .query_row(
                "SELECT team, role FROM invites WHERE code_hash = ?1 AND expires > ?2",
                params![code_hash, now],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn drop_invites(&self, team: &str) -> anyhow::Result<()> {
        let c = self.c();
        c.execute("DELETE FROM invites WHERE team = ?1", params![team])?;
        c.execute("DELETE FROM team_requests WHERE team = ?1", params![team])?;
        Ok(())
    }

    pub fn add_request(&self, team: &str, r: &TeamRequest) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT OR REPLACE INTO team_requests (team, account, root, name, role, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![team, r.account, r.root, r.name, r.role, r.created],
        )?;
        Ok(())
    }

    pub fn requests(&self, team: &str) -> anyhow::Result<Vec<TeamRequest>> {
        let c = self.c();
        let mut q =
            c.prepare("SELECT account, root, name, role, created FROM team_requests WHERE team = ?1 ORDER BY created")?;
        let rows = q.query_map(params![team], |r| {
            Ok(TeamRequest {
                account: r.get(0)?,
                root: r.get(1)?,
                name: r.get(2)?,
                role: r.get(3)?,
                created: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn drop_request(&self, team: &str, account: &str) -> anyhow::Result<()> {
        self.c().execute("DELETE FROM team_requests WHERE team = ?1 AND account = ?2", params![team, account])?;
        Ok(())
    }

    pub fn set_daemon_team(&self, daemon: &str, team: &str) -> anyhow::Result<()> {
        self.c().execute("UPDATE daemons SET team = ?2 WHERE id = ?1", params![daemon, team])?;
        Ok(())
    }

    pub fn daemon_team(&self, daemon: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .c()
            .query_row("SELECT team FROM daemons WHERE id = ?1", params![daemon], |r| r.get(0))
            .optional()?
            .flatten())
    }

    pub fn team_daemons(&self, team: &str) -> anyhow::Result<Vec<String>> {
        let c = self.c();
        let mut q = c.prepare("SELECT id FROM daemons WHERE team = ?1")?;
        let rows = q.query_map(params![team], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The accounts a daemon says it lets in (for routing only: it checks
    /// for itself).
    pub fn set_access(&self, daemon: &str, accounts: &[String], links_until: Option<u64>) -> anyhow::Result<()> {
        let mut c = self.c();
        let tx = c.transaction()?;
        tx.execute("DELETE FROM daemon_access WHERE daemon = ?1", params![daemon])?;
        for a in accounts {
            tx.execute("INSERT OR IGNORE INTO daemon_access (daemon, account) VALUES (?1, ?2)", params![daemon, a])?;
        }
        match links_until {
            Some(u) => {
                tx.execute("INSERT OR REPLACE INTO daemon_links (daemon, until) VALUES (?1, ?2)", params![daemon, u])?
            }
            None => tx.execute("DELETE FROM daemon_links WHERE daemon = ?1", params![daemon])?,
        };
        tx.commit()?;
        Ok(())
    }

    pub fn daemon_lets_in(&self, daemon: &str, account: &str) -> anyhow::Result<bool> {
        Ok(self
            .c()
            .query_row(
                "SELECT 1 FROM daemon_access WHERE daemon = ?1 AND account = ?2",
                params![daemon, account],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn daemon_has_links(&self, daemon: &str, now: u64) -> anyhow::Result<bool> {
        Ok(self
            .c()
            .query_row("SELECT 1 FROM daemon_links WHERE daemon = ?1 AND until > ?2", params![daemon, now], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Daemons shared with an account (not its own, not by team).
    pub fn shared_daemons(&self, account: &str) -> anyhow::Result<Vec<String>> {
        let c = self.c();
        let mut q = c.prepare("SELECT daemon FROM daemon_access WHERE account = ?1")?;
        let rows = q.query_map(params![account], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn daemon_row(&self, id: &str) -> anyhow::Result<Option<(String, DaemonRow)>> {
        Ok(self
            .c()
            .query_row("SELECT account, id, name, urls, last_seen FROM daemons WHERE id = ?1", params![id], |r| {
                Ok((
                    r.get(0)?,
                    DaemonRow {
                        id: r.get(1)?,
                        name: r.get(2)?,
                        urls: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
                        last_seen: r.get(4)?,
                    },
                ))
            })
            .optional()?)
    }

    // ---- metering

    pub fn add_relay_bytes(&self, account: &str, day: &str, bytes: u64) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO usage (account, day, relay_bytes) VALUES (?1, ?2, ?3)
             ON CONFLICT (account, day) DO UPDATE SET relay_bytes = relay_bytes + excluded.relay_bytes",
            params![account, day, bytes],
        )?;
        Ok(())
    }

    pub fn relay_bytes(&self, account: &str, day: &str) -> anyhow::Result<u64> {
        Ok(self
            .c()
            .query_row("SELECT relay_bytes FROM usage WHERE account = ?1 AND day = ?2", params![account, day], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0))
    }
}

/// How long a join code stays good.
pub const JOIN_TTL_MS: u64 = 15 * 60 * 1000;
