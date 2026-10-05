//! Test daemons listen on `127.0.0.1:0` and say which port they got. A port
//! picked by binding and letting go of it could be taken again before the
//! daemon binds it, and the daemon would exit (#66).

use std::{
    path::Path,
    time::{Duration, Instant},
};

/// What to pass to `--listen`: a port picked by the daemon.
pub const ANY: &str = "127.0.0.1:0";

/// The port the daemon with this state dir took, once it has.
pub fn port(state: &Path) -> Option<u16> {
    recorded(state, "listen")
}

/// The port, waiting for it.
pub fn wait_port(state: &Path) -> u16 {
    wait(state, "listen")
}

/// The block sites' port (`--block-listen 127.0.0.1:0`), waiting for it.
pub fn wait_block_port(state: &Path) -> u16 {
    wait(state, "block-listen")
}

fn recorded(state: &Path, file: &str) -> Option<u16> {
    let addr = std::fs::read_to_string(state.join(file)).ok()?;
    addr.trim().rsplit(':').next()?.parse().ok()
}

fn wait(state: &Path, file: &str) -> u16 {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(p) = recorded(state, file) {
            return p;
        }
        assert!(Instant::now() < deadline, "daemon did not start: no port in {}", state.join(file).display());
        std::thread::sleep(Duration::from_millis(20));
    }
}
