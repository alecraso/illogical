//! The CLI as a device of your account on Arugula control (M49).
//!
//! `arugula login` makes this CLI a `cli` device: it asks control to join
//! the account the way a daemon does (`arugulad join`), shows a code, and
//! waits while a signed-in device approves it. It pins the account's root
//! from the approval, as a daemon does, and keeps its key and the pin in
//! the config dir (`cli-key`, `cli-control.json`).
//!
//! Then `--host NAME` finds names that aren't in the local daemon's host
//! list in control's directory, checks the machine's certificate, and opens
//! a Noise channel to it with the CLI's key: straight to a URL the machine
//! lists, else through control's relay. The account's own machines are
//! checked against the pinned root; a team's or a shared machine of another
//! account against that account's root, pinned the first time this CLI sees
//! it (as a browser does) and refused if control later says otherwise.
//! Requests to control are signed with the key (`x-arugula-auth`, as
//! daemons sign theirs), so there's no session to keep or expire.
//!
//! Each HTTP connection the commands make is a request carried over that
//! one channel ([`Link::stream`]), so `run`, `ls`, `capture` and the rest
//! work unchanged. An answer with no end (`events --follow`, `tail
//! --follow`) comes back in parts as it's written. A WebSocket to `/ws`
//! (`attach`, `tui`) is the channel's own protocol messages, which the
//! channel already carries: the daemon makes each channel a client of its
//! own, so one `/ws` per command.

#[cfg(unix)]
use std::os::unix::net::UnixStream;
/// Windows: the HTTP client's end of a carried connection is a loopback
/// TCP pair ([`pair`]).
#[cfg(not(unix))]
type UnixStream = std::net::TcpStream;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{BufRead, BufReader, ErrorKind, IsTerminal, Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use arugula_control_wire as wire;
use arugula_e2e::{
    Cert, DeviceKeys, Kind, Revocation, Trust,
    cert::{join_proof_body, request_auth},
    channel::{Channel, Initiator, Msg, RequestHead, prologue},
    keys::fingerprint,
    now_ms,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

use crate::http::{Stream, Target, Url, send_on};

const KEY_FILE: &str = "cli-key";
const STATE_FILE: &str = "cli-control.json";
/// How long a direct URL gets before the relay is tried.
const DIRECT_TIMEOUT: Duration = Duration::from_secs(3);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

fn config_dir() -> PathBuf {
    arugula_proto::dirs::config_dir().unwrap_or_else(|| PathBuf::from(".config/arugula"))
}

/// What `arugula login` pinned.
#[derive(Serialize, Deserialize)]
struct Saved {
    url: String,
    cert: Cert,
    trust: Trust,
    /// Other accounts' roots (their team's or shared machines), by account,
    /// as first seen.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pins: BTreeMap<String, String>,
}

impl Saved {
    fn write(&self) -> anyhow::Result<()> {
        let dir = config_dir();
        fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!("{STATE_FILE}.tmp"));
        let mut file = fs::OpenOptions::new();
        file.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut file, 0o600);
        file.open(&tmp)?.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        fs::rename(&tmp, dir.join(STATE_FILE))?;
        Ok(())
    }
}

/// This CLI, enrolled.
pub struct Enrolled {
    url: String,
    cert: Cert,
    trust: Trust,
    pins: BTreeMap<String, String>,
    keys: DeviceKeys,
}

impl Enrolled {
    pub fn load() -> anyhow::Result<Option<Self>> {
        let dir = config_dir();
        let Ok(text) = fs::read_to_string(dir.join(STATE_FILE)) else { return Ok(None) };
        let s: Saved =
            serde_json::from_str(&text).with_context(|| format!("reading {}", dir.join(STATE_FILE).display()))?;
        let keys = DeviceKeys::load(&dir.join(KEY_FILE))?;
        if keys.id() != s.cert.device {
            bail!("{} and {} don't match: `arugula logout`, then log in again", KEY_FILE, STATE_FILE);
        }
        Ok(Some(Self { url: s.url, cert: s.cert, trust: s.trust, pins: s.pins, keys }))
    }

    fn url(&self) -> anyhow::Result<Url> {
        Url::parse(&self.url)
    }

    /// A request to control, signed with this CLI's key.
    fn api(&self, method: &str, path: &str, body: Option<&Value>) -> anyhow::Result<Value> {
        let bytes = body.map(|b| b.to_string()).unwrap_or_default();
        let auth = request_auth(&self.keys, method, path, bytes.as_bytes());
        let u = self.url()?;
        let res = send_on(
            u.connect(Some(Duration::from_secs(10)))?,
            method,
            path,
            &u.authority,
            &[("Content-Type", "application/json"), ("x-arugula-auth", &auth)],
            bytes.as_bytes(),
        )
        .with_context(|| format!("asking control at {}", self.url))?;
        res.json().map_err(|e| e.context(format!("control at {}", self.url)))
    }

    /// The machines control lists: the account's own, and its teams' and
    /// those shared with it that check out against their account's root
    /// (pinned here on first sight).
    pub fn directory(&mut self) -> anyhow::Result<Vec<Listed>> {
        let v = self.api("GET", "/api/directory", None)?;
        let mut out = Vec::new();
        let mut pinned = false;
        for d in v["daemons"].as_array().into_iter().flatten() {
            let Ok(mut m) = serde_json::from_value::<Listed>(d.clone()) else { continue };
            if let Some(account) = m.account.clone() {
                let Ok(chain) = serde_json::from_value::<Chain>(d["chain"].clone()) else { continue };
                let Some(trust) = chain.trust.filter(|t| t.account == account) else { continue };
                match self.pins.get(&account) {
                    Some(root) if *root != trust.root => {
                        eprintln!(
                            "arugula: control says {}'s account is {} now, not {} as first seen here: not trusting {}",
                            m.owner(),
                            fingerprint(&trust.root),
                            fingerprint(root),
                            m.name
                        );
                        continue;
                    }
                    Some(_) => {}
                    None => {
                        self.pins.insert(account.clone(), trust.root.clone());
                        pinned = true;
                    }
                }
                let cert = trust.evaluate(&chain.certs, &chain.revocations).get(&m.id).cloned();
                let Some(cert) = cert.filter(|c| c.kind == Kind::Daemon) else { continue };
                m.cert = Some(cert);
            }
            out.push(m);
        }
        if pinned {
            let saved = Saved {
                url: self.url.clone(),
                cert: self.cert.clone(),
                trust: self.trust.clone(),
                pins: self.pins.clone(),
            };
            saved.write()?;
        }
        Ok(out)
    }

    /// The account's devices that chain to the pinned root.
    fn trusted(&self) -> anyhow::Result<arugula_e2e::cert::Trusted> {
        let v = self.api("GET", "/api/devices", None)?;
        let certs: Vec<Cert> = serde_json::from_value(v["certs"].clone()).unwrap_or_default();
        let revs: Vec<Revocation> = serde_json::from_value(v["revocations"].clone()).unwrap_or_default();
        Ok(self.trust.evaluate(&certs, &revs))
    }

    /// A channel to `machine` (by name or id): direct first, then through
    /// the relay.
    pub fn open(&self, machine: &Listed) -> anyhow::Result<Link> {
        let trusted = self.trusted()?;
        if trusted.get(&self.cert.device).is_none() {
            bail!(
                "this CLI isn't one of the account's devices any more (revoked?): `arugula logout`, then log in again"
            );
        }
        let cert = match &machine.cert {
            // Another account's, checked by `directory`.
            Some(c) => c.clone(),
            None => trusted
                .get(&machine.id)
                .filter(|c| c.kind == Kind::Daemon)
                .with_context(|| format!("control lists {}, but it isn't a machine your account trusts", machine.name))?
                .clone(),
        };
        let mut errors = Vec::new();
        for u in &machine.urls {
            let Ok(url) = Url::parse(u) else { continue };
            let ws = format!("{}://{}/e2e", if url.tls { "wss" } else { "ws" }, url.authority);
            match Link::connect(&url, &ws, &[], &cert, &self.keys, Some(DIRECT_TIMEOUT), format!("direct {u}")) {
                Ok(l) => return Ok(l),
                Err(e) => errors.push(format!("{u}: {e:#}")),
            }
        }
        let c = self.url()?;
        let path = format!("/api/relay/c/{}", machine.id);
        let ws = format!("{}://{}{path}", if c.tls { "wss" } else { "ws" }, c.authority);
        let auth = request_auth(&self.keys, "GET", &path, b"");
        let origin = self.url.trim_end_matches('/').to_owned();
        let headers = [("x-arugula-auth", auth.as_str()), ("origin", origin.as_str())];
        match Link::connect(&c, &ws, &headers, &cert, &self.keys, None, "relayed through control".into()) {
            Ok(l) => Ok(l),
            Err(e) => {
                errors.push(format!("the relay: {e:#}"));
                bail!("can't reach {} ({})", machine.name, errors.join("; "))
            }
        }
    }
}

/// A machine in control's directory.
#[derive(Deserialize, Clone, Debug)]
pub struct Listed {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub online: bool,
    /// Another account's machine (a team's, or shared): that account.
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub owner_name: Option<String>,
    /// The team it's in, if any.
    #[serde(default)]
    pub team: Option<String>,
    /// Another account's machine's certificate, checked against that
    /// account's pinned root.
    #[serde(skip)]
    pub cert: Option<Cert>,
}

impl Listed {
    /// Whose it is, for another account's machine.
    pub fn owner(&self) -> String {
        match (&self.owner_name, &self.account) {
            (Some(n), _) if !n.is_empty() => n.clone(),
            (_, Some(a)) => format!("account {}", &a[..a.len().min(8)]),
            _ => String::new(),
        }
    }
}

/// Another account's certificates, as the directory sends them.
#[derive(Deserialize)]
struct Chain {
    trust: Option<Trust>,
    #[serde(default)]
    certs: Vec<Cert>,
    #[serde(default)]
    revocations: Vec<Revocation>,
}

/// `--host NAME` through control: `None` when this CLI isn't logged in or
/// control doesn't list the name.
pub fn target(host: &str) -> anyhow::Result<Option<Target>> {
    let Some(mut e) = Enrolled::load()? else { return Ok(None) };
    let list = e.directory()?;
    let Some(m) = list.iter().find(|m| m.name == host).or_else(|| list.iter().find(|m| m.id == host)) else {
        return Ok(None);
    };
    let link = e.open(m)?;
    if verbose() {
        eprintln!("arugula: {}: {}", m.name, link.route);
    }
    Ok(Some(Target::Control(Arc::new(link))))
}

fn verbose() -> bool {
    std::env::var("ARUGULA_VERBOSE").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// `arugula hosts`' control part: the URL logged in to and its machines.
pub fn listing() -> anyhow::Result<Option<(String, Vec<Listed>)>> {
    let Some(mut e) = Enrolled::load()? else { return Ok(None) };
    let list = e.directory()?;
    Ok(Some((e.url.clone(), list)))
}

/// Whether this CLI is logged in to a control.
pub fn logged_in() -> bool {
    config_dir().join(STATE_FILE).exists()
}

// ---------------------------------------------------------------- login

/// `arugula login`: join the account at `url` as a `cli` device. Shows
/// the code to approve on a signed-in device and waits; then checks the
/// account (`account`, the fingerprint the approving device shows, or by
/// asking) before pinning it.
pub fn login(url: &str, name: &str, account: Option<&str>) -> anyhow::Result<()> {
    let url = url.trim_end_matches('/').to_owned();
    if let Some(e) = Enrolled::load()? {
        bail!("this CLI is already logged in to {} (`arugula logout` first)", e.url);
    }
    let want = account.map(parse_fingerprint).transpose()?;
    let u = Url::parse(&url)?;
    if !u.tls && !private_host(&u.host) {
        bail!("control's URL must be https:// (or http on loopback or a private network, for testing)");
    }
    let get = |path: &str| -> anyhow::Result<crate::http::Response> {
        send_on(u.connect(Some(Duration::from_secs(10)))?, "GET", path, &u.authority, &[], b"")
    };
    let about =
        get(wire::CONTROL_JSON).and_then(|r| r.json()).with_context(|| format!("can't reach control at {url}"))?;
    // Only this field, so one the CLI never reads can't make control look older.
    let takes_cli = serde_json::from_value::<wire::ControlCliJoin>(about).ok().and_then(|c| c.cli_join);
    if takes_cli.is_none() {
        bail!("control at {url} doesn't take the Arugula CLI as a device yet (it's older than this CLI)");
    }
    let keys = DeviceKeys::generate();
    let ask = Cert { account: String::new(), ..Cert::new(&keys, "", Kind::Cli, name) };
    let ms = now_ms();
    let proof = wire::JoinProof { ms, sig: hex(&keys.signature(join_proof_body(&ask, ms).as_bytes())) };
    let body = serde_json::to_string(&wire::CliJoinRequest { cert: ask.clone(), proof })?;
    let started = send_on(
        u.connect(Some(Duration::from_secs(10)))?,
        "POST",
        wire::JOIN,
        &u.authority,
        &[("Content-Type", "application/json")],
        body.as_bytes(),
    )?
    .json()
    .context("control")?;
    let started: wire::JoinStarted =
        serde_json::from_value(started).context("control sent no code or poll token it could be joined with")?;
    let (code, poll, secs) = (started.code, started.poll, started.expires_in_secs);
    if code != arugula_e2e::cert::join_code(&ask) {
        bail!("control sent a code that isn't this key's; not logging in");
    }
    println!();
    println!("  To make this terminal ({name}) one of your devices, open");
    println!();
    println!("    {url}/#join={code}");
    println!();
    println!("  on a device that's signed in, and check the code there is {code}.");
    println!("  Or sign in at {url} and type the code.");
    println!();
    println!("  Waiting for approval (the code lasts {} minutes)…", secs / 60);
    std::io::stdout().flush()?;
    let deadline = Instant::now() + Duration::from_secs(secs);
    let got = loop {
        if Instant::now() > deadline {
            bail!("nobody approved it in {} minutes; run `arugula login` again for a new code", secs / 60);
        }
        std::thread::sleep(Duration::from_secs(2));
        let Ok(res) = get(&wire::JoinPoll::path(&code, &poll)) else { continue };
        if res.status == 404 {
            bail!("the code expired; run `arugula login` again");
        }
        let v: wire::JoinPoll = serde_json::from_value(res.json().context("control")?)
            .context("control's answer about the approval isn't one this CLI can read")?;
        if let Some(on) = &v.rejected {
            bail!("turned down on {on}");
        }
        if v.approved {
            break v;
        }
    };
    let cert = got.cert.context("control approved it but sent no certificate")?;
    let trust = got.trust.context("control sent no account root")?;
    let (certs, revs) = (got.certs, got.revocations);
    if !cert.same_request(&ask) {
        bail!("control sent back a certificate for a different key; not logging in");
    }
    // Which check failed, if one did (#327).
    if let Some(r) = trust.refusal(&certs, &revs, &cert) {
        bail!("the approval doesn't check out against the account's devices: {}; not logging in", r.check());
    }
    let approver = trust.evaluate(&certs, &revs).get(&cert.approver).map(|c| c.name.clone()).unwrap_or_default();
    let fp = fingerprint(&trust.root);
    match want {
        Some(w) if w != trust.root => {
            bail!("control approved this CLI into the account {fp}, not {}; not logging in", fingerprint(&w))
        }
        Some(_) => {}
        None => confirm(&approver, &fp, &trust.root)?,
    }
    let dir = config_dir();
    fs::create_dir_all(&dir)?;
    keys.save(&dir.join(KEY_FILE))?;
    Saved { url: url.clone(), cert, trust, pins: BTreeMap::new() }.write()?;
    println!();
    println!("  Logged in. This terminal is one of your devices, approved on \"{approver}\".");
    println!("  The account is {fp}. `arugula hosts` lists your machines; `--host NAME` reaches one.");
    Ok(())
}

fn confirm(approver: &str, fp: &str, root: &str) -> anyhow::Result<()> {
    println!();
    println!("  Approved on \"{approver}\". Before this CLI trusts it, check the account:");
    println!();
    println!("    {fp}");
    println!();
    println!(
        "  The device you approved on shows its account's fingerprint under Devices and machines… in the host menu."
    );
    print!("  Is it the same? [y/N, or type the fingerprint] ");
    std::io::stdout().flush()?;
    if !std::io::stdin().is_terminal() {
        println!();
        bail!("no terminal to ask in, so not logging in; pass --account with the fingerprint your device shows");
    }
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let said = line.trim();
    if matches!(said.to_ascii_lowercase().as_str(), "y" | "yes") || parse_fingerprint(said).is_ok_and(|f| f == root) {
        return Ok(());
    }
    bail!("not logging in. If the fingerprints differ, control isn't telling the truth about your account")
}

/// `arugula logout`: forget the key and the pin. The device stays on the
/// account's list until a device revokes it.
pub fn logout() -> anyhow::Result<()> {
    let dir = config_dir();
    let Some(e) = Enrolled::load()? else {
        println!("Not logged in.");
        return Ok(());
    };
    fs::remove_file(dir.join(STATE_FILE))?;
    fs::remove_file(dir.join(KEY_FILE))?;
    println!(
        "Logged out of {}. This terminal ({}) is still listed on the account: remove it under Devices and machines… to revoke it.",
        e.url, e.cert.name
    );
    Ok(())
}

/// A fingerprint as typed: 16 hex digits, dashes optional.
fn parse_fingerprint(typed: &str) -> anyhow::Result<String> {
    let id: String = typed.chars().filter(|c| *c != '-').map(|c| c.to_ascii_lowercase()).collect();
    if id.len() != 16 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("an account fingerprint is 16 hex digits, like 1a2b-3c4d-5e6f-7a8b");
    }
    Ok(id)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Loopback or a private address (or `localhost`): plain http is allowed
/// there, for tests and labs.
fn private_host(host: &str) -> bool {
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.is_loopback() || v4.is_private(),
        Ok(std::net::IpAddr::V6(v6)) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00,
        Err(_) => host == "localhost",
    }
}

// ---------------------------------------------------------------- the channel

/// One part of an answer: (status, content type, body, more to come).
type Part = (u16, Option<String>, Vec<u8>, bool);
type Reply = mpsc::Sender<Part>;

/// What the socket's thread is given.
enum Out {
    /// A message to seal and send; a request's answer goes to `Reply`.
    Send(Msg, Option<Reply>),
    /// Where the daemon's protocol messages go from now on (the `/ws` of
    /// this command), and how to wake whoever reads them.
    Sink(mpsc::Sender<Msg>, crate::wake::Waker),
}

/// What the daemon sends before a `/ws` is there to take it (its hello,
/// first of all) is kept, up to this much.
const EARLY_MAX: usize = 64 << 20;

/// One Noise channel to a machine, shared by every request a command makes.
/// A thread owns the socket: it seals what's queued, in order, and hands
/// each response to the request waiting for it.
pub struct Link {
    out: Mutex<mpsc::Sender<Out>>,
    waker: crate::wake::Waker,
    next: AtomicU32,
    /// How it got there, in words.
    pub route: String,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Link({})", self.route)
    }
}

impl Link {
    fn connect(
        at: &Url,
        ws_url: &str,
        headers: &[(&str, &str)],
        cert: &Cert,
        keys: &DeviceKeys,
        connect_timeout: Option<Duration>,
        route: String,
    ) -> anyhow::Result<Self> {
        use tungstenite::client::IntoClientRequest;
        let stream = at.connect(connect_timeout.or(Some(Duration::from_secs(10))))?;
        stream.set_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let mut req = ws_url.into_client_request()?;
        for (k, v) in headers {
            req.headers_mut().insert(tungstenite::http::HeaderName::from_bytes(k.as_bytes())?, v.parse()?);
        }
        let (mut ws, _) = tungstenite::client(req, stream).map_err(|e| match e {
            tungstenite::HandshakeError::Failure(tungstenite::Error::Http(r)) => {
                let body = r.body().as_deref().map(String::from_utf8_lossy).unwrap_or_default();
                let said = serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|v| v["error"].as_str().map(String::from))
                    .unwrap_or_else(|| body.to_string());
                anyhow::anyhow!("{} {said}", r.status())
            }
            tungstenite::HandshakeError::Failure(e) => anyhow::Error::from(e).context("websocket handshake"),
            tungstenite::HandshakeError::Interrupted(_) => anyhow::anyhow!("websocket handshake interrupted"),
        })?;
        // Noise IK, the CLI the initiator.
        let (init, m1) = Initiator::start(keys, &cert.noise_key(), &prologue(&cert.device))?;
        ws.send(Message::Binary(m1.into()))?;
        let m2 = loop {
            match ws.read() {
                Ok(Message::Binary(b)) => break b,
                Ok(Message::Close(_)) => bail!("closed during the handshake (not an approved device?)"),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    bail!("no answer to the handshake")
                }
                Err(e) => {
                    return Err(anyhow::Error::from(e).context("closed during the handshake (not an approved device?)"));
                }
            }
        };
        let (_, ch) = init.finish(&m2)?;
        ws.get_ref().set_timeout(None)?;
        ws.get_mut().set_nonblocking(true)?;
        let (tx, rx) = mpsc::channel();
        let (wake, waker) = crate::wake::Wake::pair()?;
        std::thread::spawn(move || io(ws, ch, rx, wake));
        Ok(Self { out: Mutex::new(tx), waker, next: AtomicU32::new(1), route })
    }

    fn put(&self, o: Out) -> anyhow::Result<()> {
        self.out.lock().unwrap().send(o).map_err(|_| anyhow::anyhow!("the channel closed"))?;
        self.waker.wake();
        Ok(())
    }

    /// One request; its answer comes in one part, or in several when
    /// `stream` is set and the daemon has no end to it.
    fn request(
        &self,
        method: &str,
        path: &str,
        content_type: Option<String>,
        body: Vec<u8>,
        stream: bool,
    ) -> anyhow::Result<mpsc::Receiver<Part>> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let head = RequestHead { method: method.into(), path: path.into(), content_type, stream };
        self.put(Out::Send(Msg::Request { id, head, body }, Some(tx)))?;
        Ok(rx)
    }

    /// A connection the HTTP client can use as if it were a socket to the
    /// daemon: each request on it is carried over the channel, and a
    /// WebSocket to `/ws` is the channel's protocol messages.
    pub fn stream(self: &Arc<Self>) -> anyhow::Result<UnixStream> {
        let (ours, theirs) = pair()?;
        let link = self.clone();
        std::thread::spawn(move || {
            let _ = serve_one(&link, theirs);
        });
        Ok(ours)
    }
}

/// Two connected ends, for a request carried in this process.
#[cfg(unix)]
fn pair() -> std::io::Result<(UnixStream, UnixStream)> {
    UnixStream::pair()
}

/// Windows: a loopback TCP pair. Whoever connects first to the listener is
/// checked to be us (by its address) before it's used.
#[cfg(not(unix))]
fn pair() -> std::io::Result<(UnixStream, UnixStream)> {
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    let ours = std::net::TcpStream::connect(l.local_addr()?)?;
    loop {
        let (theirs, from) = l.accept()?;
        if from == ours.local_addr()? {
            ours.set_nodelay(true)?;
            theirs.set_nodelay(true)?;
            return Ok((ours, theirs));
        }
    }
}

/// Read one HTTP/1.1 request from `s`, carry it, write its answer.
fn serve_one(link: &Link, s: UnixStream) -> anyhow::Result<()> {
    let mut r = BufReader::new(s.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut words = line.split_whitespace();
    let (method, path) = (words.next().unwrap_or("GET").to_owned(), words.next().unwrap_or("/").to_owned());
    let (mut len, mut ct, mut upgrade, mut key) = (0usize, None, false, None);
    loop {
        line.clear();
        r.read_line(&mut line)?;
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':').unwrap_or((l, ""));
        match k.trim().to_ascii_lowercase().as_str() {
            "content-length" => len = v.trim().parse().unwrap_or(0),
            "content-type" => ct = Some(v.trim().to_owned()),
            "upgrade" => upgrade = v.trim().eq_ignore_ascii_case("websocket"),
            "sec-websocket-key" => key = Some(v.trim().to_owned()),
            _ => {}
        }
    }
    let mut w = s;
    if upgrade {
        let early = r.buffer().to_vec();
        return match key {
            Some(key) if path == "/ws" => serve_ws(link, w, early, &key),
            _ => {
                let e = json!({"error": "only /ws goes through control as a WebSocket"}).to_string();
                write_head(&mut w, 404, Some("application/json"), Some(e.len()))?;
                w.write_all(e.as_bytes())?;
                Ok(())
            }
        };
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let failed = |w: &mut UnixStream, e: String| -> anyhow::Result<()> {
        let e = json!({"error": e}).to_string();
        write_head(w, 502, Some("application/json"), Some(e.len()))?;
        w.write_all(e.as_bytes())?;
        Ok(())
    };
    let parts = match link.request(&method, &path, ct, body, true) {
        Ok(p) => p,
        Err(e) => return failed(&mut w, format!("{e:#}")),
    };
    let Ok((status, ct, body, more)) = parts.recv() else {
        return failed(&mut w, "the channel to the machine closed".into());
    };
    if !more {
        write_head(&mut w, status, ct.as_deref(), Some(body.len()))?;
        w.write_all(&body)?;
        w.flush()?;
        return Ok(());
    }
    // In parts as they come, chunked; a closed channel ends it unfinished.
    write_head(&mut w, status, ct.as_deref(), None)?;
    let chunk = |w: &mut UnixStream, b: &[u8]| -> std::io::Result<()> {
        if !b.is_empty() {
            write!(w, "{:x}\r\n", b.len())?;
            w.write_all(b)?;
            w.write_all(b"\r\n")?;
            w.flush()?;
        }
        Ok(())
    };
    chunk(&mut w, &body)?;
    while let Ok((_, _, body, more)) = parts.recv() {
        chunk(&mut w, &body)?;
        if !more {
            w.write_all(b"0\r\n\r\n")?;
            break;
        }
    }
    w.flush()?;
    Ok(())
}

/// A response head; chunked without a length.
fn write_head(w: &mut UnixStream, status: u16, ct: Option<&str>, len: Option<usize>) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status} X\r\nConnection: close\r\n");
    match len {
        Some(n) => head.push_str(&format!("Content-Length: {n}\r\n")),
        None => head.push_str("Transfer-Encoding: chunked\r\n"),
    }
    if let Some(ct) = ct {
        head.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes())
}

/// `/ws` over the channel: the WebSocket's messages are the channel's
/// `T`/`B` messages, both ways, until either end closes.
fn serve_ws(link: &Link, mut s: UnixStream, early: Vec<u8>, key: &str) -> anyhow::Result<()> {
    use tungstenite::protocol::Role;
    let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
    write!(
        s,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    )?;
    let (tx, rx) = mpsc::channel();
    let (mut wake, waker) = crate::wake::Wake::pair()?;
    link.put(Out::Sink(tx, waker))?;
    s.set_nonblocking(true)?;
    let mut ws = WebSocket::from_partially_read(s, early, Role::Server, None);
    loop {
        let _ = wake.wait(Some(ws.get_ref()), Duration::from_millis(50));
        wake.drain();
        loop {
            let m = match rx.try_recv() {
                Ok(Msg::Text(t)) => Message::Text(t.into()),
                Ok(Msg::Binary(b)) => Message::Binary(b.into()),
                Ok(_) => continue,
                Err(mpsc::TryRecvError::Empty) => break,
                // The channel ended.
                Err(mpsc::TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return Ok(());
                }
            };
            match ws.write(m) {
                Ok(()) => {}
                Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
                Err(_) => return Ok(()),
            }
        }
        match ws.flush() {
            Ok(()) => {}
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
            Err(_) => return Ok(()),
        }
        loop {
            match ws.read() {
                Ok(Message::Text(t)) => link.put(Out::Send(Msg::Text(t.to_string()), None))?,
                Ok(Message::Binary(b)) => link.put(Out::Send(Msg::Binary(b.to_vec()), None))?,
                Ok(Message::Close(_)) => return Ok(()),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => break,
                Err(_) => return Ok(()),
            }
        }
    }
}

/// Where the daemon's protocol messages go.
enum Sink {
    /// No `/ws` yet: kept for it (`None` once there was too much).
    Early(Option<Vec<Msg>>, usize),
    To(mpsc::Sender<Msg>, crate::wake::Waker),
    /// Its `/ws` closed; the channel is one client, so there's no other.
    Gone,
}

impl Sink {
    fn take(&mut self, m: Msg) {
        match self {
            Sink::Early(Some(kept), n) => {
                *n += match &m {
                    Msg::Text(t) => t.len(),
                    Msg::Binary(b) => b.len(),
                    _ => 0,
                };
                kept.push(m);
                if *n > EARLY_MAX {
                    *self = Sink::Early(None, 0);
                }
            }
            Sink::Early(None, _) | Sink::Gone => {}
            Sink::To(tx, wake) => {
                if tx.send(m).is_ok() {
                    wake.wake();
                } else {
                    *self = Sink::Gone;
                }
            }
        }
    }

    /// The `/ws` arrived: what was kept goes first. A second `/ws`, or one
    /// after too much went unread, gets nothing and closes.
    fn attach(&mut self, tx: mpsc::Sender<Msg>, wake: crate::wake::Waker) {
        let Sink::Early(Some(kept), _) = std::mem::replace(self, Sink::Gone) else { return };
        for m in kept {
            let _ = tx.send(m);
        }
        wake.wake();
        *self = Sink::To(tx, wake);
    }
}

/// The socket's thread: until every `Link` handle is gone or the socket
/// closes.
fn io(mut ws: WebSocket<Box<dyn Stream>>, ch: Channel, rx: mpsc::Receiver<Out>, mut wake: crate::wake::Wake) {
    let mut waiting: HashMap<u32, Reply> = HashMap::new();
    let mut sink = Sink::Early(Some(Vec::new()), 0);
    'outer: loop {
        let _ = wake.wait(Some(ws.get_ref().as_ref()), Duration::from_millis(100));
        wake.drain();
        loop {
            match rx.try_recv() {
                Ok(Out::Sink(tx, wake)) => sink.attach(tx, wake),
                Ok(Out::Send(m, reply)) => {
                    if let (Msg::Request { id, .. }, Some(r)) = (&m, reply) {
                        waiting.insert(*id, r);
                    }
                    let Ok(wires) = ch.seal(&m) else { break 'outer };
                    for w in wires {
                        match ws.write(Message::Binary(w.into())) {
                            Ok(()) => {}
                            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
                            Err(_) => break 'outer,
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break 'outer,
            }
        }
        match ws.flush() {
            Ok(()) => {}
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
            Err(_) => break,
        }
        // Always read until there's nothing: TLS or tungstenite may hold a
        // whole message the poll above didn't see.
        loop {
            match ws.read() {
                Ok(Message::Binary(w)) => match ch.open(&w) {
                    Ok(Some(Msg::Response { id, head, body })) => {
                        let more = head.more;
                        let gone = match waiting.get(&id) {
                            Some(r) => r.send((head.status, head.content_type, body, more)).is_err(),
                            None => false,
                        };
                        if !more || gone {
                            waiting.remove(&id);
                        }
                    }
                    Ok(Some(m @ (Msg::Text(_) | Msg::Binary(_)))) => sink.take(m),
                    Ok(_) => {}
                    Err(_) => break 'outer,
                },
                Ok(Message::Close(_)) => break 'outer,
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => break,
                Err(_) => break 'outer,
            }
        }
    }
    let _ = ws.close(None);
    let _ = ws.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints() {
        assert_eq!(parse_fingerprint("1A2B-3c4d-5e6f-7a8b").unwrap(), "1a2b3c4d5e6f7a8b");
        assert!(parse_fingerprint("1a2b").is_err());
        assert!(parse_fingerprint("zzzz-3c4d-5e6f-7a8b").is_err());
    }

    #[test]
    fn plain_http_only_nearby() {
        for ok in ["127.0.0.1", "10.229.85.10", "192.168.1.2", "localhost", "::1"] {
            assert!(private_host(ok), "{ok}");
        }
        for no in ["control.arugula.io", "8.8.8.8"] {
            assert!(!private_host(no), "{no}");
        }
    }
}
