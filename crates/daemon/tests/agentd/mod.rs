//! A daemon for agent block tests, driven over its Unix socket.

#![allow(dead_code)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

use illogical_testkit::{Builder, illogicald};
#[allow(unused_imports)]
pub use illogical_testkit::{Scratch, systemctl};
use serde_json::{Value, json};

pub fn fake() -> String {
    format!("{}/tests/fake_acp.py", env!("CARGO_MANIFEST_DIR"))
}

/// A testkit daemon with a sessions dir for the agents it runs
/// (`FAKE_ACP_DIR`, their `cwd`).
pub struct Daemon {
    d: illogical_testkit::Daemon,
    pub sessions: PathBuf,
}

impl std::ops::Deref for Daemon {
    type Target = illogical_testkit::Daemon;
    fn deref(&self) -> &Self::Target {
        &self.d
    }
}

impl std::ops::DerefMut for Daemon {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.d
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Machines go with what owns them: close those first, so a failed
        // test leaves no VM behind.
        if self.raw("GET", "/api/machines", None).0 == 200 {
            let machines: Value = serde_json::from_str(&self.raw("GET", "/api/machines", None).1).unwrap_or_default();
            for m in machines.as_array().into_iter().flatten() {
                if let Some(p) = m["owner"]["pane"].as_u64() {
                    self.raw("POST", &format!("/api/panes/{p}/close"), None);
                }
            }
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline && self.raw("GET", "/api/machines", None).1.trim() != "[]" {
                std::thread::sleep(Duration::from_millis(200));
            }
            if !machines.as_array().is_none_or(|m| m.is_empty()) {
                // The delete itself runs in the background.
                std::thread::sleep(Duration::from_secs(2));
            }
        }
        self.d.halt();
        // Anything it left running.
        let _ = Command::new("pkill").args(["-f", &self.sessions.display().to_string()]).status();
        if std::env::var_os("ILLOGICAL_KEEP_TEST_STATE").is_none() {
            let _ = std::fs::remove_dir_all(&self.sessions);
        }
        // The testkit daemon, dropped next, removes the state dir.
    }
}

/// A sessions dir for a test's agents.
fn sessions() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let sessions = std::env::temp_dir().join(format!("ilg-agt-sessions-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&sessions);
    std::fs::create_dir_all(&sessions).unwrap();
    sessions
}

impl Daemon {
    pub fn child() -> Self {
        Self::child_with(&[])
    }

    /// With extra daemon arguments (`--wisp-token-file …`).
    pub fn child_with(args: &[&str]) -> Self {
        Self::child_env(args, &[])
    }

    /// ...and extra environment.
    pub fn child_env(args: &[&str], env: &[(&str, &str)]) -> Self {
        Self::child_in(args, env, |_| {})
    }

    /// ...with its state directory made by `setup` first (a control
    /// enrollment, say).
    pub fn child_in(args: &[&str], env: &[(&str, &str)], setup: impl FnOnce(&std::path::Path)) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        // Short: Unix socket paths are limited to about 100 bytes.
        let state = std::env::temp_dir().join(format!("ilg-agt-in-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        std::fs::create_dir_all(&state).unwrap();
        setup(&state);
        let sessions = sessions();
        let b = Self::builder(&sessions).state_dir(state);
        let b = if args.is_empty() { b.no_wisp() } else { b.args(args) };
        Self { d: b.envs(env.iter().copied()).start(), sessions }
    }

    /// Under systemd (FD store, scopes); `None` without a user manager.
    pub fn service() -> Option<Self> {
        Self::service_env(&[])
    }

    /// ...with extra environment.
    pub fn service_env(env: &[(&str, &str)]) -> Option<Self> {
        let sessions = sessions();
        let Some(d) = Self::builder(&sessions).no_wisp().envs(env.iter().copied()).service() else {
            let _ = std::fs::remove_dir_all(&sessions);
            return None;
        };
        Some(Self { d, sessions })
    }

    fn builder(sessions: &std::path::Path) -> Builder {
        illogicald!("agt").env("FAKE_ACP_DIR", sessions).wait_secs(20)
    }

    /// A request over TCP from a tailnet user, as `tailscale serve` hands
    /// it on (the daemon needs `--owner`, and someone else's login here).
    pub fn raw_as(&self, login: &str, method: &str, path: &str, body: Option<Value>) -> (u16, String) {
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let headers = [("Tailscale-User-Login", login), ("Content-Type", "application/json")];
        let (status, _, body) = self.tcp(method, path, &headers, Some(&body));
        (status, body)
    }

    pub fn call(&self, id: u64, method: &str, args: Value) -> Value {
        self.post(&format!("/api/blocks/{id}/call/{method}"), args)
    }

    pub fn state(&self, id: u64) -> Value {
        self.get(&format!("/api/blocks/{id}"))["state"].clone()
    }

    pub fn wait(&self, id: u64, until: &str) -> String {
        self.wait_secs(id, until, 20)
    }

    pub fn wait_secs(&self, id: u64, until: &str, secs: u64) -> String {
        let v = self.get(&format!("/api/panes/{id}/wait?until={until}&timeout={secs}"));
        assert_ne!(v["result"], "timeout", "waiting for {until}: {}", self.state(id));
        v["state"].as_str().unwrap_or("").to_owned()
    }

    pub fn open(&self, prompt: &str) -> u64 {
        let config = json!({ "agent": "acp", "command": ["python3", fake()], "cwd": self.sessions, "prompt": prompt });
        self.open_with(json!({ "type": "agent", "config": config }))
    }

    pub fn open_with(&self, req: Value) -> u64 {
        self.post("/api/blocks", req)["block"].as_u64().unwrap()
    }
}

pub fn entries(state: &Value) -> Vec<Value> {
    state["entries"].as_array().cloned().unwrap_or_default()
}

pub fn last_tool(state: &Value) -> Value {
    entries(state).into_iter().rev().find(|e| e["type"] == "tool").unwrap_or_default()
}

/// Running: it exists and isn't a zombie (`ps`, so it works without /proc).
pub fn alive(pid: u64) -> bool {
    let out = std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    !stat.trim().is_empty() && !stat.trim_start().starts_with('Z')
}

/// A notification as the browser whose keys these are reads it (RFC 8291).
pub fn open_push(ua: &p256::SecretKey, auth: &[u8], body: &[u8]) -> Value {
    use aes_gcm::{Aes128Gcm, KeyInit, aead::Aead};
    use hkdf::Hkdf;
    use p256::PublicKey;
    use sha2::Sha256;

    let (salt, rest) = body.split_at(16);
    let idlen = rest[4] as usize;
    let (as_public, sealed) = rest[5..].split_at(idlen);
    let shared =
        p256::ecdh::diffie_hellman(ua.to_nonzero_scalar(), PublicKey::from_sec1_bytes(as_public).unwrap().as_affine());
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(&ua.public_key().to_sec1_bytes());
    info.extend_from_slice(as_public);
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth), shared.raw_secret_bytes().as_ref()).expand(&info, &mut ikm).unwrap();
    let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
    let (mut cek, mut nonce) = ([0u8; 16], [0u8; 12]);
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).unwrap();
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).unwrap();
    let mut plain = Aes128Gcm::new_from_slice(&cek).unwrap().decrypt(&nonce.into(), sealed).unwrap();
    plain.pop();
    serde_json::from_slice(&plain).unwrap()
}

/// A phone subscribed to push notifications: a push service of our own,
/// whose messages it decrypts as a browser would (RFC 8291).
pub struct Phone {
    service: TcpListener,
    ua: p256::SecretKey,
    ua_public: Vec<u8>,
    auth: [u8; 16],
}

impl Phone {
    pub fn subscribe(d: &Daemon) -> Self {
        Self::subscribe_as(d, None)
    }

    /// Subscribed as a tailnet user (M29), or as the owner.
    pub fn subscribe_as(d: &Daemon, login: Option<&str>) -> Self {
        let phone = Self::bind();
        let sub = phone.subscription();
        match login {
            None => {
                d.post("/api/push/subscribe", sub);
            }
            Some(l) => {
                let (status, body) = d.raw_as(l, "POST", "/api/push/subscribe", Some(sub));
                assert_eq!(status, 200, "{l} subscribing: {body}");
            }
        }
        phone
    }

    /// A phone nobody has subscribed yet.
    pub fn bind() -> Self {
        let service = TcpListener::bind("127.0.0.1:0").unwrap();
        let ua = p256::SecretKey::from_slice(&[7u8; 32]).unwrap();
        let ua_public = ua.public_key().to_sec1_bytes().to_vec();
        Self { service, ua, ua_public, auth: [9u8; 16] }
    }

    /// Its subscription, as `PushSubscription.toJSON()` gives it.
    pub fn subscription(&self) -> Value {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
        let endpoint = format!("http://127.0.0.1:{}/push/abc", self.service.local_addr().unwrap().port());
        json!({"endpoint": endpoint, "keys": {"p256dh": B64.encode(&self.ua_public), "auth": B64.encode(self.auth)}})
    }

    /// Whether nothing came for `ms`.
    pub fn quiet(&self, ms: u64) -> bool {
        self.service.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < deadline {
            if self.service.accept().is_ok() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        true
    }

    /// The next push.
    pub fn next(&self) -> Value {
        self.service.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut conn = loop {
            match self.service.accept() {
                Ok((c, _)) => break c,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "no push came");
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("push service: {e}"),
            }
        };
        conn.set_nonblocking(false).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let mut r = BufReader::new(conn.try_clone().unwrap());
        let mut len = 0;
        loop {
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                len = v.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; len];
        r.read_exact(&mut body).unwrap();
        write!(conn, "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        open_push(&self.ua, &self.auth, &body)
    }

    /// Pushes come for each attention change nobody is looking at; the next
    /// one that asks for you.
    pub fn needs_you(&self) -> Value {
        loop {
            let msg = self.next();
            if msg["title"] == "Needs you" {
                return msg;
            }
        }
    }
}

/// An MCP client as Claude Code in a terminal pane starts one: `illogical
/// mcp` on the daemon's socket, with `ILLOGICAL_PANE` (or none), for tests
/// that aren't async (#234).
pub struct Mcp {
    rt: tokio::runtime::Runtime,
    session: Option<rmcp::service::RunningService<rmcp::RoleClient, McpClient>>,
}

#[derive(Clone)]
pub struct McpClient;

impl rmcp::ClientHandler for McpClient {
    fn get_info(&self) -> rmcp::model::ClientConfig {
        rmcp::model::ClientConfig::new(
            rmcp::model::ClientCapabilities::default(),
            rmcp::model::Implementation::new("claude-code", "1"),
        )
    }
}

impl Mcp {
    pub fn bridge(d: &Daemon, pane: Option<u64>) -> Self {
        use rmcp::ServiceExt;
        let bin = std::path::Path::new(env!("CARGO_BIN_EXE_illogicald")).with_file_name("illogical");
        let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "illogical"]).status().unwrap();
        assert!(status.success(), "building the CLI");
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let mut cmd = tokio::process::Command::new(bin);
        cmd.arg("--socket").arg(d.sock()).arg("mcp").env_remove("ILLOGICAL_PANE");
        if let Some(p) = pane {
            cmd.env("ILLOGICAL_PANE", p.to_string());
        }
        let session = rt.block_on(async {
            McpClient.serve(rmcp::transport::TokioChildProcess::new(cmd).unwrap()).await.expect("illogical mcp")
        });
        Self { rt, session: Some(session) }
    }

    /// A tool's structured result, or its error sentence.
    pub fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
        let s = self.session.as_ref().unwrap();
        let params = rmcp::model::CallToolRequestParams::new(tool.to_owned())
            .with_arguments(args.as_object().cloned().unwrap_or_default());
        let r = self.rt.block_on(s.call_tool(params)).map_err(|e| format!("{tool}: {e}"))?;
        if r.is_error == Some(true) {
            return Err(r.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect());
        }
        Ok(r.structured_content.unwrap_or_default())
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        if let Some(s) = self.session.take() {
            let _ = self.rt.block_on(s.cancel());
        }
    }
}
