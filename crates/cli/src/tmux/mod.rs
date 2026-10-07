//! `arugula tmux -CC`: arugulad as a tmux server in control mode, for
//! iTerm2 and anything else that speaks it (M5; spike S11 has the protocol
//! as iTerm2 and real tmux 3.6 speak it).
//!
//! It runs where the daemon is (over ssh, typically) and talks to it like
//! any other client, over its socket or `--host`. Sessions are sessions,
//! tabs are windows, every block is a `%pane`; layouts are derived from the
//! daemon's split ratios, and a divider drag in iTerm2 becomes new ratios
//! that reproduce its cells exactly. The same layout shows live in the
//! browser.
//!
//! Installed or linked as `tmux`, the CLI behaves as `arugula tmux`.

mod commands;
mod format;
mod front;
mod layout;
mod parse;
mod vars;

use std::{
    io::{Read, Write},
    os::fd::AsFd,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use nix::{
    poll::{PollFd, PollFlags, PollTimeout, poll},
    sys::termios::{self, InputFlags, OutputFlags, SetArg},
};

use crate::http::Target;
use front::{Conn, Front, VERSION};

/// A command line or reply longer than this ends the session (as in
/// Ghostty's client).
const MAX_LINE: usize = 1 << 20;

/// Keeps stdin in control mode's terminal settings, restored on exit.
struct Raw(Option<termios::Termios>);

impl Raw {
    /// Like tmux for `-CC`: raw, except CR becomes LF on input and LF
    /// becomes CRLF on output.
    fn enter() -> Self {
        let stdin = std::io::stdin();
        let Ok(saved) = termios::tcgetattr(stdin.as_fd()) else { return Self(None) };
        let mut t = saved.clone();
        termios::cfmakeraw(&mut t);
        t.input_flags |= InputFlags::ICRNL;
        t.output_flags |= OutputFlags::OPOST | OutputFlags::ONLCR;
        let _ = termios::tcsetattr(stdin.as_fd(), SetArg::TCSANOW, &t);
        Self(Some(saved))
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        if let Some(t) = &self.0 {
            let _ = termios::tcsetattr(std::io::stdin().as_fd(), SetArg::TCSANOW, t);
        }
    }
}

/// `arugula tmux [flags] [command]`, with tmux's own flags.
pub fn run(target: Target, args: &[String]) -> anyhow::Result<i32> {
    let mut control = 0;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            i += 1;
            break;
        }
        if !a.starts_with('-') || a.len() < 2 {
            break;
        }
        let flags: Vec<char> = a[1..].chars().collect();
        for (j, f) in flags.iter().enumerate() {
            match f {
                'C' => control += 1,
                'V' => {
                    println!("tmux {VERSION}");
                    return Ok(0);
                }
                // These take a value: the rest of the word, or the next.
                'c' | 'f' | 'L' | 'S' | 'T' => {
                    if j + 1 == flags.len() {
                        i += 1;
                    }
                    break;
                }
                _ => {}
            }
        }
        i += 1;
    }
    if control == 0 {
        bail!("only tmux control mode is supported here: run `tmux -CC` (as iTerm2 does) or `arugula tmux -CC`");
    }
    let start = match args.get(i..).filter(|w| !w.is_empty()) {
        Some(words) => Some(parse::parse_cmd(words).map_err(anyhow::Error::msg)?),
        None => None,
    };
    if let Some(c) = &start
        && !matches!(c.name, "attach-session" | "new-session")
    {
        bail!("{} isn't supported as a starting command (use attach or new)", c.name);
    }
    let (conn, client, state) = Conn::open(&target).context("connecting to arugulad")?;
    let _raw = Raw::enter();
    let mut f = Front::new(conn, target, control > 1, client, state);
    begin(&mut f, start.as_ref())?;
    serve(&mut f)
}

/// The greeting: control mode's DCS, an empty block, then the session.
fn begin(f: &mut Front, start: Option<&parse::Cmd>) -> anyhow::Result<()> {
    if f.dcs {
        f.out.extend_from_slice(b"\x1bP1000p");
    }
    let (t, n) = (now(), next(f));
    f.out.extend_from_slice(format!("%begin {t} {n} 0\n%end {t} {n} 0\n").as_bytes());
    let new = start.is_some_and(|c| c.name == "new-session");
    if new || f.state.sessions.is_empty() {
        f.note(b"%sessions-changed\n");
        let cmd = match start {
            Some(c) if new => c.clone(),
            _ => parse::parse_cmd(&["new-session".to_owned()]).map_err(anyhow::Error::msg)?,
        };
        let before: Vec<_> = f.state.sessions.iter().map(|s| s.id).collect();
        f.exec(&cmd).map_err(anyhow::Error::msg)?;
        if f.session == 0 {
            // `-A` found an existing one, or `-d`: attach to it anyway.
            let s = f.state.sessions.iter().map(|s| s.id).find(|s| !before.contains(s));
            let s = s.or_else(|| f.state.sessions.first().map(|s| s.id)).context("no session")?;
            f.switch_session(s)?;
        }
    } else {
        let wanted = start.and_then(|c| c.get('t')).map(str::to_owned);
        let session = match wanted {
            Some(t) => {
                let found = f.state.sessions.iter().find(|s| {
                    s.name == t || format!("${}", s.id) == t || t.strip_prefix('$').is_none() && s.name.starts_with(&t)
                });
                found.map(|s| s.id).with_context(|| format!("can't find session: {t}"))?
            }
            None => f.state.sessions.first().map(|s| s.id).context("no session")?,
        };
        f.switch_session(session)?;
    }
    Ok(())
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn next(f: &mut Front) -> u64 {
    f.cmd_no += 1;
    f.cmd_no
}

/// `ARUGULA_TMUX_LOG=FILE`: every line both ways (`>` from the client,
/// `<` to it), to see what a client really sends.
struct Log(Option<std::fs::File>);

impl Log {
    fn open() -> Self {
        let path = std::env::var_os("ARUGULA_TMUX_LOG");
        Self(path.and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok()))
    }

    fn lines(&mut self, dir: char, data: &[u8]) {
        let Some(file) = &mut self.0 else { return };
        let text = String::from_utf8_lossy(data).replace('\x1b', "\\033");
        for line in text.split('\n').filter(|l| !l.is_empty()) {
            let _ = writeln!(file, "{dir} {line}");
        }
    }
}

/// Commands from stdin, notifications from the daemon, until detach.
fn serve(f: &mut Front) -> anyhow::Result<i32> {
    let mut log = Log::open();
    let mut stdout = std::io::stdout();
    let stdin = std::io::stdin();
    let mut pending: Vec<u8> = Vec::new();
    let mut after_cr = false;
    // Once `%exit` is out: waiting for the client's empty line (wait-exit).
    let mut leaving: Option<Instant> = None;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if let Some(reason) = f.exit.take()
            && leaving.is_none()
        {
            let line = if reason.is_empty() { "%exit\n".to_owned() } else { format!("%exit {reason}\n") };
            f.out.extend_from_slice(line.as_bytes());
            if !f.flags.wait_exit {
                return finish(f, &mut stdout, &mut log);
            }
            leaving = Some(Instant::now() + Duration::from_secs(5));
        }
        if !f.out.is_empty() {
            log.lines('<', &f.out);
            if stdout.write_all(&f.out).and_then(|()| stdout.flush()).is_err() {
                return Ok(0);
            }
            f.out.clear();
        }
        if leaving.is_some_and(|t| Instant::now() > t) {
            return finish(f, &mut stdout, &mut log);
        }
        f.conn.flush()?;
        let input = {
            let mut fds = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN), PollFd::new(f.conn_fd(), PollFlags::POLLIN)];
            poll(&mut fds, PollTimeout::from(100u16))?;
            fds[0].revents().is_some_and(|e| !e.is_empty())
        };
        // Every time round, not only when poll says so: the websocket can
        // hold messages it has already read.
        loop {
            match f.conn.read() {
                Ok(Some(m)) => f.on_daemon(m)?,
                Ok(None) => break,
                Err(_) => {
                    f.exit.get_or_insert_with(|| "server exited".into());
                    break;
                }
            }
        }
        if input {
            let n = stdin.lock().read(&mut buf)?;
            if n == 0 {
                return Ok(0);
            }
            for &b in &buf[..n] {
                if b == b'\n' && after_cr {
                    after_cr = false;
                    continue;
                }
                after_cr = b == b'\r';
                if b == b'\r' || b == b'\n' {
                    let line = std::mem::take(&mut pending);
                    log.lines('>', if line.is_empty() { b"(empty line)" } else { &line });
                    if line.is_empty() {
                        // An empty line detaches, or answers our `%exit`.
                        if leaving.is_some() {
                            return finish(f, &mut stdout, &mut log);
                        }
                        f.exit.get_or_insert_with(String::new);
                        break;
                    }
                    if leaving.is_none() {
                        command_line(f, &line);
                    }
                } else if pending.len() < MAX_LINE {
                    pending.push(b);
                }
            }
        }
        f.tick();
    }
}

/// The end of control mode: the DCS closes with ST.
fn finish(f: &mut Front, stdout: &mut std::io::Stdout, log: &mut Log) -> anyhow::Result<i32> {
    if f.dcs {
        f.out.extend_from_slice(b"\x1b\\");
    }
    log.lines('<', &f.out);
    let _ = stdout.write_all(&f.out);
    let _ = stdout.flush();
    Ok(0)
}

/// One line from the client: commands separated by `;`, each answered with
/// its own `%begin`/`%end` (or `%error`, which skips the rest, as in tmux).
fn command_line(f: &mut Front, line: &[u8]) {
    // iTerm2 sends ^C first, in case a shell rather than tmux is listening.
    let start = line.iter().position(|b| *b != 0x03).unwrap_or(line.len());
    let text = String::from_utf8_lossy(&line[start..]).into_owned();
    let parsed = parse::split_line(&text)
        .and_then(|cmds| cmds.iter().map(|w| parse::parse_cmd(w)).collect::<Result<Vec<_>, _>>())
        .map_err(|e| if e.starts_with("syntax") { e } else { format!("parse error: {e}") });
    let cmds = match parsed {
        Ok(c) => c,
        Err(e) => {
            let (t, n) = (now(), next(f));
            f.out.extend_from_slice(format!("%begin {t} {n} 1\n{e}\n%error {t} {n} 1\n").as_bytes());
            return;
        }
    };
    f.busy = true;
    for c in &cmds {
        let (t, n) = (now(), next(f));
        f.out.extend_from_slice(format!("%begin {t} {n} 1\n").as_bytes());
        match f.exec(c) {
            Ok(body) => {
                f.out.extend(body);
                f.out.extend_from_slice(format!("%end {t} {n} 1\n").as_bytes());
            }
            Err(e) => {
                f.out.extend_from_slice(format!("{e}\n%error {t} {n} 1\n").as_bytes());
                break;
            }
        }
    }
    f.settle();
}
