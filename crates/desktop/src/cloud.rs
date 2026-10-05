//! The app as a client of illogical control (M48, #159): once this machine
//! is joined, the window is control's own client, with every machine in
//! the account and the team, each reached directly or through the relay,
//! end to end. Before that it's the local page (whose Getting started has
//! the button that joins).
//!
//! Signing in happens in the person's browser, where passkeys work (an
//! unsigned app's webview has none): the app asks control for a ticket,
//! opens control's page on it, and polls; once the person allows it there,
//! the window opens the ticket's redeem URL, which signs the window in.
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

static LOCAL: Mutex<Option<Local>> = Mutex::new(None);
/// "Just this machine" for the rest of this run.
static LOCAL_ONLY: Mutex<bool> = Mutex::new(false);

pub fn local() -> Local {
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

fn state_file(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("cloud.json"))
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
}

#[tauri::command]
pub fn cloud_status() -> Status {
    Status { control: control(), name: device_name(), auto: std::env::var_os("ILLOGICAL_SIGNIN_AUTO").is_some() }
}

#[derive(serde::Serialize)]
pub struct Started {
    code: String,
    url: String,
}

/// Start signing in: a ticket from control, its page in the browser, and a
/// poll that sends `window` to redeem it once it's allowed.
#[tauri::command]
pub async fn cloud_signin(app: AppHandle, window: tauri::WebviewWindow) -> Result<Started, String> {
    let control = control().ok_or("this machine isn't joined to illogical cloud")?;
    let c = control.clone();
    let t: Value = tauri::async_runtime::spawn_blocking(move || {
        agent()
            .post(&format!("{c}/auth/app"))
            .send_json(json!({ "name": device_name() }))
            .map_err(|e| format!("can't reach {c}: {e}"))?
            .body_mut()
            .read_json::<Value>()
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    let get = |k: &str| t[k].as_str().map(str::to_owned).ok_or(format!("control's answer has no {k}"));
    let (id, secret, code, url) = (get("ticket")?, get("secret")?, get("code")?, get("url")?);
    if let Ok(u) = url.parse::<tauri::Url>() {
        crate::open_outside(&u);
    }
    let label = window.label().to_owned();
    std::thread::spawn(move || {
        let poll = format!("{control}/auth/app/{id}/poll?secret={secret}");
        for _ in 0..300 {
            std::thread::sleep(Duration::from_secs(2));
            let state = agent()
                .get(&poll)
                .call()
                .ok()
                .and_then(|mut r| r.body_mut().read_json::<Value>().ok())
                .and_then(|v| v["state"].as_str().map(str::to_owned));
            match state.as_deref() {
                Some("allowed") => {
                    let redeem: tauri::Url = format!("{control}/auth/app/{id}/redeem?secret={secret}").parse().unwrap();
                    set_signed_in(&app, &control, true);
                    let a = app.clone();
                    let _ = app.run_on_main_thread(move || {
                        if let Some(w) = a.get_webview_window(&label) {
                            let _ = w.navigate(redeem);
                            let _ = w.set_focus();
                        }
                    });
                    return;
                }
                Some("expired") => return,
                _ => {}
            }
        }
    });
    Ok(Started { code, url })
}

/// "Just this machine": the local page, for the rest of this run.
#[tauri::command]
pub fn cloud_local(window: tauri::WebviewWindow) -> Result<(), String> {
    set_local_only(true);
    window.navigate(crate::page_at("/")).map_err(|e| e.to_string())
}
