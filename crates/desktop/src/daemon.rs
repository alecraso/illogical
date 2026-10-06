//! The local daemon, as the app's menus and notifications show it.
//!
//! - **Dropped by control** (#325): a thread (`follow`) reads `/api/host`'s
//!   `control_state` every few seconds and posts one native notification
//!   when control drops this machine (a new `dropped_ms`, remembered
//!   across launches in `daemon.json` beside the app's settings). Its click
//!   opens the daemon's page at Getting started's cloud step, to join
//!   again.

use std::{sync::OnceLock, time::Duration};

use illogical_proto::hosts::ControlState;
use tauri::{AppHandle, Manager};

/// How often the thread reads the daemon.
const POLL: Duration = Duration::from_secs(10);

fn agent() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(5))).build().into())
}

/// `GET /api/host`, with the local token; `None` when it doesn't answer.
pub fn host() -> Option<serde_json::Value> {
    let mut req = agent().get(&format!("{}/api/host", crate::page()));
    if let Some(b) = crate::bearer() {
        req = req.header("Authorization", &b);
    }
    req.call().ok()?.body_mut().read_json().ok()
}

/// The thread: a notification when control drops this machine.
pub fn follow(app: AppHandle) {
    loop {
        if let Some(c) = host().as_ref().and_then(ControlState::of_host) {
            dropped(&app, &c);
        }
        std::thread::sleep(POLL);
    }
}

/// Whether to notify of `c`: a drop whose time isn't `seen`, the last one
/// notified.
fn new_drop(c: &ControlState, seen: Option<u64>) -> Option<u64> {
    c.dropped_ms.filter(|at| c.is_dropped() && seen != Some(*at))
}

/// One notification per drop: a `dropped_ms` not seen before, in this run
/// or an earlier one (`daemon.json` beside the app's settings).
fn dropped(app: &AppHandle, c: &ControlState) {
    let file = app.path().app_config_dir().ok().map(|d| d.join("daemon.json"));
    let seen = file
        .as_ref()
        .and_then(|f| std::fs::read(f).ok())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v["dropped_ms"].as_u64());
    let Some(at) = new_drop(c, seen) else { return };
    if let Some(f) = &file {
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(f, serde_json::json!({ "dropped_ms": at }).to_string());
    }
    eprintln!("illogical: control dropped this machine: {}", c.line());
    let what = match &c.code {
        Some(code) => format!("{}. It asks to join again: approve {code} on a device you use.", c.line()),
        None => format!("{}. Click to join again.", c.line()),
    };
    crate::notify(app, crate::Click::JoinAgain, "This machine is no longer in illogical control".into(), what);
}

/// Getting started on the daemon's page, at `section` (`cloud`: to join
/// control, or join again, #325), in a window of ours. Not while the
/// daemon and the app don't match: the setup page says why.
#[cfg(not(windows))]
pub fn getting_started(app: &AppHandle, section: &str) {
    if crate::compat::mismatch().is_some() {
        return crate::focus_or_open(app);
    }
    #[cfg(target_os = "macos")]
    let _ = app.show();
    let on_page = app.webview_windows().into_values().find(|w| w.url().is_ok_and(|u| crate::daemons(&u)));
    match on_page {
        // The client opens it (main.tsx) without a reload.
        Some(w) => {
            let _ = w.eval(format!(
                "dispatchEvent(new CustomEvent('illogical:getting-started', {{ detail: {section:?} }}))"
            ));
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
        None => {
            let _ = crate::open_window(
                app,
                tauri::WebviewUrl::External(crate::page_at(&format!("/#getting-started={section}"))),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_notification_per_drop() {
        let dropped = ControlState { state: "dropped".into(), dropped_ms: Some(7), ..Default::default() };
        assert_eq!(new_drop(&dropped, None), Some(7));
        assert_eq!(new_drop(&dropped, Some(7)), None, "already said");
        assert_eq!(new_drop(&dropped, Some(3)), Some(7), "another drop");
        let joined = ControlState { state: "joined".into(), ..Default::default() };
        assert_eq!(new_drop(&joined, None), None);
    }
}
