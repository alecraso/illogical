//! `ssh box s28-bridge`: copy stdin to the daemon's Unix socket and the
//! socket to stdout, until either side closes. Finds the socket the way the
//! CLI does: $ILLOGICAL_SOCK, then <state>/sock.path, then <state>/sock.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::{env, fs, thread};

fn socket() -> PathBuf {
    if let Some(p) = env::var_os("ILLOGICAL_SOCK") {
        return p.into();
    }
    let state = env::var_os("ILLOGICAL_STATE_DIR").map(PathBuf::from).unwrap_or_else(|| {
        env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".local/state"))
            .join("illogical")
    });
    match fs::read_to_string(state.join("sock.path")) {
        Ok(p) => p.trim().into(),
        Err(_) => state.join("sock"),
    }
}

fn main() {
    let path = socket();
    let sock = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("s28-bridge: can't reach illogicald at {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    let mut up = sock.try_clone().expect("clone");
    // No shutdown(Write) at stdin's EOF: the daemon (hyper) takes a half-close
    // as the client going away and drops a request it hasn't answered yet.
    // The run ends when the daemon closes its side.
    let t = thread::spawn(move || {
        let _ = io::copy(&mut io::stdin().lock(), &mut up);
    });
    let mut down = sock;
    let mut out = io::stdout().lock();
    let mut buf = [0u8; 16384];
    loop {
        match down.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if out.write_all(&buf[..n]).and_then(|_| out.flush()).is_err() {
                    break;
                }
            }
        }
    }
    drop(t);
}
