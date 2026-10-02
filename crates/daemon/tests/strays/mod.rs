//! Programs a test daemon leaves behind (#35).

use std::path::Path;

/// Kill the process group of every program a daemon's state dir records as
/// still running. Call it after the daemon is stopped, before the dir goes:
/// then a test leaves nothing behind, whatever the daemon did.
pub fn kill_programs(state: &Path) {
    for sub in ["blocks", "closed"] {
        let Ok(dirs) = std::fs::read_dir(state.join(sub)) else { continue };
        for d in dirs.flatten() {
            let Ok(text) = std::fs::read_to_string(d.path().join("process")) else { continue };
            // The last start, unless an end follows it.
            let mut pid = None;
            for line in text.lines() {
                match line.split_whitespace().collect::<Vec<_>>().as_slice() {
                    ["pid", p, _] => pid = p.parse::<i32>().ok(),
                    ["exit" | "signal", _] => pid = None,
                    _ => {}
                }
            }
            if let Some(p) = pid.filter(|p| *p > 1) {
                let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(p), nix::sys::signal::Signal::SIGKILL);
            }
        }
    }
}
