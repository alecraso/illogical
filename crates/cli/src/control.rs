//! The CLI as a device of your account on illogical control (M49).
//!
//! `illogical login` makes this CLI a `cli` device: it asks control to join
//! the account the way a daemon does (`illogicald join`), shows a code, and
//! waits while a signed-in device approves it. It pins the account's root
//! from the approval, as a daemon does, and keeps its key and the pin in
//! the config dir (`cli-key`, `cli-control.json`).
//!
//! Then `--host NAME` finds names that aren't in the local daemon's host
//! list in control's directory (the account's own machines), checks the
//! machine's certificate against the pinned root, and opens a Noise channel
//! to it with the CLI's key: straight to a URL the machine lists, else
//! through control's relay. Requests to control are signed with the key
//! (`x-illogical-auth`, as daemons sign theirs), so there's no session to
//! keep or expire.
//!
//! Each HTTP connection the commands make is a request carried over that
//! one channel ([`Link::stream`]), so `run`, `ls`, `capture` and the rest
//! work unchanged. What needs a WebSocket (`attach`, `tui`) or a streamed
//! answer (`events --follow`) doesn't yet.

use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, ErrorKind, IsTerminal, Read, Write},
    os::unix::{fs::OpenOptionsExt, net::UnixStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use illogical_e2e::{
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
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
        .join("illogical")
}

/// What `illogical login` pinned.
#[derive(Serialize, Deserialize)]
struct Saved {
    url: String,
    cert: Cert,
    trust: Trust,
}

/// This CLI, enrolled.
pub struct Enrolled {
    url: String,
    cert: Cert,
    trust: Trust,
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
            bail!("{} and {} don't match: `illogical logout`, then log in again", KEY_FILE, STATE_FILE);
        }
        Ok(Some(Self { url: s.url, cert: s.cert, trust: s.trust, keys }))
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
            &[("Content-Type", "application/json"), ("x-illogical-auth", &auth)],
            bytes.as_bytes(),
        )
        .with_context(|| format!("asking control at {}", self.url))?;
        res.json().map_err(|e| e.context(format!("control at {}", self.url)))
    }

    /// The account's machines, as control lists them. Only the account's
    /// own: a team's or a shared machine is checked against another
    /// account's root, which this CLI hasn't pinned.
    pub fn directory(&self) -> anyhow::Result<Vec<Listed>> {
        let v = self.api("GET", "/api/directory", None)?;
        Ok(v["daemons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| d.get("account").is_none_or(Value::is_null))
            .filter_map(|d| serde_json::from_value(d.clone()).ok())
            .collect())
    }

    /// The account's devices that chain to the pinned root.
    fn trusted(&self) -> anyhow::Result<illogical_e2e::cert::Trusted> {
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
                "this CLI isn't one of the account's devices any more (revoked?): `illogical logout`, then log in again"
            );
        }
        let cert = trusted
            .get(&machine.id)
            .filter(|c| c.kind == Kind::Daemon)
            .with_context(|| format!("control lists {}, but it isn't a machine your account trusts", machine.name))?
            .clone();
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
        let headers = [("x-illogical-auth", auth.as_str()), ("origin", origin.as_str())];
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
}

/// `--host NAME` through control: `None` when this CLI isn't logged in or
/// control doesn't list the name.
pub fn target(host: &str) -> anyhow::Result<Option<Target>> {
    let Some(e) = Enrolled::load()? else { return Ok(None) };
    let list = e.directory()?;
    let Some(m) = list.iter().find(|m| m.name == host).or_else(|| list.iter().find(|m| m.id == host)) else {
        return Ok(None);
    };
    let link = e.open(m)?;
    if verbose() {
        eprintln!("illogical: {}: {}", m.name, link.route);
    }
    Ok(Some(Target::Control(Arc::new(link))))
}

fn verbose() -> bool {
    std::env::var("ILLOGICAL_VERBOSE").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// `illogical hosts`' control part: the URL logged in to and its machines.
pub fn listing() -> anyhow::Result<Option<(String, Vec<Listed>)>> {
    let Some(e) = Enrolled::load()? else { return Ok(None) };
    let list = e.directory()?;
    Ok(Some((e.url.clone(), list)))
}

/// Whether this CLI is logged in to a control.
pub fn logged_in() -> bool {
    config_dir().join(STATE_FILE).exists()
}

// ---------------------------------------------------------------- login

/// `illogical login`: join the account at `url` as a `cli` device. Shows
/// the code to approve on a signed-in device and waits; then checks the
/// account (`account`, the fingerprint the approving device shows, or by
/// asking) before pinning it.
pub fn login(url: &str, name: &str, account: Option<&str>) -> anyhow::Result<()> {
    let url = url.trim_end_matches('/').to_owned();
    if let Some(e) = Enrolled::load()? {
        bail!("this CLI is already logged in to {} (`illogical logout` first)", e.url);
    }
    let want = account.map(parse_fingerprint).transpose()?;
    let u = Url::parse(&url)?;
    if !u.tls && !private_host(&u.host) {
        bail!("control's URL must be https:// (or http on loopback or a private network, for testing)");
    }
    let get = |path: &str| -> anyhow::Result<crate::http::Response> {
        send_on(u.connect(Some(Duration::from_secs(10)))?, "GET", path, &u.authority, &[], b"")
    };
    let about = get("/control.json").and_then(|r| r.json()).with_context(|| format!("can't reach control at {url}"))?;
    if about["cli_join"].as_u64().is_none() {
        bail!("control at {url} doesn't take the illogical CLI as a device yet (it's older than this CLI)");
    }
    let keys = DeviceKeys::generate();
    let ask = Cert { account: String::new(), ..Cert::new(&keys, "", Kind::Cli, name) };
    let ms = now_ms();
    let proof = json!({ "ms": ms, "sig": hex(&keys.signature(join_proof_body(&ask, ms).as_bytes())) });
    let body = json!({ "cert": ask, "proof": proof }).to_string();
    let started = send_on(
        u.connect(Some(Duration::from_secs(10)))?,
        "POST",
        "/api/join",
        &u.authority,
        &[("Content-Type", "application/json")],
        body.as_bytes(),
    )?
    .json()
    .context("control")?;
    let code = started["code"].as_str().context("control sent no code")?.to_owned();
    let poll = started["poll"].as_str().context("control sent no poll token")?.to_owned();
    let secs = started["expires_in_secs"].as_u64().unwrap_or(600);
    if code != illogical_e2e::cert::join_code(&ask) {
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
            bail!("nobody approved it in {} minutes; run `illogical login` again for a new code", secs / 60);
        }
        std::thread::sleep(Duration::from_secs(2));
        let Ok(res) = get(&format!("/api/join/{code}?poll={poll}")) else { continue };
        if res.status == 404 {
            bail!("the code expired; run `illogical login` again");
        }
        let v = res.json().context("control")?;
        if let Some(on) = v["rejected"].as_str() {
            bail!("turned down on {on}");
        }
        if v["approved"] == true {
            break v;
        }
    };
    let cert: Cert =
        serde_json::from_value(got["cert"].clone()).context("control approved it but sent no certificate")?;
    let trust: Trust = serde_json::from_value(got["trust"].clone()).context("control sent no account root")?;
    let certs: Vec<Cert> = serde_json::from_value(got["certs"].clone()).unwrap_or_default();
    let revs: Vec<Revocation> = serde_json::from_value(got["revocations"].clone()).unwrap_or_default();
    if !cert.same_request(&ask) {
        bail!("control sent back a certificate for a different key; not logging in");
    }
    let mut all = certs.clone();
    all.push(cert.clone());
    let now = trust.evaluate(&all, &revs);
    if now.get(&cert.device) != Some(&cert) {
        bail!("the approval doesn't check out against the account's devices; not logging in");
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
    let saved = Saved { url: url.clone(), cert, trust };
    let tmp = dir.join(format!("{STATE_FILE}.tmp"));
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?
        .write_all(serde_json::to_string_pretty(&saved)?.as_bytes())?;
    fs::rename(&tmp, dir.join(STATE_FILE))?;
    println!();
    println!("  Logged in. This terminal is one of your devices, approved on \"{approver}\".");
    println!("  The account is {fp}. `illogical hosts` lists your machines; `--host NAME` reaches one.");
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

/// `illogical logout`: forget the key and the pin. The device stays on the
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

type Reply = mpsc::Sender<(u16, Option<String>, Vec<u8>)>;

/// One Noise channel to a machine, shared by every request a command makes.
/// A thread owns the socket: it seals what's queued, in order, and hands
/// each response to the request waiting for it.
pub struct Link {
    out: Mutex<mpsc::Sender<(Msg, Option<Reply>)>>,
    wake: Mutex<UnixStream>,
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
        let (wake, woken) = UnixStream::pair()?;
        woken.set_nonblocking(true)?;
        std::thread::spawn(move || io(ws, ch, rx, woken));
        Ok(Self { out: Mutex::new(tx), wake: Mutex::new(wake), next: AtomicU32::new(1), route })
    }

    fn put(&self, m: Msg, reply: Option<Reply>) -> anyhow::Result<()> {
        self.out.lock().unwrap().send((m, reply)).map_err(|_| anyhow::anyhow!("the channel closed"))?;
        let _ = self.wake.lock().unwrap().write_all(b"x");
        Ok(())
    }

    /// One request and its answer: (status, content type, body).
    pub fn request(
        &self,
        method: &str,
        path: &str,
        content_type: Option<String>,
        body: Vec<u8>,
    ) -> anyhow::Result<(u16, Option<String>, Vec<u8>)> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let head = RequestHead { method: method.into(), path: path.into(), content_type };
        self.put(Msg::Request { id, head, body }, Some(tx))?;
        rx.recv().map_err(|_| anyhow::anyhow!("the channel to the machine closed"))
    }

    /// A connection the HTTP client can use as if it were a socket to the
    /// daemon: each request on it is carried over the channel.
    pub fn stream(self: &Arc<Self>) -> anyhow::Result<UnixStream> {
        let (ours, theirs) = UnixStream::pair()?;
        let link = self.clone();
        std::thread::spawn(move || {
            let _ = serve_one(&link, theirs);
        });
        Ok(ours)
    }
}

/// Read one HTTP/1.1 request from `s`, carry it, write its answer.
fn serve_one(link: &Link, s: UnixStream) -> anyhow::Result<()> {
    let mut r = BufReader::new(s.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut words = line.split_whitespace();
    let (method, path) = (words.next().unwrap_or("GET").to_owned(), words.next().unwrap_or("/").to_owned());
    let (mut len, mut ct, mut upgrade) = (0usize, None, false);
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
            "upgrade" => upgrade = true,
            _ => {}
        }
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let (status, ct, body) = if upgrade {
        let e = json!({"error": "attach and tui don't work through control yet: use the web page, or --ssh"});
        (501, Some("application/json".to_owned()), e.to_string().into_bytes())
    } else {
        match link.request(&method, &path, ct, body) {
            Ok(r) => r,
            Err(e) => {
                (502, Some("application/json".to_owned()), json!({"error": format!("{e:#}")}).to_string().into_bytes())
            }
        }
    };
    let mut w = s;
    let mut head = format!("HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
    if let Some(ct) = ct {
        head.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes())?;
    w.write_all(&body)?;
    w.flush()?;
    Ok(())
}

/// The socket's thread: until every `Link` handle is gone or the socket
/// closes.
fn io(
    mut ws: WebSocket<Box<dyn Stream>>,
    ch: Channel,
    rx: mpsc::Receiver<(Msg, Option<Reply>)>,
    mut woken: UnixStream,
) {
    use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
    use std::os::fd::AsFd;
    let mut waiting: HashMap<u32, Reply> = HashMap::new();
    let mut buf = [0u8; 256];
    'outer: loop {
        {
            let mut fds =
                [PollFd::new(ws.get_ref().fd(), PollFlags::POLLIN), PollFd::new(woken.as_fd(), PollFlags::POLLIN)];
            let _ = poll(&mut fds, PollTimeout::from(100u16));
        }
        while woken.read(&mut buf).is_ok_and(|n| n > 0) {}
        loop {
            match rx.try_recv() {
                Ok((m, reply)) => {
                    if let (Msg::Request { id, .. }, Some(r)) = (&m, reply) {
                        waiting.insert(*id, r);
                    }
                    let Ok(wires) = ch.seal(&m) else { break 'outer };
                    for w in wires {
                        if ws.write(Message::Binary(w.into())).is_err() {
                            break 'outer;
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
                        if let Some(r) = waiting.remove(&id) {
                            let _ = r.send((head.status, head.content_type, body));
                        }
                    }
                    // The daemon's protocol messages (its hello, state):
                    // nothing here asks for them.
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
        for no in ["control.illogical.widgets.wtf", "8.8.8.8"] {
            assert!(!private_host(no), "{no}");
        }
    }
}
