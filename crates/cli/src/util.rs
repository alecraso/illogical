//! What several subcommands share: the pane argument, where a path or a pane comes from, durations and
//! times as people read them.

use crate::http::{self, request};
use anyhow::{Context, bail};
use serde_json::Value;

/// Talking to another daemon (`--host`): this shell's pane and directory
/// mean nothing there.
pub static REMOTE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The pane we're running in, if it is on the daemon we're talking to.
pub fn env_pane() -> Option<u32> {
    if REMOTE.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    std::env::var("ARUGULA_PANE").ok().and_then(|v| v.parse().ok())
}

/// Panes are `%N` or `N`; commands default to the pane they run in
/// ($ARUGULA_PANE).
#[derive(Clone, Debug)]
pub struct Pane(pub u32);

impl std::str::FromStr for Pane {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        s.trim_start_matches('%').parse().map(Pane).map_err(|_| format!("not a pane: {s} (want %N or N)"))
    }
}

/// The pane given, or the one we're running in.
/// `src/main.rs:42` is a file and a line (when `check`, only if the file
/// is there and the whole name isn't).
pub fn file_line(p: &str, check: bool) -> (String, Option<u32>) {
    if let Some((file, line)) = p.rsplit_once(':')
        && let Ok(n) = line.parse::<u32>()
        && !file.is_empty()
        && (!check || (std::path::Path::new(file).exists() && !std::path::Path::new(p).exists()))
    {
        return (file.to_owned(), Some(n));
    }
    (p.to_owned(), None)
}

/// A path from here as a whole one.
pub fn absolute(p: &str) -> anyhow::Result<String> {
    let p = std::path::Path::new(p);
    let whole = if p.is_absolute() { p.to_owned() } else { std::env::current_dir()?.join(p) };
    Ok(whole.display().to_string())
}

/// `--split right` (the pane this runs in) or `--split %N`.
pub fn split_of(split: Option<&str>) -> anyhow::Result<Option<u32>> {
    Ok(match split {
        None => None,
        Some("right") => Some(here(None)?),
        Some(p) => Some(p.parse::<Pane>().map_err(anyhow::Error::msg)?.0),
    })
}

/// A block's `describe` once it has read what it shows (M11's views read
/// in the background; at most 30s).
pub fn loaded(sock: &http::Target, block: u64) -> anyhow::Result<Value> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let v = request(sock, "GET", &format!("/api/blocks/{block}"), None)?.json()?;
        if v["state"]["loading"] != true || std::time::Instant::now() > deadline {
            return Ok(v);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

pub fn here(p: Option<Pane>) -> anyhow::Result<u32> {
    match p {
        Some(Pane(n)) => Ok(n),
        None => env_pane().context("which pane? (give %N, or run this inside an Arugula pane)"),
    }
}

/// `90s`, `30m`, `2h`, `7d` (or plain seconds) as seconds.
pub fn duration(s: &str) -> anyhow::Result<u64> {
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = n.parse().with_context(|| format!("bad duration {s}"))?;
    Ok(n * match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => bail!("bad duration {s} (use s, m, h or d)"),
    })
}

pub fn time(ms: u64) -> String {
    let secs = ms / 1000;
    let ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().saturating_sub(secs))
        .unwrap_or(0);
    match ago {
        0..60 => format!("{ago}s ago"),
        60..3600 => format!("{}m ago", ago / 60),
        3600..86400 => format!("{}h ago", ago / 3600),
        _ => format!("{}d ago", ago / 86400),
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Seconds as the largest whole unit.
pub fn span(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

pub fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    #[test]
    fn durations_and_policies() {
        assert_eq!(super::duration("90").unwrap(), 90);
        assert_eq!(super::duration("30m").unwrap(), 1800);
        assert_eq!(super::duration("2d").unwrap(), 172800);
        assert!(super::duration("2w").is_err());
        assert_eq!("%12".parse::<super::Pane>().unwrap().0, 12);
    }

    #[test]
    fn files_with_lines() {
        let s = |p: &str, check| super::file_line(p, check);
        assert_eq!(s("src/main.rs:42", false), ("src/main.rs".into(), Some(42)));
        assert_eq!(s("src/main.rs", false), ("src/main.rs".into(), None));
        assert_eq!(s(":42", false), (":42".into(), None));
        assert_eq!(s("a:b", false), ("a:b".into(), None));
        // Here, only when the file is there.
        assert_eq!(s("/nowhere/x.rs:3", true), ("/nowhere/x.rs:3".into(), None));
        let there = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        assert_eq!(s(&format!("{there}:3"), true), (there.into(), Some(3)));
        // Unix paths: on Windows `/a/b` has no drive.
        if cfg!(unix) {
            assert_eq!(super::absolute("/a/b").unwrap(), "/a/b");
            assert!(super::absolute("b").unwrap().ends_with("/b"));
        }
    }
}
