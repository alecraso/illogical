//! #233: `illogical invite --root` shows the root's fingerprint and goes no
//! further without `--yes` (or a yes typed at a terminal): nothing reaches
//! the daemon.

use std::process::{Command, Stdio};

#[test]
fn an_explicit_root_without_yes_prints_the_fingerprint_and_stops() {
    let dir = std::env::temp_dir().join(format!("ilg-cli-invite-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // A daemon that notes any connection at all.
    let sock = dir.join("sock");
    let _ = std::fs::remove_file(&sock);
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    listener.set_nonblocking(true).unwrap();

    let root = "0123456789abcdef0123456789abcdef";
    let out = Command::new(env!("CARGO_BIN_EXE_illogical"))
        .args(["--socket", sock.to_str().unwrap(), "invite", "account:x1", "--session", "api", "--root", root])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("0123-4567-89ab-cdef-0123-4567-89ab-cdef"), "{err}");
    assert!(err.contains("--yes"), "{err}");
    assert!(listener.accept().is_err(), "nothing was sent");

    // With --yes it goes on to the daemon.
    let mut child = Command::new(env!("CARGO_BIN_EXE_illogical"))
        .args(["--socket", sock.to_str().unwrap(), "invite", "account:x1", "--session", "api", "--root", root, "--yes"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let asked = loop {
        if listener.accept().is_ok() {
            break true;
        }
        if std::time::Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(asked, "--yes asks the daemon");
}
