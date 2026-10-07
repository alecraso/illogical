//! #234: `arugula mcp` in a pane says which (`$ARUGULA_PANE`), so a
//! tool like `invite_person` knows where Claude Code in a terminal works;
//! outside a pane it says nothing.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixListener,
    process::{Command, Stdio},
};

/// The headers of the one request `arugula mcp` sends for a line, with
/// `ARUGULA_PANE` as given.
fn headers_sent(pane: Option<&str>) -> String {
    headers_sent_with("ARUGULA_PANE", pane, &[])
}

/// The same with the pane in `var`, and more of the environment.
fn headers_sent_with(var: &str, pane: Option<&str>, env: &[(&str, &str)]) -> String {
    let mut env = env.to_vec();
    if let Some(p) = pane {
        env.push((var, p));
    }
    let line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let answer = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
    first_request(&format!("mcp-{var}-{}", pane.unwrap_or("none")), &["mcp"], &env, Some(line), answer)
}

/// The head (lowercase) of the first request `arugula ARGS` sends to a
/// socket of ours, which answers it with `answer`. Neither name of the
/// pane variable, nor `CLAUDE_CONFIG_DIR`, unless `env` says.
fn first_request(name: &str, args: &[&str], env: &[(&str, &str)], stdin: Option<&str>, answer: &str) -> String {
    let dir = std::env::temp_dir().join(format!("ilg-cli-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("sock");
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_arugula"));
    cmd.arg("--socket").arg(&sock).args(args).stdin(Stdio::piped()).stdout(Stdio::piped());
    for k in ["ARUGULA_PANE", "ILLOGICAL_PANE", "CLAUDE_CONFIG_DIR", "CLAUDECODE", "AI_AGENT"] {
        cmd.env_remove(k);
    }
    cmd.envs(env.iter().copied());
    let mut child = cmd.spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    if let Some(l) = stdin {
        writeln!(input, "{l}").unwrap();
    }
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
    let mut conn = conn;
    write!(
        conn,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
        answer.len()
    )
    .unwrap();
    drop(conn);
    drop(input);
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    head.to_ascii_lowercase()
}

#[test]
fn the_bridge_says_which_pane_it_runs_in() {
    let head = headers_sent(Some("7"));
    assert!(head.contains("x-arugula-pane: 7\r\n"), "{head}");
    let head = headers_sent(None);
    assert!(!head.contains("x-arugula-pane"), "{head}");
    // Something that isn't a pane isn't passed on.
    let head = headers_sent(Some("seven"));
    assert!(!head.contains("x-arugula-pane"), "{head}");
}

/// #505: the pane and the Claude Code config directory go under both
/// names, for a daemon older than 0.25; a pane an illogical daemon started
/// says which under the old name.
#[test]
fn the_bridge_says_it_under_both_names() {
    let head = headers_sent_with("ARUGULA_PANE", Some("7"), &[("CLAUDE_CONFIG_DIR", "/c")]);
    for h in ["x-arugula-pane: 7\r\n", "x-illogical-pane: 7\r\n"] {
        assert!(head.contains(h), "{head}");
    }
    for h in ["arugula-claude-config-dir: /c\r\n", "illogical-claude-config-dir: /c\r\n"] {
        assert!(head.contains(h), "{head}");
    }
    let head = headers_sent_with("ILLOGICAL_PANE", Some("8"), &[]);
    assert!(head.contains("x-arugula-pane: 8\r\n"), "{head}");
}

/// #505: the CLI under Claude Code says an agent runs it under both names,
/// so a daemon older than 0.25 doesn't take it for the owner.
#[test]
fn an_agent_says_so_under_both_names() {
    let head = first_request("agent", &["attention", "--json"], &[("CLAUDECODE", "1")], None, "[]");
    for h in ["x-arugula-agent: 1\r\n", "x-illogical-agent: 1\r\n"] {
        assert!(head.contains(h), "{head}");
    }
    let head = first_request("owner", &["attention", "--json"], &[], None, "[]");
    assert!(!head.contains("agent"), "{head}");
}
