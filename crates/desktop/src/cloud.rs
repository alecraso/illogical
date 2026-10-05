//! The app as a client of illogical control (M48, #159): once this machine
//! is joined, the window is control's own client, with every machine in
//! the account and the team, each reached directly or through the relay,
//! end to end. Before that it's the local page (whose Getting started has
//! the button that joins).
//!
//! Signing in happens in the person's browser, where passkeys work (an
//! unsigned app's webview has none): the app makes a verifier it keeps,
//! listens on a loopback port, asks control for a ticket, and opens
//! control's page on it. Once the person allows it there, their browser
//! hands a one-time grant to that port (so only the app on the computer
//! the browser runs on hears it), and the window opens control's page with
//! the grant and the verifier, which signs the window in (control's
//! `app_login.rs`).
//! The window is then a new device, which a trusted device approves on
//! control's page as any browser is. All of that is control's page's own
//! code; this module only routes the window and runs the ticket.

use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use serde_json::{Value, json};
use tauri::{AppHandle, Manager};

/// The app's own sign-in page.
pub const SIGNIN: &str = "signin.html";

fn agent() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into())
}

/// What the local daemon says about itself, read when a window opens.
#[derive(Clone, Default)]
pub struct Local {
    /// This machine's name, for the device ("illogical app on jake-air").
    pub name: String,
    /// The control it's joined to (`ILLOGICAL_CONTROL` overrides, for tests).
    pub control: Option<String>,
}

/// The control a machine joins by default (the daemon's `setup::CONTROL`).
const CONTROL: &str = "https://control.illogical.widgets.wtf";

static LOCAL: Mutex<Option<Local>> = Mutex::new(None);
/// "Just this machine" for the rest of this run.
static LOCAL_ONLY: Mutex<bool> = Mutex::new(false);

pub fn local() -> Local {
    if crate::DAEMONLESS {
        // No daemon to ask: this computer's name, and the default control.
        let control = std::env::var("ILLOGICAL_CONTROL").unwrap_or_else(|_| CONTROL.into());
        let l = Local {
            name: std::env::var("COMPUTERNAME").unwrap_or_default(),
            control: Some(control.trim_end_matches('/').to_owned()),
        };
        *LOCAL.lock().unwrap() = Some(l.clone());
        return l;
    }
    let read = || -> Option<Local> {
        let mut req = agent().get(&format!("{}/api/host", crate::page()));
        if let Some(b) = crate::bearer() {
            req = req.header("Authorization", &b);
        }
        let v: Value = req.call().ok()?.body_mut().read_json().ok()?;
        let control = std::env::var("ILLOGICAL_CONTROL")
            .ok()
            .or_else(|| v["control"].as_str().map(str::to_owned))
            .map(|c| c.trim_end_matches('/').to_owned());
        Some(Local { name: v["name"].as_str().unwrap_or("this machine").to_owned(), control })
    };
    match read() {
        Some(l) => {
            *LOCAL.lock().unwrap() = Some(l.clone());
            l
        }
        None => LOCAL.lock().unwrap().clone().unwrap_or_default(),
    }
}

/// The control the window may show, from what was last read.
pub fn control() -> Option<String> {
    LOCAL.lock().unwrap().as_ref().and_then(|l| l.control.clone())
}

pub fn device_name() -> String {
    let name = LOCAL.lock().unwrap().as_ref().map(|l| l.name.clone()).unwrap_or_default();
    if name.is_empty() { "illogical app".into() } else { format!("illogical app on {name}") }
}

pub fn set_local_only(on: bool) {
    *LOCAL_ONLY.lock().unwrap() = on;
}

pub fn local_only() -> bool {
    *LOCAL_ONLY.lock().unwrap()
}

/// Beside the window's profile when it has one of its own (`profile.rs`):
/// its sessions are its own.
fn state_file(app: &AppHandle) -> Option<PathBuf> {
    crate::profile::dir().or_else(|| app.path().app_config_dir().ok()).map(|d| d.join("cloud.json"))
}

/// Whether the window holds a session on `control` (as far as the app
/// knows: control's page says so if it lapsed, and the window comes back
/// here).
pub fn signed_in(app: &AppHandle, control: &str) -> bool {
    state_file(app)
        .and_then(|f| std::fs::read(f).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .is_some_and(|v| v["signed_in"][control].as_bool() == Some(true))
}

pub fn set_signed_in(app: &AppHandle, control: &str, on: bool) {
    let Some(f) = state_file(app) else { return };
    let mut v: Value =
        std::fs::read(&f).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| json!({}));
    v["signed_in"][control] = json!(on);
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, serde_json::to_vec_pretty(&v).unwrap_or_default());
}

/// The app's own page, as a URL to navigate a window to.
pub fn app_url(page: &str) -> tauri::Url {
    let base = if cfg!(windows) { "http://tauri.localhost/" } else { "tauri://localhost/" };
    format!("{base}{page}").parse().unwrap()
}

/// Whether `url` is control's own sign-in, which can't finish in the
/// window (GitHub's pages, passkeys): the app's sign-in takes over.
pub fn is_control_signin(url: &tauri::Url) -> bool {
    let Some(c) = control() else { return false };
    url.as_str().starts_with(&format!("{c}/auth/github"))
}

/// Script for every page in the app's windows: the name control's page
/// gives this device, and no passkey button where passkeys can't work.
pub fn init_script() -> String {
    let control = control().unwrap_or_default();
    format!(
        "window.__illogicalApp = {{ name: {} }};\n\
         if ({control:?} && location.origin === new URL({control:?}).origin) {{\n\
           addEventListener('DOMContentLoaded', () => {{\n\
             const s = document.createElement('style');\n\
             s.textContent = '[data-signin=passkey] {{ display: none !important; }}';\n\
             document.head.appendChild(s);\n\
           }});\n\
         }}",
        serde_json::to_string(&device_name()).unwrap(),
    )
}

#[derive(serde::Serialize)]
pub struct Status {
    control: Option<String>,
    name: String,
    /// `ILLOGICAL_SIGNIN_AUTO=1` (for tests): start signing in on load.
    auto: bool,
    /// No local daemon (Windows until M59): no "just this machine".
    daemonless: bool,
}

#[tauri::command]
pub fn cloud_status() -> Status {
    Status {
        control: control(),
        name: device_name(),
        auto: std::env::var_os("ILLOGICAL_SIGNIN_AUTO").is_some(),
        daemonless: crate::DAEMONLESS,
    }
}

#[derive(serde::Serialize)]
pub struct Started {
    code: String,
    url: String,
}

/// Start signing in: a verifier and a loopback port, a ticket from
/// control, its page in the browser, and a listener that sends `window` to
/// redeem it once the person's browser hands over the grant.
#[tauri::command]
pub async fn cloud_signin(app: AppHandle, window: tauri::WebviewWindow) -> Result<Started, String> {
    use sha2::{Digest, Sha256};
    let control = control().ok_or("this machine isn't joined to illogical cloud")?;
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(|e| e.to_string())?;
    let verifier = hex(&secret);
    let challenge = hex(&Sha256::digest(verifier.as_bytes()));
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("can't listen for the sign-in: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let c = control.clone();
    let t: Value = tauri::async_runtime::spawn_blocking(move || {
        let mut r = agent()
            .post(&format!("{c}/auth/app"))
            .config()
            .http_status_as_error(false)
            .build()
            .send_json(json!({ "name": device_name(), "challenge": challenge, "port": port }))
            .map_err(|e| format!("can't reach {c}: {e}"))?;
        let ok = r.status().is_success();
        let v = r.body_mut().read_json::<Value>().map_err(|e| e.to_string())?;
        if !ok {
            return Err(v["error"].as_str().unwrap_or("control refused the sign-in").to_owned());
        }
        Ok(v)
    })
    .await
    .map_err(|e| e.to_string())??;
    let get = |k: &str| t[k].as_str().map(str::to_owned).ok_or(format!("control's answer has no {k}"));
    let (id, code, url) = (get("ticket")?, get("code")?, get("url")?);
    if let Ok(u) = url.parse::<tauri::Url>() {
        crate::open_outside(&u);
    }
    let label = window.label().to_owned();
    std::thread::spawn(move || {
        let Some(grant) = await_grant(&listener, &id, &control) else { return };
        let redeem: tauri::Url = format!("{control}/#app-redeem={id}.{grant}.{verifier}").parse().unwrap();
        set_signed_in(&app, &control, true);
        let a = app.clone();
        let _ = app.run_on_main_thread(move || {
            if let Some(w) = a.get_webview_window(&label) {
                let _ = w.navigate(redeem);
                let _ = w.set_focus();
            }
        });
    });
    Ok(Started { code, url })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The grant for ticket `id`, when the person's browser brings it to the
/// loopback port (`GET /illogical-signin?ticket=…&grant=…`); none if ten
/// minutes pass. The browser goes back to control's page.
fn await_grant(listener: &std::net::TcpListener, id: &str, control: &str) -> Option<String> {
    use std::io::{BufRead, BufReader, Write};
    let deadline = std::time::Instant::now() + Duration::from_secs(600);
    listener.set_nonblocking(true).ok()?;
    while std::time::Instant::now() < deadline {
        let conn = match listener.accept() {
            Ok((c, _)) => c,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            Err(_) => return None,
        };
        let _ = conn.set_nonblocking(false);
        let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
        let mut line = String::new();
        let mut r = BufReader::new(&conn);
        if r.read_line(&mut line).is_err() {
            continue;
        }
        let target = line.split_whitespace().nth(1).unwrap_or_default();
        let query = target.strip_prefix("/illogical-signin?").unwrap_or_default();
        let param = |k: &str| {
            query.split('&').find_map(|kv| kv.strip_prefix(k).and_then(|v| v.strip_prefix('='))).map(str::to_owned)
        };
        let ok = param("ticket").as_deref() == Some(id);
        let grant = param("grant").filter(|g| ok && !g.is_empty() && g.bytes().all(|c| c.is_ascii_hexdigit()));
        let reply = match &grant {
            Some(_) => format!(
                "HTTP/1.1 303 See Other\r\nLocation: {control}/#app-done\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ),
            None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        };
        let _ = (&conn).write_all(reply.as_bytes());
        if grant.is_some() {
            return grant;
        }
    }
    None
}

/// "Just this machine": the local page, for the rest of this run.
#[tauri::command]
pub fn cloud_local(window: tauri::WebviewWindow) -> Result<(), String> {
    if crate::DAEMONLESS {
        return Err("this computer can't run panes yet".into());
    }
    set_local_only(true);
    window.navigate(crate::page_at("/")).map_err(|e| e.to_string())
}
