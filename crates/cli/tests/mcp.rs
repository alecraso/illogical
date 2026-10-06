//! #234: `illogical mcp` in a pane says which (`$ILLOGICAL_PANE`), so a
//! tool like `invite_person` knows where Claude Code in a terminal works;
//! outside a pane it says nothing.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixListener,
    process::{Command, Stdio},
};

/// The headers of the one request `illogical mcp` sends for a line, with
/// `ILLOGICAL_PANE` as given.
fn headers_sent(pane: Option<&str>) -> String {
    let dir = std::env::temp_dir().join(format!("ilg-cli-mcp-{}-{}", std::process::id(), pane.unwrap_or("none")));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("sock");
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_illogical"));
    cmd.args(["--socket", sock.to_str().unwrap(), "mcp"]).stdin(Stdio::piped()).stdout(Stdio::piped());
    cmd.env_remove("ILLOGICAL_PANE");
    if let Some(p) = pane {
        cmd.env("ILLOGICAL_PANE", p);
    }
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list"}}"#).unwrap();
    let (conn, _) = listener.accept().unwrap();
    let mut r = BufReader::new(conn.try_clone().unwrap());
    let mut head = String::new();
    let mut len = 0;
    loop {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        if line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().unwrap();
        }
        head.push_str(&line);
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).unwrap();
    let answer = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
    let mut conn = conn;
    write!(
        conn,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
        answer.len()
    )
    .unwrap();
    drop(stdin);
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    head.to_ascii_lowercase()
}

#[test]
fn the_bridge_says_which_pane_it_runs_in() {
    let head = headers_sent(Some("7"));
    assert!(head.contains("x-illogical-pane: 7\r\n"), "{head}");
    let head = headers_sent(None);
    assert!(!head.contains("x-illogical-pane"), "{head}");
    // Something that isn't a pane isn't passed on.
    let head = headers_sent(Some("seven"));
    assert!(!head.contains("x-illogical-pane"), "{head}");
}
