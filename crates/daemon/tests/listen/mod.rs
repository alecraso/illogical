//! Test daemons listen on `127.0.0.1:0` and say which port they got. A port
//! picked by binding and letting go of it could be taken again before the
//! daemon binds it, and the daemon would exit (#66).

#![allow(dead_code)]

use std::{
    path::Path,
    time::{Duration, Instant},
};

/// What to pass to `--listen`: a port picked by the daemon.
pub const ANY: &str = "127.0.0.1:0";

/// The port the daemon with this state dir took, once it has.
pub fn port(state: &Path) -> Option<u16> {
    let addr = std::fs::read_to_string(state.join("listen")).ok()?;
    addr.trim().rsplit(':').next()?.parse().ok()
}

/// The port, waiting for it.
pub fn wait_port(state: &Path) -> u16 {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(p) = port(state) {
            return p;
        }
        assert!(Instant::now() < deadline, "daemon did not start: no port in {}", state.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}
