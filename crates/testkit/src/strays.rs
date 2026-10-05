//! Programs a test daemon leaves behind (#35), and its state dir (#68).

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[cfg(unix)]
use nix::{sys::signal, unistd::Pid};

/// Kill what a stopped daemon's panes left running, then delete its state
/// dir. Call it after the daemon is stopped: then a test leaves nothing
/// behind, whatever the daemon did.
///
/// A pane's shim outlives the daemon and appends how its program ended to
/// `blocks/<id>/process`. That lands as soon as the program goes (a hangup
/// when the daemon dies, or the kill here), and if it lands while the dir is
/// being deleted, the delete fails and leaves `blocks/<id>/process` behind.
/// So: wait for the shims to finish, and retry the delete.
pub fn remove(state: &Path) {
    let shims = kill_programs(state);
    let deadline = Instant::now() + Duration::from_secs(5);
    while shims.iter().any(|(record, shim)| !ended(record) && alive(*shim)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    for _ in 0..50 {
        let _ = std::fs::remove_dir_all(state);
        if !state.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Kill the process group of every program a daemon's state dir records as
/// still running. Returns each record with a shim still to write its end.
pub fn kill_programs(state: &Path) -> Vec<(PathBuf, i32)> {
    let mut shims = Vec::new();
    for sub in ["blocks", "closed"] {
        let Ok(dirs) = std::fs::read_dir(state.join(sub)) else { continue };
        for d in dirs.flatten() {
            let record = d.path().join("process");
            let Some((pid, shim)) = running(&record) else { continue };
            #[cfg(unix)]
            if pid > 1 {
                let _ = signal::killpg(Pid::from_raw(pid), signal::Signal::SIGKILL);
            }
            #[cfg(not(unix))]
            let _ = pid;
            if let Some(s) = shim.filter(|s| *s > 1) {
                shims.push((record, s));
            }
        }
    }
    shims
}

/// The program (and its shim) of the last start, unless an end follows it.
fn running(record: &Path) -> Option<(i32, Option<i32>)> {
    let text = std::fs::read_to_string(record).ok()?;
    let mut last = None;
    for line in text.lines() {
        match line.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["pid", p, _] => last = p.parse::<i32>().ok().map(|p| (p, None)),
            ["shim", s, _] => {
                if let Some((_, shim)) = last.as_mut() {
                    *shim = s.parse().ok();
                }
            }
            ["exit" | "signal", _] => last = None,
            _ => {}
        }
    }
    last
}

fn ended(record: &Path) -> bool {
    running(record).is_none()
}

/// Windows has no shims: a pane's program is in a job that ends with the
/// daemon, so there's nothing left to wait for.
fn alive(pid: i32) -> bool {
    #[cfg(unix)]
    return signal::kill(Pid::from_raw(pid), None).is_ok();
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}
