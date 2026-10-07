//! `arugula://` links (M46), and `illogical://` ones from before the
//! rename (#505):
//!
//! - `arugula://pane/%3` (or `pane/3`): that pane on this machine's
//!   daemon, in a window of ours;
//! - `arugula://open?cwd=/some/dir`: a new tab there, shown; with no
//!   `cwd`, a new tab in the home directory.
//!
//! macOS hands links to the running app (or starts it) through the URL
//! scheme in Info.plist (tauri-plugin-deep-link). Linux runs the app with
//! the link as an argument (the .desktop file's `x-scheme-handler`), so a
//! second launch's arguments reach the first through single-instance.
//!
//! The file managers (M47) use the same path: Nautilus's extension opens
//! `arugula://open?cwd=`, and Finder's service calls `open_dir` here.
//! A `.command` file opened with the app (`run_file`) runs in a new tab;
//! that never comes from a link, so no web page can run a command.

use std::{path::Path, time::Duration};

use tauri::AppHandle;

#[derive(Debug, PartialEq)]
pub enum Link {
    Pane(u32),
    Open { cwd: Option<String> },
}

/// What to do once the daemon answers.
enum Act {
    Show(u32),
    Run { cwd: Option<String>, command: Option<String> },
}

/// `illogical://` too (#505): links in people's notes and scripts from
/// before the rename keep working.
const SCHEMES: [&str; 2] = ["arugula", arugula_proto::rename::OLD];

pub fn parse(url: &str) -> Option<Link> {
    let url = tauri::Url::parse(url).ok()?;
    if !SCHEMES.contains(&url.scheme()) {
        return None;
    }
    let path = url.path().trim_matches('/');
    match url.host_str()? {
        "pane" => {
            // `%3` as the CLI prints it; a browser may send it escaped.
            let id = path.trim_start_matches("%25").trim_start_matches('%');
            id.parse().ok().map(Link::Pane)
        }
        "open" if path.is_empty() => {
            let cwd =
                url.query_pairs().find(|(k, _)| k == "cwd").map(|(_, v)| v.into_owned()).filter(|v| !v.is_empty());
            Some(Link::Open { cwd })
        }
        _ => None,
    }
}

/// The links among a launch's arguments.
pub fn in_args<I: IntoIterator<Item = String>>(args: I) -> Vec<String> {
    args.into_iter()
        .filter(|a| SCHEMES.iter().any(|s| a.strip_prefix(s).is_some_and(|r| r.starts_with("://"))))
        .collect()
}

/// Do what `url` says. Waits (off the main thread) for the daemon, which a
/// first launch may still be installing.
pub fn handle(app: &AppHandle, url: String) {
    let Some(link) = parse(&url) else {
        eprintln!("arugula: not a link this app knows: {url}");
        let a = app.clone();
        let _ = app.run_on_main_thread(move || crate::focus_or_open(&a));
        return;
    };
    eprintln!("arugula: opening {url}");
    let act = match link {
        Link::Pane(p) => Act::Show(p),
        Link::Open { cwd } => Act::Run { cwd, command: None },
    };
    act_on(app, url, act);
}

/// A new tab in `dir` (Finder's service, M47). A file opens in its folder.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn open_dir(app: &AppHandle, dir: &Path) {
    let dir = if dir.is_dir() { dir } else { dir.parent().unwrap_or(dir) };
    eprintln!("arugula: a new tab in {}", dir.display());
    act_on(app, dir.display().to_string(), Act::Run { cwd: Some(dir.display().to_string()), command: None });
}

/// A `.command` file opened with the app (M47): it runs in a new tab, in
/// its own folder, as Terminal runs it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn run_file(app: &AppHandle, file: &Path) {
    eprintln!("arugula: running {}", file.display());
    let cwd = file.parent().map(|d| d.display().to_string());
    act_on(app, file.display().to_string(), Act::Run { cwd, command: Some(quote(&file.display().to_string())) });
}

/// `s` as one word for the pane's shell.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn act_on(app: &AppHandle, what: String, act: Act) {
    let app = app.clone();
    std::thread::spawn(move || {
        for _ in 0..240 {
            if crate::reachable() {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let pane = match act {
            Act::Show(p) => p,
            Act::Run { cwd, command } => match run(cwd.as_deref(), command.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("arugula: {what}: {e}");
                    let a = app.clone();
                    let _ = app.run_on_main_thread(move || crate::focus_or_open(&a));
                    return;
                }
            },
        };
        let a = app.clone();
        let _ = app.run_on_main_thread(move || crate::open_pane(&a, pane));
    });
}

/// A new tab on the local daemon (`POST /api/run`), in `cwd`, running
/// `command` (or a shell).
fn run(cwd: Option<&str>, command: Option<&str>) -> Result<u32, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into();
    let mut req = agent.post(&format!("{}/api/run", crate::page()));
    if let Some(b) = crate::bearer() {
        req = req.header("Authorization", &b);
    }
    let body = serde_json::json!({ "cwd": cwd, "command": command });
    let mut resp = req.send_json(&body).map_err(|e| e.to_string())?;
    let v: serde_json::Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;
    v["pane"].as_u64().map(|p| p as u32).ok_or_else(|| format!("the daemon answered {v}"))
}

#[cfg(test)]
mod tests {
    use super::{Link, parse};

    #[test]
    fn reads_links() {
        assert_eq!(parse("arugula://pane/%3"), Some(Link::Pane(3)));
        assert_eq!(parse("arugula://pane/%253"), Some(Link::Pane(3)));
        assert_eq!(parse("arugula://pane/12"), Some(Link::Pane(12)));
        assert_eq!(parse("arugula://pane/12/"), Some(Link::Pane(12)));
        assert_eq!(parse("arugula://open?cwd=%2Ftmp%2Fa%20b"), Some(Link::Open { cwd: Some("/tmp/a b".into()) }));
        assert_eq!(parse("arugula://open"), Some(Link::Open { cwd: None }));
        assert_eq!(parse("arugula://open?cwd="), Some(Link::Open { cwd: None }));
        assert_eq!(parse("arugula://pane/x"), None);
        assert_eq!(parse("arugula://pane/"), None);
        assert_eq!(parse("arugula://delete/everything"), None);
        assert_eq!(parse("https://pane/3"), None);
        // #505: the old scheme too.
        assert_eq!(parse("illogical://pane/%3"), Some(Link::Pane(3)));
        assert_eq!(parse("illogical://open?cwd=%2Ftmp"), Some(Link::Open { cwd: Some("/tmp".into()) }));
        assert_eq!(parse("illogical://delete/everything"), None);
    }

    #[test]
    fn quotes_paths() {
        assert_eq!(super::quote("/tmp/a b/x.command"), "'/tmp/a b/x.command'");
        assert_eq!(super::quote("/tmp/it's"), r"'/tmp/it'\''s'");
    }

    #[test]
    fn picks_links_from_args() {
        let args = ["/usr/bin/arugula-desktop", "arugula://pane/3", "--x", "illogical://open", "illogicalx://y"]
            .map(String::from);
        assert_eq!(super::in_args(args), ["arugula://pane/3", "illogical://open"]);
    }
}
