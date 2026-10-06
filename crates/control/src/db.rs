//! Control's database (SQLite). What's in it is metadata only: accounts,
//! sign-in identities, sessions, device certificates (public keys),
//! revocations, pending joins, the directory and byte counts. Nothing a
//! daemon sends inside a channel ever reaches it.

use std::{path::Path, sync::Mutex};

use anyhow::Context;
use illogical_e2e::{Cert, Kind, Revocation};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// How long an unseen notice waits.
const NOTICE_TTL_MS: u64 = 30 * 24 * 3600 * 1000;

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
CREATE TABLE IF NOT EXISTS turned_down (
    account TEXT NOT NULL,
    device TEXT NOT NULL,
    by_name TEXT NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (account, device)
);
CREATE TABLE IF NOT EXISTS revocations (
    account TEXT NOT NULL,
    device TEXT NOT NULL,
    body TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS revocations_device ON revocations (device);
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
CREATE TABLE IF NOT EXISTS daemon_watches (
    daemon TEXT NOT NULL,
    team TEXT NOT NULL,
    seen INTEGER NOT NULL,
    PRIMARY KEY (daemon, team)
);
CREATE TABLE IF NOT EXISTS presigned_invites (
    key TEXT PRIMARY KEY,
    team TEXT NOT NULL,
    body TEXT NOT NULL,
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
CREATE TABLE IF NOT EXISTS daemon_offers (
    daemon TEXT NOT NULL,
    account TEXT NOT NULL,
    since INTEGER NOT NULL,
    PRIMARY KEY (daemon, account)
);
CREATE TABLE IF NOT EXISTS daemon_offer_pushes (
    daemon TEXT NOT NULL,
    account TEXT NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (daemon, account)
);
CREATE TABLE IF NOT EXISTS share_answers (
    account TEXT NOT NULL,
    daemon TEXT NOT NULL,
    accepted INTEGER NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (account, daemon)
);
CREATE TABLE IF NOT EXISTS daemon_links (
    daemon TEXT PRIMARY KEY,
    until INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS settings (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS push_subs (
    endpoint TEXT PRIMARY KEY,
    account TEXT NOT NULL,
    body TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sandboxes (
    id TEXT PRIMARY KEY,
    account TEXT NOT NULL,
    device TEXT NOT NULL,
    ticket_hash TEXT NOT NULL,
    state TEXT NOT NULL,
    created INTEGER NOT NULL,
    deleted INTEGER,
    daemon TEXT
);
CREATE TABLE IF NOT EXISTS billing (
    owner TEXT PRIMARY KEY,
    customer TEXT,
    subscription TEXT,
    status TEXT NOT NULL,
    seat_item TEXT,
    updated INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS reported (
    account TEXT PRIMARY KEY,
    minutes INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS usage (
    account TEXT NOT NULL,
    day TEXT NOT NULL,
    relay_bytes INTEGER NOT NULL,
    PRIMARY KEY (account, day)
);
CREATE TABLE IF NOT EXISTS retained_certs (
    account TEXT PRIMARY KEY,
    certs TEXT NOT NULL,
    revocations TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS retained_for (
    account TEXT NOT NULL,
    team TEXT NOT NULL,
    PRIMARY KEY (account, team)
);
CREATE TABLE IF NOT EXISTS notices (
    id INTEGER PRIMARY KEY,
    account TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    created INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS notices_account ON notices (account);
-- Machines whose account was deleted, by a hash of the device id (so
-- nothing of the account is kept): one asking again is told why (#208).
CREATE TABLE IF NOT EXISTS gone_daemons (
    hash TEXT PRIMARY KEY,
    at INTEGER NOT NULL
);
";

/// Columns added after a table first shipped.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let has = |table: &str, col: &str| -> rusqlite::Result<bool> {
        let mut q = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let names: Vec<String> = q.query_map([], |r| r.get(1))?.collect::<Result<_, _>>()?;
        Ok(names.iter().any(|n| n == col))
    };
    // A revocation of one of the account's own machines (#330): only those
    // keep a key out of every account (see `Db::revoked`).
    if !has("revocations", "machine")? {
        conn.execute_batch("ALTER TABLE revocations ADD COLUMN machine INTEGER NOT NULL DEFAULT 0")?;
    }
    if !has("daemons", "team")? {
        conn.execute_batch("ALTER TABLE daemons ADD COLUMN team TEXT")?;
    }
    if !has("joins", "team")? {
        conn.execute_batch("ALTER TABLE joins ADD COLUMN team TEXT")?;
    }
    if !has("joins", "sandbox")? {
        conn.execute_batch("ALTER TABLE joins ADD COLUMN sandbox TEXT")?;
    }
    if !has("joins", "team_sig")? {
        conn.execute_batch("ALTER TABLE joins ADD COLUMN team_sig TEXT")?;
    }
    if !has("joins", "rejected")? {
        conn.execute_batch("ALTER TABLE joins ADD COLUMN rejected TEXT")?;
    }
    if !has("accounts", "name")? {
        conn.execute_batch("ALTER TABLE accounts ADD COLUMN name TEXT")?;
    }
    if !has("daemons", "moved")? {
        conn.execute_batch("ALTER TABLE daemons ADD COLUMN moved TEXT")?;
    }
    if !has("daemons", "features")? {
        conn.execute_batch("ALTER TABLE daemons ADD COLUMN features TEXT")?;
    }
    if !has("joins", "features")? {
        conn.execute_batch("ALTER TABLE joins ADD COLUMN features TEXT")?;
    }
    if !has("sessions", "agent")? {
        conn.execute_batch("ALTER TABLE sessions ADD COLUMN agent TEXT")?;
    }
    // Poll hashes a later join from the same machine took over from (#329).
    if !has("joins", "replaced")? {
        conn.execute_batch("ALTER TABLE joins ADD COLUMN replaced TEXT NOT NULL DEFAULT ''")?;
    }
    if !has("joins", "proven")? {
        conn.execute_batch("ALTER TABLE joins ADD COLUMN proven INTEGER NOT NULL DEFAULT 0")?;
    }
    // #326: the machine's join code a waiting browser came to approve.
    if !has("devices", "join_code")? {
        conn.execute_batch("ALTER TABLE devices ADD COLUMN join_code TEXT")?;
    }
    // #208: what a passkey is (its maker, from its AAGUID), the browser
    // that added it, and when it last signed in, to tell them apart.
    if !has("passkeys", "agent")? {
        conn.execute_batch("ALTER TABLE passkeys ADD COLUMN agent TEXT")?;
    }
    if !has("passkeys", "provider")? {
        conn.execute_batch("ALTER TABLE passkeys ADD COLUMN provider TEXT")?;
    }
    if !has("passkeys", "used")? {
        conn.execute_batch("ALTER TABLE passkeys ADD COLUMN used INTEGER")?;
    }
    Ok(())
}

/// Before share answers were kept, every account a daemon listed was
/// routed to: those already listed count as accepted, so nothing shared
/// then stops working.
fn grandfather_shares(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO share_answers (account, daemon, accepted, at)
         SELECT account, daemon, 1, 0 FROM daemon_access",
        [],
    )?;
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
pub struct BillingRow {
    pub customer: Option<String>,
    pub subscription: Option<String>,
    pub status: String,
    pub seat_item: Option<String>,
}

impl BillingRow {
    pub fn active(&self) -> bool {
        matches!(self.status.as_str(), "active" | "trialing" | "past_due")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SandboxRow {
    pub id: String,
    pub account: String,
    pub device: String,
    pub state: String,
    pub created: u64,
    pub deleted: Option<u64>,
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
    /// What other people see (#102): the name it chose, or its GitHub
    /// login. Empty only for a passkey account made before names.
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub hash: String,
    pub created: u64,
    pub expires: u64,
    pub agent: String,
}

/// What deleting an account (#173) takes with it, besides its own rows.
pub struct Erase<'a> {
    pub account: &'a str,
    /// Teams that go with it (it founded them, or nobody else is left).
    pub disband: &'a [String],
    /// Teams whose signed history its devices signed: its certificates
    /// stay, for checking those signatures only, while they last.
    pub retain_for: &'a [String],
}

#[derive(Debug, Clone, Serialize)]
pub struct DaemonRow {
    pub id: String,
    pub name: String,
    pub urls: Vec<String>,
    pub last_seen: Option<u64>,
}

/// A passkey as the account panel lists it.
#[derive(Debug, Serialize)]
pub struct PasskeyRow {
    pub id: String,
    pub created: u64,
    /// The User-Agent of the browser that added it ("" before #208).
    pub agent: String,
    /// What keeps it (iCloud Keychain, Windows Hello...), when known.
    pub provider: Option<String>,
    /// Its last sign-in, if any since #208.
    pub used: Option<u64>,
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
    /// A hosted sandbox's (M20): its requester approves it by itself.
    pub sandbox: Option<String>,
    /// The approver's signature putting it in `team` (#100).
    pub team_sig: Option<String>,
    /// Turned down, on the device named here (#100).
    pub rejected: Option<String>,
    /// What the daemon said it understands when it asked (older ones say
    /// nothing).
    pub features: String,
    /// It signed for its key when it asked (0.17 and newer).
    pub proven: bool,
    /// Poll hashes of requests a later one from this machine took over
    /// from, space-separated (#329).
    pub replaced: String,
}

/// What asking for a join code did (#329).
#[derive(Debug, PartialEq, Eq)]
pub enum Asked {
    /// A new join, or this request took over the one waiting (keeping an
    /// approval it had).
    Waiting,
    /// A join from this machine is waiting already, and this request can't
    /// take it over: it didn't prove it holds the key.
    Taken,
}

/// A row that's already there (a primary key), as opposed to anything
/// else going wrong.
#[derive(Debug)]
pub struct Taken;

impl std::fmt::Display for Taken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("already there")
    }
}

impl std::error::Error for Taken {}

fn taken(e: rusqlite::Error) -> anyhow::Error {
    match e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => Taken.into(),
        e => e.into(),
    }
}

fn cert_of(s: String) -> rusqlite::Result<Cert> {
    serde_json::from_str(&s)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        // Litestream (#174) takes the write lock now and then to checkpoint.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let answers_before: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'share_answers')",
            [],
            |r| r.get(0),
        )?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        if !answers_before {
            grandfather_shares(&conn)?;
        }
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

    /// The account's GitHub identity (M40): its numeric id (as a string)
    /// and the login it had when it last signed in.
    pub fn github_identity(&self, account: &str) -> anyhow::Result<Option<(String, String)>> {
        Ok(self
            .c()
            .query_row(
                "SELECT subject, login FROM identities WHERE account = ?1 AND provider = 'github' LIMIT 1",
                params![account],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn account(&self, id: &str) -> anyhow::Result<Option<Account>> {
        Ok(self
            .c()
            .query_row(
                "SELECT a.id, a.root,
                    COALESCE((SELECT login FROM identities WHERE account = a.id LIMIT 1), ''),
                    COALESCE(NULLIF(a.name, ''),
                        (SELECT login FROM identities WHERE account = a.id AND login != '' ORDER BY provider = 'github' DESC LIMIT 1), '')
                 FROM accounts a WHERE a.id = ?1",
                params![id],
                |r| Ok(Account { id: r.get(0)?, root: r.get(1)?, login: r.get(2)?, name: r.get(3)? }),
            )
            .optional()?)
    }

    /// The account's display name (#102).
    pub fn set_name(&self, account: &str, name: &str) -> anyhow::Result<()> {
        self.c().execute("UPDATE accounts SET name = ?2 WHERE id = ?1", params![account, name])?;
        Ok(())
    }

    pub fn add_session(&self, token_hash: &str, account: &str, now: u64, expires: u64) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO sessions (token_hash, account, created, expires) VALUES (?1, ?2, ?3, ?4)",
            params![token_hash, account, now, expires],
        )?;
        Ok(())
    }

    /// The account a live session is for. An account that's gone (#173)
    /// has none, whenever its session was made.
    pub fn session(&self, token_hash: &str, now: u64) -> anyhow::Result<Option<String>> {
        Ok(self
            .c()
            .query_row(
                "SELECT s.account FROM sessions s JOIN accounts a ON a.id = s.account
                 WHERE s.token_hash = ?1 AND s.expires > ?2",
                params![token_hash, now],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn drop_session(&self, token_hash: &str) -> anyhow::Result<()> {
        self.c().execute("DELETE FROM sessions WHERE token_hash = ?1", params![token_hash])?;
        Ok(())
    }

    /// What signed in, as its browser or app said (#173).
    pub fn set_session_agent(&self, token_hash: &str, agent: &str) -> anyhow::Result<()> {
        self.c().execute("UPDATE sessions SET agent = ?2 WHERE token_hash = ?1", params![token_hash, agent])?;
        Ok(())
    }

    /// An account's live sessions, newest first: (token hash, created,
    /// expires, agent).
    pub fn sessions(&self, account: &str, now: u64) -> anyhow::Result<Vec<SessionRow>> {
        let c = self.c();
        let mut q = c.prepare(
            "SELECT token_hash, created, expires, COALESCE(agent, '') FROM sessions
             WHERE account = ?1 AND expires > ?2 ORDER BY created DESC",
        )?;
        let rows = q.query_map(params![account, now], |r| {
            Ok(SessionRow { hash: r.get(0)?, created: r.get(1)?, expires: r.get(2)?, agent: r.get(3)? })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Sign out one session of the account's, by the start of its hash.
    /// How many went.
    pub fn drop_session_of(&self, account: &str, hash_prefix: &str) -> anyhow::Result<usize> {
        if hash_prefix.len() < 16 || !hash_prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(0);
        }
        Ok(self.c().execute(
            "DELETE FROM sessions WHERE account = ?1 AND substr(token_hash, 1, ?3) = ?2",
            params![account, hash_prefix, hash_prefix.len()],
        )?)
    }

    /// Sign out everywhere.
    pub fn drop_sessions(&self, account: &str) -> anyhow::Result<usize> {
        Ok(self.c().execute("DELETE FROM sessions WHERE account = ?1", params![account])?)
    }

    /// Forget what has expired: sessions, join codes, invites. How many
    /// sessions went.
    pub fn prune(&self, now: u64) -> anyhow::Result<usize> {
        let c = self.c();
        let n = c.execute("DELETE FROM sessions WHERE expires <= ?1", params![now])?;
        c.execute("DELETE FROM joins WHERE created < ?1", params![now.saturating_sub(JOIN_TTL_MS)])?;
        c.execute("DELETE FROM invites WHERE expires <= ?1", params![now])?;
        c.execute("DELETE FROM presigned_invites WHERE expires <= ?1", params![now])?;
        // A notice nobody came back for in a month isn't news any more.
        c.execute("DELETE FROM notices WHERE created < ?1", params![now.saturating_sub(NOTICE_TTL_MS)])?;
        Ok(n)
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

    /// Where a new passkey came from: the browser that added it and,
    /// when its AAGUID is a known one, what keeps it.
    pub fn note_passkey(&self, id: &str, agent: &str, provider: Option<&str>) -> anyhow::Result<()> {
        self.c()
            .execute("UPDATE passkeys SET agent = ?2, provider = ?3 WHERE id = ?1", params![id, agent, provider])?;
        Ok(())
    }

    pub fn passkey_used(&self, id: &str, count: u32, now: u64) -> anyhow::Result<()> {
        self.c().execute("UPDATE passkeys SET sign_count = ?2, used = ?3 WHERE id = ?1", params![id, count, now])?;
        Ok(())
    }

    pub fn passkey_count(&self, account: &str) -> anyhow::Result<u64> {
        Ok(self.c().query_row("SELECT COUNT(*) FROM passkeys WHERE account = ?1", params![account], |r| r.get(0))?)
    }

    /// An account's passkeys, oldest first.
    pub fn passkeys(&self, account: &str) -> anyhow::Result<Vec<PasskeyRow>> {
        let c = self.c();
        let mut q =
            c.prepare("SELECT id, created, agent, provider, used FROM passkeys WHERE account = ?1 ORDER BY created")?;
        let rows = q.query_map(params![account], |r| {
            Ok(PasskeyRow {
                id: r.get(0)?,
                created: r.get(1)?,
                agent: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                provider: r.get(3)?,
                used: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Ways to sign in to an account: its passkeys and its GitHub link.
    pub fn sign_ins(&self, account: &str) -> anyhow::Result<u64> {
        Ok(self.c().query_row(
            "SELECT (SELECT COUNT(*) FROM passkeys WHERE account = ?1)
                  + (SELECT COUNT(*) FROM identities WHERE account = ?1 AND provider = 'github')",
            params![account],
            |r| r.get(0),
        )?)
    }

    /// Remove a passkey, unless it's the account's last way to sign in
    /// (checked in the same transaction). Whether it went.
    pub fn drop_passkey(&self, account: &str, id: &str) -> anyhow::Result<Result<(), &'static str>> {
        let mut c = self.c();
        let tx = c.transaction()?;
        let ways: u64 = tx.query_row(
            "SELECT (SELECT COUNT(*) FROM passkeys WHERE account = ?1)
                  + (SELECT COUNT(*) FROM identities WHERE account = ?1 AND provider = 'github')",
            params![account],
            |r| r.get(0),
        )?;
        let n = tx.execute("DELETE FROM passkeys WHERE id = ?2 AND account = ?1", params![account, id])?;
        if n == 0 {
            return Ok(Err("no such passkey"));
        }
        if ways <= 1 {
            return Ok(Err(
                "that's the only way to sign in to this account: add another passkey (or sign in with GitHub) first",
            ));
        }
        // A passkey that made the account is its sign-in identity too.
        tx.execute(
            "DELETE FROM identities WHERE provider = 'passkey' AND subject = ?2 AND account = ?1",
            params![account, id],
        )?;
        tx.commit()?;
        Ok(Ok(()))
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
        // Asking again (or being approved) clears a turn-down.
        tx.execute("DELETE FROM turned_down WHERE account = ?1 AND device = ?2", params![cert.account, cert.device])?;
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
        self.approved_cert(id, Kind::Daemon)
    }

    /// An approved device of this kind, by its id.
    pub fn approved_cert(&self, id: &str, kind: Kind) -> anyhow::Result<Option<Cert>> {
        Ok(self
            .c()
            .query_row(
                "SELECT cert FROM devices WHERE id = ?1 AND kind = ?2 AND approved = 1",
                params![id, kind.as_str()],
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

    /// Turn a pending device down, remembering which device did (by name)
    /// so the one waiting can say so.
    pub fn turn_down(&self, account: &str, id: &str, by_name: &str, now: u64) -> anyhow::Result<()> {
        let mut c = self.c();
        let tx = c.transaction()?;
        let n =
            tx.execute("DELETE FROM devices WHERE account = ?1 AND id = ?2 AND approved = 0", params![account, id])?;
        if n > 0 {
            tx.execute(
                "INSERT INTO turned_down (account, device, by_name, at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (account, device) DO UPDATE SET by_name = excluded.by_name, at = excluded.at",
                params![account, id, by_name, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Who turned this device down (a device's name, maybe empty), if it was.
    pub fn turned_down(&self, account: &str, id: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .c()
            .query_row(
                "SELECT by_name FROM turned_down WHERE account = ?1 AND device = ?2",
                params![account, id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn add_revocation(&self, r: &Revocation) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO revocations (account, device, body) VALUES (?1, ?2, ?3)",
            params![r.account, r.device, serde_json::to_string(r)?],
        )?;
        Ok(())
    }

    /// A revocation of a machine of `r.account`'s own (a `daemons` row of
    /// that account when it was revoked): see [`Db::revoked`].
    pub fn add_machine_revocation(&self, r: &Revocation) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO revocations (account, device, body, machine) VALUES (?1, ?2, ?3, 1)",
            params![r.account, r.device, serde_json::to_string(r)?],
        )?;
        Ok(())
    }

    /// Whether `device` is `account`'s: one of its devices (approved or
    /// asking), one of its machines, or a join it approved that the
    /// machine hasn't collected yet. What it may revoke.
    pub fn is_accounts(&self, account: &str, device: &str) -> anyhow::Result<bool> {
        if self.device(account, device)?.is_some() || self.daemon_account(device)?.as_deref() == Some(account) {
            return Ok(true);
        }
        let c = self.c();
        let mut q = c.prepare("SELECT cert FROM joins WHERE account = ?1")?;
        let certs = q.query_map(params![account], |r| r.get::<_, String>(0))?;
        for cert in certs {
            if cert_of(cert?)?.device == device {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn revocations(&self, account: &str) -> anyhow::Result<Vec<Revocation>> {
        let c = self.c();
        let mut q = c.prepare("SELECT body FROM revocations WHERE account = ?1")?;
        let rows = q.query_map(params![account], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    /// The first time `device` was removed as one of its account's
    /// machines (#330), if it was: that key never joins or signs again.
    ///
    /// This looks across accounts, so only machine revocations count: ones
    /// whose account had the key as a machine (a `daemons` row) when it
    /// revoked it. That row comes only from a join, and a join for a key
    /// control knows needs the key's proof, so the account held the key.
    /// Any other revocation (a device row anyone can make with someone
    /// else's public keys, through `enroll`) keeps it out of that account
    /// only, which `Trust::evaluate` does.
    pub fn revoked(&self, device: &str) -> anyhow::Result<Option<Revocation>> {
        let c = self.c();
        let mut q = c.prepare("SELECT body FROM revocations WHERE device = ?1 AND machine = 1")?;
        let rows = q.query_map(params![device], |r| r.get::<_, String>(0))?;
        let mut first: Option<Revocation> = None;
        for r in rows {
            let r: Revocation = serde_json::from_str(&r?)?;
            if first.as_ref().is_none_or(|f| r.at < f.at) {
                first = Some(r);
            }
        }
        Ok(first)
    }

    // ---- joins

    /// Ask for a join code. A code is the machine's key's, so a second
    /// request from the machine finds the first one's row (#329). Proven to
    /// hold the key (or both unproven, as older daemons ask), it takes the
    /// row over: the first requester's polls are told so, and an approval
    /// already given stays, for this request to collect. Otherwise
    /// [`Asked::Taken`].
    #[allow(clippy::too_many_arguments)]
    pub fn add_join(
        &self,
        code: &str,
        cert: &Cert,
        poll_hash: &str,
        urls: &[String],
        team: Option<&str>,
        sandbox: Option<&str>,
        features: &str,
        proven: bool,
        now: u64,
    ) -> anyhow::Result<Asked> {
        let mut c = self.c();
        let tx = c.transaction()?;
        // Old ones go first; a code can be asked for again.
        tx.execute("DELETE FROM joins WHERE created < ?1", params![now.saturating_sub(JOIN_TTL_MS)])?;
        let had: Option<(String, Option<String>, bool, String)> = tx
            .query_row(
                "SELECT poll_hash, account, proven, replaced FROM joins WHERE code = ?1 AND rejected IS NULL",
                params![code],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        if let Some((old, account, was_proven, replaced)) = had {
            if !proven && (was_proven || account.is_some()) {
                return Ok(Asked::Taken);
            }
            let replaced = format!("{replaced} {old}").trim().to_owned();
            if account.is_some() {
                // Approved: the approval (and the approver's team) stays.
                tx.execute(
                    "UPDATE joins SET poll_hash = ?2, replaced = ?3 WHERE code = ?1",
                    params![code, poll_hash, replaced],
                )?;
            } else {
                tx.execute(
                    "UPDATE joins SET cert = ?2, poll_hash = ?3, urls = ?4, created = ?5, team = ?6, sandbox = ?7,
                     features = ?8, proven = ?9, replaced = ?10 WHERE code = ?1",
                    params![
                        code,
                        serde_json::to_string(cert)?,
                        poll_hash,
                        serde_json::to_string(urls)?,
                        now,
                        team,
                        sandbox,
                        features,
                        proven,
                        replaced
                    ],
                )?;
            }
            tx.commit()?;
            return Ok(Asked::Waiting);
        }
        tx.execute(
            "INSERT OR REPLACE INTO joins (code, cert, poll_hash, urls, created, account, team, sandbox, features, proven)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8, ?9)",
            params![
                code,
                serde_json::to_string(cert)?,
                poll_hash,
                serde_json::to_string(urls)?,
                now,
                team,
                sandbox,
                features,
                proven
            ],
        )?;
        tx.commit()?;
        Ok(Asked::Waiting)
    }

    pub fn join(&self, code: &str, now: u64) -> anyhow::Result<Option<Join>> {
        Ok(self
            .c()
            .query_row(
                "SELECT cert, poll_hash, urls, created, account, team, sandbox, team_sig, rejected,
                 COALESCE(features, ''), proven, replaced FROM joins
                 WHERE code = ?1 AND created >= ?2",
                params![code, now.saturating_sub(JOIN_TTL_MS)],
                |r| {
                    Ok(Join {
                        cert: cert_of(r.get(0)?)?,
                        poll_hash: r.get(1)?,
                        urls: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                        created: r.get(3)?,
                        account: r.get(4)?,
                        team: r.get(5)?,
                        sandbox: r.get(6)?,
                        team_sig: r.get(7)?,
                        rejected: r.get(8)?,
                        features: r.get(9)?,
                        proven: r.get(10)?,
                        replaced: r.get(11)?,
                    })
                },
            )
            .optional()?)
    }

    /// Approved: the signed certificate replaces the request, and the team
    /// the approver chose (with their signature) replaces the one asked for.
    pub fn approve_join(&self, code: &str, cert: &Cert, team: Option<(&str, &str)>) -> anyhow::Result<()> {
        self.c().execute(
            "UPDATE joins SET cert = ?2, account = ?3, team = ?4, team_sig = ?5 WHERE code = ?1",
            params![code, serde_json::to_string(cert)?, cert.account, team.map(|t| t.0), team.map(|t| t.1)],
        )?;
        Ok(())
    }

    /// The machine's join code a waiting device came to approve (#326), or
    /// none: the device that approves it sees both together.
    pub fn set_device_join(&self, account: &str, id: &str, code: Option<&str>) -> anyhow::Result<()> {
        self.c().execute(
            "UPDATE devices SET join_code = ?3 WHERE account = ?1 AND id = ?2 AND approved = 0",
            params![account, id, code],
        )?;
        Ok(())
    }

    /// Waiting devices of an account with the join code each came to
    /// approve, while that join is still open (not approved, turned down,
    /// collected or expired): (device, code).
    pub fn device_joins(&self, account: &str, now: u64) -> anyhow::Result<Vec<(String, String)>> {
        let c = self.c();
        let mut q = c.prepare(
            "SELECT d.id, d.join_code FROM devices d JOIN joins j ON j.code = d.join_code
             WHERE d.account = ?1 AND d.approved = 0 AND j.account IS NULL AND j.rejected IS NULL
             AND j.created >= ?2",
        )?;
        let rows = q.query_map(params![account, now.saturating_sub(JOIN_TTL_MS)], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Turned down: the daemon learns it on its next poll.
    pub fn reject_join(&self, code: &str, on: &str) -> anyhow::Result<()> {
        self.c().execute("UPDATE joins SET rejected = ?2 WHERE code = ?1", params![code, on])?;
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
        // What it shared, and the answers: a machine that joins again (to
        // another account, say) asks again.
        c.execute("DELETE FROM daemon_access WHERE daemon = ?1", params![id])?;
        c.execute("DELETE FROM daemon_links WHERE daemon = ?1", params![id])?;
        c.execute("DELETE FROM daemon_offers WHERE daemon = ?1", params![id])?;
        c.execute("DELETE FROM daemon_offer_pushes WHERE daemon = ?1", params![id])?;
        c.execute("DELETE FROM share_answers WHERE daemon = ?1", params![id])?;
        Ok(())
    }

    /// Whether this id is a daemon here already, or a device of any
    /// account's (what a join may not take over without its key).
    pub fn device_known(&self, id: &str) -> anyhow::Result<bool> {
        let c = self.c();
        let d: bool = c.query_row("SELECT EXISTS (SELECT 1 FROM daemons WHERE id = ?1)", params![id], |r| r.get(0))?;
        let v: bool = c.query_row("SELECT EXISTS (SELECT 1 FROM devices WHERE id = ?1)", params![id], |r| r.get(0))?;
        Ok(d || v)
    }

    // ---- shares offered to people, and their answers

    /// The accounts a daemon would share with that haven't said yes yet:
    /// these replace the ones it named before. The ones new since then.
    pub fn offer_shares(&self, daemon: &str, accounts: &[String], now: u64) -> anyhow::Result<Vec<String>> {
        let mut c = self.c();
        let tx = c.transaction()?;
        let had: Vec<String> = {
            let mut q = tx.prepare("SELECT account FROM daemon_offers WHERE daemon = ?1")?;
            let rows = q.query_map(params![daemon], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        tx.execute("DELETE FROM daemon_offers WHERE daemon = ?1", params![daemon])?;
        let mut new = Vec::new();
        for a in accounts {
            tx.execute(
                "INSERT OR IGNORE INTO daemon_offers (daemon, account, since) VALUES (?1, ?2, ?3)",
                params![daemon, a, now],
            )?;
            if !had.contains(a) {
                new.push(a.clone());
            }
        }
        tx.commit()?;
        Ok(new)
    }

    /// Whether to push `account` of `daemon`'s offer (#232): once a day at
    /// most, however often the daemon drops and makes it again.
    pub fn push_offer(&self, daemon: &str, account: &str, now: u64) -> anyhow::Result<bool> {
        const DAY: u64 = 24 * 3600 * 1000;
        let n = self.c().execute(
            "INSERT INTO daemon_offer_pushes (daemon, account, at) VALUES (?1, ?2, ?3)
             ON CONFLICT (daemon, account) DO UPDATE SET at = excluded.at WHERE at <= excluded.at - ?4",
            params![daemon, account, now, DAY],
        )?;
        Ok(n > 0)
    }

    /// Daemons offering to share with this account that it hasn't answered.
    pub fn offers_for(&self, account: &str) -> anyhow::Result<Vec<String>> {
        let c = self.c();
        let mut q = c.prepare(
            "SELECT o.daemon FROM daemon_offers o
             WHERE o.account = ?1
               AND NOT EXISTS (SELECT 1 FROM share_answers s WHERE s.account = o.account AND s.daemon = o.daemon)
             ORDER BY o.since",
        )?;
        let rows = q.query_map(params![account], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn offered(&self, daemon: &str, account: &str) -> anyhow::Result<bool> {
        Ok(self.c().query_row(
            "SELECT EXISTS (SELECT 1 FROM daemon_offers WHERE daemon = ?1 AND account = ?2)
                 OR EXISTS (SELECT 1 FROM daemon_access WHERE daemon = ?1 AND account = ?2)",
            params![daemon, account],
            |r| r.get(0),
        )?)
    }

    /// Whether `account` accepted (`Some(true)`) or turned down a share
    /// from `daemon`.
    pub fn share_answer(&self, account: &str, daemon: &str) -> anyhow::Result<Option<bool>> {
        Ok(self
            .c()
            .query_row(
                "SELECT accepted FROM share_answers WHERE account = ?1 AND daemon = ?2",
                params![account, daemon],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn answer_share(&self, account: &str, daemon: &str, accepted: bool, now: u64) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT OR REPLACE INTO share_answers (account, daemon, accepted, at) VALUES (?1, ?2, ?3, ?4)",
            params![account, daemon, accepted, now],
        )?;
        Ok(())
    }

    // ---- people and teams (M19)

    /// An account by its sign-in login (to share with a person).
    pub fn account_by_login(&self, login: &str) -> anyhow::Result<Option<Account>> {
        if login.is_empty() {
            return Ok(None);
        }
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

    /// Accounts that chose this display name (#102), a few at most.
    pub fn accounts_named(&self, name: &str) -> anyhow::Result<Vec<String>> {
        let c = self.c();
        let mut q = c.prepare("SELECT id FROM accounts WHERE name != '' AND lower(name) = lower(?1) LIMIT 2")?;
        let rows = q.query_map(params![name], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
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

    /// Fails with [`Taken`] when that version is already there.
    pub fn add_roster(&self, team: &str, version: u64, body: &str) -> anyhow::Result<()> {
        let c = self.c();
        c.execute("INSERT INTO rosters (team, version, body) VALUES (?1, ?2, ?3)", params![team, version, body])
            .map_err(taken)?;
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

    /// Who made a live invite (#103): (team, their account).
    pub fn invite_by(&self, code_hash: &str, now: u64) -> anyhow::Result<Option<(String, String)>> {
        Ok(self
            .c()
            .query_row(
                "SELECT team, by_account FROM invites WHERE code_hash = ?1 AND expires > ?2",
                params![code_hash, now],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// A presigned invite (its one-time key's public half names it), as the
    /// owner's device signed it. Fails with [`Taken`] for a key already used.
    pub fn add_presigned(&self, key: &str, team: &str, body: &str, expires: u64, by: &str) -> anyhow::Result<()> {
        self.c()
            .execute(
                "INSERT INTO presigned_invites (key, team, body, expires, by_account) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![key, team, body, expires, by],
            )
            .map_err(taken)?;
        Ok(())
    }

    /// (team, invite JSON, who made it) for a live presigned invite.
    pub fn presigned(&self, key: &str, now: u64) -> anyhow::Result<Option<(String, String, String)>> {
        Ok(self
            .c()
            .query_row(
                "SELECT team, body, by_account FROM presigned_invites WHERE key = ?1 AND expires > ?2",
                params![key, now],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?)
    }

    /// A team's live presigned invites, soonest to expire first: (key,
    /// invite JSON, expires, who made it).
    pub fn presigned_of(&self, team: &str, now: u64) -> anyhow::Result<Vec<(String, String, u64, String)>> {
        let c = self.c();
        let mut st = c.prepare(
            "SELECT key, body, expires, by_account FROM presigned_invites WHERE team = ?1 AND expires > ?2
             ORDER BY expires, key",
        )?;
        let rows = st.query_map(params![team, now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn drop_presigned(&self, key: &str) -> anyhow::Result<()> {
        self.c().execute("DELETE FROM presigned_invites WHERE key = ?1", params![key])?;
        Ok(())
    }

    pub fn drop_invites(&self, team: &str) -> anyhow::Result<()> {
        let c = self.c();
        c.execute("DELETE FROM invites WHERE team = ?1", params![team])?;
        c.execute("DELETE FROM presigned_invites WHERE team = ?1", params![team])?;
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

    /// The teams `account` asked to join, waiting on an owner (#103).
    pub fn asked(&self, account: &str) -> anyhow::Result<Vec<String>> {
        let c = self.c();
        let mut q = c.prepare("SELECT team FROM team_requests WHERE account = ?1 ORDER BY created")?;
        let rows = q.query_map(params![account], |r| r.get(0))?;
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

    /// A machine moved after it joined: its team (none for its account)
    /// and the signed move its daemon checks.
    pub fn move_daemon(&self, daemon: &str, team: Option<&str>, moved: &str) -> anyhow::Result<()> {
        self.c().execute("UPDATE daemons SET team = ?2, moved = ?3 WHERE id = ?1", params![daemon, team, moved])?;
        Ok(())
    }

    /// The last signed move of a machine, if it ever moved.
    pub fn daemon_moved(&self, daemon: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .c()
            .query_row("SELECT moved FROM daemons WHERE id = ?1", params![daemon], |r| r.get(0))
            .optional()?
            .flatten())
    }

    pub fn daemon_team(&self, daemon: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .c()
            .query_row("SELECT team FROM daemons WHERE id = ?1", params![daemon], |r| r.get(0))
            .optional()?
            .flatten())
    }

    /// What a daemon said it understands, on its last team call (an older
    /// daemon says nothing).
    pub fn set_daemon_features(&self, daemon: &str, features: &str) -> anyhow::Result<()> {
        self.c().execute("UPDATE daemons SET features = ?2 WHERE id = ?1", params![daemon, features])?;
        Ok(())
    }

    /// What a daemon last said it understands ("" for an older one).
    pub fn daemon_features(&self, daemon: &str) -> anyhow::Result<String> {
        Ok(self
            .c()
            .query_row("SELECT COALESCE(features, '') FROM daemons WHERE id = ?1", params![daemon], |r| r.get(0))
            .optional()?
            .unwrap_or_default())
    }

    /// A daemon checks this team's rosters because a session was shared
    /// with it (M30), not because it's the team's.
    pub fn watch_team(&self, daemon: &str, team: &str, now: u64) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO daemon_watches (daemon, team, seen) VALUES (?1, ?2, ?3)
             ON CONFLICT (daemon, team) DO UPDATE SET seen = excluded.seen",
            params![daemon, team, now],
        )?;
        Ok(())
    }

    /// Every daemon that checks this team's rosters: its own machines, and
    /// those that asked about it since `since`. (name, features) each.
    pub fn team_followers(&self, team: &str, since: u64) -> anyhow::Result<Vec<(String, String)>> {
        let c = self.c();
        let mut q = c.prepare(
            "SELECT name, COALESCE(features, '') FROM daemons WHERE team = ?1
             OR id IN (SELECT daemon FROM daemon_watches WHERE team = ?1 AND seen > ?2) ORDER BY name",
        )?;
        let rows = q.query_map(params![team, since], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
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

    // ---- billing (M22)

    pub fn billing(&self, owner: &str) -> anyhow::Result<Option<BillingRow>> {
        Ok(self
            .c()
            .query_row(
                "SELECT customer, subscription, status, seat_item FROM billing WHERE owner = ?1",
                params![owner],
                |r| {
                    Ok(BillingRow {
                        customer: r.get(0)?,
                        subscription: r.get(1)?,
                        status: r.get(2)?,
                        seat_item: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn set_billing(
        &self,
        owner: &str,
        customer: Option<&str>,
        subscription: Option<&str>,
        status: &str,
        now: u64,
    ) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO billing (owner, customer, subscription, status, updated) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (owner) DO UPDATE SET customer = COALESCE(excluded.customer, customer),
               subscription = COALESCE(excluded.subscription, subscription), status = excluded.status, updated = excluded.updated",
            params![owner, customer, subscription, status, now],
        )?;
        Ok(())
    }

    pub fn set_seat_item(&self, owner: &str, item: &str) -> anyhow::Result<()> {
        self.c().execute("UPDATE billing SET seat_item = ?2 WHERE owner = ?1", params![owner, item])?;
        Ok(())
    }

    /// Relay bytes this month (`YYYY-MM`).
    pub fn relay_bytes_month(&self, account: &str, month: &str) -> anyhow::Result<u64> {
        Ok(self.c().query_row(
            "SELECT COALESCE(SUM(relay_bytes), 0) FROM usage WHERE account = ?1 AND day LIKE ?2",
            params![account, format!("{month}-%")],
            |r| r.get(0),
        )?)
    }

    /// Accounts that ever had a sandbox.
    pub fn sandbox_accounts(&self) -> anyhow::Result<Vec<String>> {
        let c = self.c();
        let mut q = c.prepare("SELECT DISTINCT account FROM sandboxes")?;
        let rows = q.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn reported_minutes(&self, account: &str) -> anyhow::Result<u64> {
        Ok(self
            .c()
            .query_row("SELECT minutes FROM reported WHERE account = ?1", params![account], |r| r.get(0))
            .optional()?
            .unwrap_or(0))
    }

    pub fn set_reported_minutes(&self, account: &str, minutes: u64) -> anyhow::Result<()> {
        self.c()
            .execute("INSERT OR REPLACE INTO reported (account, minutes) VALUES (?1, ?2)", params![account, minutes])?;
        Ok(())
    }

    // ---- hosted sandboxes (M20)

    pub fn add_sandbox(
        &self,
        id: &str,
        account: &str,
        device: &str,
        ticket_hash: &str,
        now: u64,
    ) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO sandboxes (id, account, device, ticket_hash, state, created) VALUES (?1, ?2, ?3, ?4, 'creating', ?5)",
            params![id, account, device, ticket_hash, now],
        )?;
        Ok(())
    }

    pub fn set_sandbox_state(&self, id: &str, state: &str) -> anyhow::Result<()> {
        self.c().execute("UPDATE sandboxes SET state = ?2 WHERE id = ?1", params![id, state])?;
        Ok(())
    }

    fn sandbox_row(r: &rusqlite::Row) -> rusqlite::Result<SandboxRow> {
        Ok(SandboxRow {
            id: r.get(0)?,
            account: r.get(1)?,
            device: r.get(2)?,
            state: r.get(3)?,
            created: r.get(4)?,
            deleted: r.get(5)?,
        })
    }

    pub fn sandboxes(&self, account: &str) -> anyhow::Result<Vec<SandboxRow>> {
        let c = self.c();
        let mut q = c.prepare(
            "SELECT id, account, device, state, created, deleted FROM sandboxes WHERE account = ?1 ORDER BY created",
        )?;
        let rows = q.query_map(params![account], Self::sandbox_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn sandbox(&self, id: &str) -> anyhow::Result<Option<SandboxRow>> {
        Ok(self
            .c()
            .query_row(
                "SELECT id, account, device, state, created, deleted FROM sandboxes WHERE id = ?1",
                params![id],
                Self::sandbox_row,
            )
            .optional()?)
    }

    /// The sandbox a join ticket belongs to (before its daemon joined).
    pub fn sandbox_by_ticket(&self, ticket_hash: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .c()
            .query_row(
                "SELECT id FROM sandboxes WHERE ticket_hash = ?1 AND daemon IS NULL AND deleted IS NULL",
                params![ticket_hash],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// A join waiting for a sandbox's requester: its code and certificate.
    pub fn sandbox_join(&self, sandbox: &str, now: u64) -> anyhow::Result<Option<(String, serde_json::Value)>> {
        Ok(self
            .c()
            .query_row(
                "SELECT code, cert FROM joins WHERE sandbox = ?1 AND account IS NULL AND created >= ?2",
                params![sandbox, now.saturating_sub(JOIN_TTL_MS)],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
            .map(|(code, cert)| (code, serde_json::from_str(&cert).unwrap_or_default())))
    }

    pub fn set_sandbox_daemon(&self, id: &str, daemon: &str) -> anyhow::Result<()> {
        self.c().execute("UPDATE sandboxes SET daemon = ?2, state = 'running' WHERE id = ?1", params![id, daemon])?;
        Ok(())
    }

    pub fn sandbox_daemon(&self, id: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .c()
            .query_row("SELECT daemon FROM sandboxes WHERE id = ?1", params![id], |r| r.get(0))
            .optional()?
            .flatten())
    }

    /// The live sandbox a daemon is in, if it is one.
    pub fn sandbox_of_daemon(&self, daemon: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .c()
            .query_row("SELECT id FROM sandboxes WHERE daemon = ?1 AND deleted IS NULL", params![daemon], |r| r.get(0))
            .optional()?)
    }

    pub fn end_sandbox(&self, id: &str, now: u64) -> anyhow::Result<()> {
        self.c().execute("UPDATE sandboxes SET deleted = ?2, state = 'deleted' WHERE id = ?1", params![id, now])?;
        Ok(())
    }

    /// Sandbox minutes (M22): from creation to deletion (or now).
    pub fn sandbox_minutes(&self, account: &str, since: u64, now: u64) -> anyhow::Result<u64> {
        let c = self.c();
        let mut q = c.prepare(
            "SELECT created, deleted FROM sandboxes WHERE account = ?1 AND (deleted IS NULL OR deleted > ?2)",
        )?;
        let rows = q.query_map(params![account, since], |r| Ok((r.get::<_, u64>(0)?, r.get::<_, Option<u64>>(1)?)))?;
        let mut ms = 0u64;
        for row in rows {
            let (start, end) = row?;
            ms += end.unwrap_or(now).saturating_sub(start.max(since));
        }
        Ok(ms.div_ceil(60_000))
    }

    // ---- settings and push (M21)

    pub fn setting(&self, k: &str) -> anyhow::Result<Option<String>> {
        Ok(self.c().query_row("SELECT v FROM settings WHERE k = ?1", params![k], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, k: &str, v: &str) -> anyhow::Result<()> {
        self.c().execute("INSERT OR REPLACE INTO settings (k, v) VALUES (?1, ?2)", params![k, v])?;
        Ok(())
    }

    pub fn put_push_sub(&self, endpoint: &str, account: &str, body: &str) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT OR REPLACE INTO push_subs (endpoint, account, body) VALUES (?1, ?2, ?3)",
            params![endpoint, account, body],
        )?;
        Ok(())
    }

    pub fn drop_push_sub(&self, endpoint: &str, account: Option<&str>) -> anyhow::Result<()> {
        match account {
            Some(a) => {
                self.c().execute("DELETE FROM push_subs WHERE endpoint = ?1 AND account = ?2", params![endpoint, a])?
            }
            None => self.c().execute("DELETE FROM push_subs WHERE endpoint = ?1", params![endpoint])?,
        };
        Ok(())
    }

    pub fn push_subs(&self, account: &str) -> anyhow::Result<Vec<serde_json::Value>> {
        let c = self.c();
        let mut q = c.prepare("SELECT body FROM push_subs WHERE account = ?1")?;
        let rows = q.query_map(params![account], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    pub fn push_sub_account(&self, endpoint: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .c()
            .query_row("SELECT account FROM push_subs WHERE endpoint = ?1", params![endpoint], |r| r.get(0))
            .optional()?)
    }

    /// The accounts a daemon said it lets in.
    pub fn daemon_accounts(&self, daemon: &str) -> anyhow::Result<Vec<String>> {
        let c = self.c();
        let mut q = c.prepare("SELECT account FROM daemon_access WHERE daemon = ?1")?;
        let rows = q.query_map(params![daemon], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    // ---- deleting an account (#173)

    /// Every team's id.
    pub fn team_ids(&self) -> anyhow::Result<Vec<String>> {
        let c = self.c();
        let mut q = c.prepare("SELECT id FROM teams")?;
        let rows = q.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The certificates a deleted account left for checking teams' signed
    /// history: (approved certificates, revocations).
    pub fn retained_certs(&self, account: &str) -> anyhow::Result<Option<(Vec<Cert>, Vec<Revocation>)>> {
        let row: Option<(String, String)> = self
            .c()
            .query_row("SELECT certs, revocations FROM retained_certs WHERE account = ?1", params![account], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        match row {
            Some((c, r)) => Ok(Some((serde_json::from_str(&c)?, serde_json::from_str(&r)?))),
            None => Ok(None),
        }
    }

    /// Delete an account and everything that's only its, in one
    /// transaction. Returns its machines' ids (to hang up on).
    pub fn delete_account(&self, e: &Erase) -> anyhow::Result<Vec<String>> {
        let a = e.account;
        let mut c = self.c();
        let tx = c.transaction()?;
        let daemons: Vec<String> = {
            let mut q = tx.prepare("SELECT id FROM daemons WHERE account = ?1")?;
            q.query_map(params![a], |r| r.get(0))?.collect::<Result<_, _>>()?
        };
        if !e.retain_for.is_empty() {
            // Only devices that can sign: what a roster's signature chains to.
            let certs: Vec<Cert> = {
                let mut q = tx.prepare(
                    "SELECT cert FROM devices WHERE account = ?1 AND approved = 1 AND kind != 'daemon' ORDER BY created",
                )?;
                q.query_map(params![a], |r| cert_of(r.get(0)?))?.collect::<Result<_, _>>()?
            };
            let revs: Vec<Revocation> = {
                let mut q = tx.prepare("SELECT body FROM revocations WHERE account = ?1")?;
                let bodies: Vec<String> = q.query_map(params![a], |r| r.get(0))?.collect::<Result<_, _>>()?;
                bodies.iter().map(|b| serde_json::from_str(b)).collect::<Result<_, _>>()?
            };
            tx.execute(
                "INSERT OR REPLACE INTO retained_certs (account, certs, revocations) VALUES (?1, ?2, ?3)",
                params![a, serde_json::to_string(&certs)?, serde_json::to_string(&revs)?],
            )?;
            for t in e.retain_for {
                tx.execute("INSERT OR IGNORE INTO retained_for (account, team) VALUES (?1, ?2)", params![a, t])?;
            }
        }
        for t in e.disband {
            for sql in [
                "DELETE FROM rosters WHERE team = ?1",
                "DELETE FROM invites WHERE team = ?1",
                "DELETE FROM presigned_invites WHERE team = ?1",
                "DELETE FROM team_requests WHERE team = ?1",
                "DELETE FROM daemon_watches WHERE team = ?1",
                "DELETE FROM retained_for WHERE team = ?1",
                "DELETE FROM billing WHERE owner = 'team:' || ?1",
                "UPDATE daemons SET team = NULL, moved = NULL WHERE team = ?1",
                "UPDATE joins SET team = NULL, team_sig = NULL WHERE team = ?1",
                "DELETE FROM teams WHERE id = ?1",
            ] {
                tx.execute(sql, params![t])?;
            }
        }
        // Certificates kept for teams that are gone now.
        tx.execute("DELETE FROM retained_certs WHERE account NOT IN (SELECT account FROM retained_for)", [])?;
        for d in &daemons {
            tx.execute(
                "INSERT OR REPLACE INTO gone_daemons (hash, at) VALUES (?1, ?2)",
                params![gone_hash(d), illogical_e2e::now_ms()],
            )?;
            for sql in [
                "DELETE FROM daemon_access WHERE daemon = ?1",
                "DELETE FROM daemon_links WHERE daemon = ?1",
                "DELETE FROM daemon_watches WHERE daemon = ?1",
                "DELETE FROM daemon_offers WHERE daemon = ?1",
                "DELETE FROM daemon_offer_pushes WHERE daemon = ?1",
                "DELETE FROM share_answers WHERE daemon = ?1",
            ] {
                tx.execute(sql, params![d])?;
            }
        }
        for sql in [
            "DELETE FROM identities WHERE account = ?1",
            "DELETE FROM sessions WHERE account = ?1",
            "DELETE FROM passkeys WHERE account = ?1",
            "DELETE FROM devices WHERE account = ?1",
            "DELETE FROM turned_down WHERE account = ?1",
            "DELETE FROM revocations WHERE account = ?1",
            "DELETE FROM joins WHERE account = ?1",
            "DELETE FROM daemons WHERE account = ?1",
            "DELETE FROM daemon_access WHERE account = ?1",
            "DELETE FROM daemon_offers WHERE account = ?1",
            "DELETE FROM daemon_offer_pushes WHERE account = ?1",
            "DELETE FROM share_answers WHERE account = ?1",
            "DELETE FROM push_subs WHERE account = ?1",
            "DELETE FROM notices WHERE account = ?1",
            "DELETE FROM sandboxes WHERE account = ?1",
            "DELETE FROM billing WHERE owner = 'account:' || ?1",
            "DELETE FROM reported WHERE account = ?1",
            "DELETE FROM usage WHERE account = ?1",
            "DELETE FROM team_requests WHERE account = ?1",
            "DELETE FROM invites WHERE by_account = ?1",
            "DELETE FROM presigned_invites WHERE by_account = ?1",
            "DELETE FROM accounts WHERE id = ?1",
        ] {
            tx.execute(sql, params![a])?;
        }
        tx.commit()?;
        Ok(daemons)
    }

    /// Whether `device` was a machine of an account that's been deleted.
    pub fn daemon_account_deleted(&self, device: &str) -> anyhow::Result<bool> {
        Ok(self
            .c()
            .query_row("SELECT 1 FROM gone_daemons WHERE hash = ?1", params![gone_hash(device)], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Every row in every table that mentions `needle`, as `table.column`
    /// (tests: nothing of a deleted account is left).
    #[cfg(test)]
    pub fn mentions(&self, needle: &str) -> Vec<String> {
        let c = self.c();
        let tables: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let mut out = Vec::new();
        for t in tables {
            let cols: Vec<String> = c
                .prepare(&format!("PRAGMA table_info({t})"))
                .unwrap()
                .query_map([], |r| r.get(1))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            for col in cols {
                let n: u64 = c
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {t} WHERE CAST({col} AS TEXT) LIKE '%' || ?1 || '%'"),
                        params![needle],
                        |r| r.get(0),
                    )
                    .unwrap();
                if n > 0 {
                    out.push(format!("{t}.{col}"));
                }
            }
        }
        out
    }

    // ---- notices

    /// Something to show the account in the app once (#206: a team it was
    /// in was deleted), beside the push notification it may not get.
    pub fn add_notice(&self, account: &str, title: &str, body: &str, now: u64) -> anyhow::Result<()> {
        self.c().execute(
            "INSERT INTO notices (account, title, body, created) VALUES (?1, ?2, ?3, ?4)",
            params![account, title, body, now],
        )?;
        Ok(())
    }

    /// (id, title, body), oldest first.
    pub fn notices(&self, account: &str) -> anyhow::Result<Vec<(i64, String, String)>> {
        let c = self.c();
        let mut q = c.prepare("SELECT id, title, body FROM notices WHERE account = ?1 ORDER BY id")?;
        let rows = q.query_map(params![account], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Seen: whether it was this account's.
    pub fn drop_notice(&self, account: &str, id: i64) -> anyhow::Result<bool> {
        Ok(self.c().execute("DELETE FROM notices WHERE id = ?1 AND account = ?2", params![id, account])? > 0)
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

fn gone_hash(device: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(format!("illogical gone daemon\n{device}")))
}

#[cfg(test)]
mod tests {
    use illogical_e2e::{Cert, Kind};

    use super::*;

    /// #102: a GitHub account goes by its login, a passkey one by the name
    /// it chose; rosters and requests get one word, never "you".
    #[test]
    fn names() {
        let db = Db::memory();
        db.account_for("github", "1", "jhgaylor", "a1", 1).unwrap();
        db.account_for("passkey", "p2", "", "b2c3d4e5", 1).unwrap();
        assert_eq!(db.account("a1").unwrap().unwrap().name, "jhgaylor");
        let pk = db.account("b2c3d4e5").unwrap().unwrap();
        assert_eq!(pk.name, "");
        assert_eq!(crate::teams::member_name(&pk), "account-b2c3d4");

        db.set_name("b2c3d4e5", "Ada Lovelace").unwrap();
        let pk = db.account("b2c3d4e5").unwrap().unwrap();
        assert_eq!(pk.name, "Ada Lovelace");
        assert_eq!(crate::teams::member_name(&pk), "Ada-Lovelace");
        assert_eq!(db.accounts_named("ada lovelace").unwrap(), vec!["b2c3d4e5".to_owned()]);
        // An empty login finds nobody (passkey identities have none).
        assert!(db.account_by_login("").unwrap().is_none());

        // A GitHub account can pick a name too.
        db.set_name("a1", "Jake").unwrap();
        assert_eq!(db.account("a1").unwrap().unwrap().name, "Jake");
        assert_eq!(crate::api::display_name("  Ada \n Lovelace ").unwrap(), "Ada Lovelace");
        assert!(crate::api::display_name("   ").is_err());
        assert!(crate::api::display_name(&"x".repeat(65)).is_err());
    }

    fn pending(device: &str) -> Cert {
        Cert {
            v: 1,
            account: "a1".into(),
            device: device.into(),
            kind: Kind::Browser,
            name: "phone".into(),
            noise: String::new(),
            sign: String::new(),
            created: 1,
            approver: String::new(),
            sig: String::new(),
        }
    }

    #[test]
    fn a_teams_followers_and_what_they_understand() {
        let db = Db::memory();
        for (id, name) in [("d1", "buildbox"), ("d2", "laptop"), ("d3", "elsewhere")] {
            db.put_daemon("a1", id, name, &[]).unwrap();
        }
        db.set_daemon_team("d1", "t1").unwrap();
        db.watch_team("d2", "t1", 100).unwrap();
        // An older daemon never says what it understands.
        assert_eq!(
            db.team_followers("t1", 0).unwrap(),
            vec![("buildbox".to_owned(), String::new()), ("laptop".to_owned(), String::new())]
        );
        db.set_daemon_features("d1", "presigned-invites").unwrap();
        assert_eq!(db.team_followers("t1", 0).unwrap()[0].1, "presigned-invites");
        // A share that stopped asking ages out.
        assert_eq!(db.team_followers("t1", 100).unwrap().len(), 1);
    }

    #[test]
    fn turned_down_until_it_asks_again() {
        let db = Db::memory();
        db.put_device(&pending("d1"), false, 1).unwrap();
        db.turn_down("a1", "d1", "Chrome on Mac", 2).unwrap();
        assert!(db.device("a1", "d1").unwrap().is_none());
        assert_eq!(db.turned_down("a1", "d1").unwrap().as_deref(), Some("Chrome on Mac"));
        // Nothing pending: nothing to turn down, nothing remembered.
        db.turn_down("a1", "d2", "x", 3).unwrap();
        assert_eq!(db.turned_down("a1", "d2").unwrap(), None);
        // Asking again forgets the turn-down.
        db.put_device(&pending("d1"), false, 4).unwrap();
        assert_eq!(db.turned_down("a1", "d1").unwrap(), None);
    }
}
