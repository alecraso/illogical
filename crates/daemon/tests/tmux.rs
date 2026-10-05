//! M5: `illogical tmux -CC` against a real daemon, driven the way iTerm2
//! drives tmux (the command sequence spike S11 took from iTerm2's source and
//! replayed against tmux 3.6), with every reply compared to what tmux 3.6
//! answered (`fixtures/s11-iterm2-vs-tmux-3.6.txt`, S11's transcript): the
//! same success or error, and the same body once ids, checksums and the
//! known differences are set aside.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use illogical_testkit::{Daemon, illogicald};
use regex::Regex;

const COLS: u16 = 120;
const ROWS: u16 = 40;
/// iTerm2's default: MAX(client rows, scrollback lines).
const MAX_HISTORY: u32 = 1000;

/// TmuxStateParser's keys, in order.
const STATE_KEYS: &[&str] = &[
    "pane_id",
    "alternate_on",
    "alternate_saved_x",
    "alternate_saved_y",
    "cursor_x",
    "cursor_y",
    "scroll_region_upper",
    "scroll_region_lower",
    "pane_tabs",
    "cursor_flag",
    "insert_flag",
    "keypad_cursor_flag",
    "keypad_flag",
    "wrap_flag",
    "mouse_standard_flag",
    "mouse_button_flag",
    "mouse_any_flag",
    "mouse_utf8_flag",
    "mouse_sgr_flag",
    "bracket_paste_flag",
    "pane_key_mode",
];

fn state_fmt() -> String {
    STATE_KEYS.iter().map(|k| format!("{k}=#{{{k}}}")).collect::<Vec<_>>().join("\t")
}

/// TmuxController's detailed list-windows format.
fn detailed() -> String {
    let f = [
        "#{session_name}",
        "#{window_id}",
        "#{window_name}",
        "#{window_width}",
        "#{window_height}",
        "#{window_layout}",
        "#{window_flags}",
        "#{?window_active,1,0}",
        "#{window_visible_layout}",
        "#{pane-border-status}",
    ];
    format!("\"{}\"", f.join("\t"))
}

const LIST_WINDOWS: &str =
    "list-windows -F \"#{window_id} #{window_layout} #{window_flags} #{window_visible_layout} #{pane-border-status}\"";

fn start() -> Daemon {
    illogicald!("tmux").env("PS1", "$ ").wait_secs(10).start()
}

/// The CLI, built next to the daemon (cargo builds only this package's
/// binaries for its tests).
fn cli_bin() -> PathBuf {
    let bin = Path::new(env!("CARGO_BIN_EXE_illogicald")).with_file_name("illogical");
    let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "illogical"]).status().unwrap();
    assert!(status.success(), "building the CLI");
    bin
}

/// A reply: the command, whether it succeeded, its body.
type Reply = (String, bool, Vec<String>);

/// A control-mode client playing iTerm2, like S11's replay.py.
struct Cc {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<String>,
    /// Every line from the server, in order (ESC shown as `\033`).
    lines: Vec<String>,
    queue: VecDeque<String>,
    current: Option<(String, Vec<String>)>,
    done: Vec<Reply>,
    /// Lines outside replies.
    notes: Vec<String>,
    exited: bool,
    /// Lets a client that starts out not reading (`start_stalled`) read.
    go: Option<mpsc::Sender<()>>,
}

impl Cc {
    fn start(daemon: &Daemon, args: &[&str]) -> Self {
        Self::start_with(daemon, args, false)
    }

    /// A client that doesn't read its output until `resume`, so the front
    /// end blocks writing to it and falls behind the daemon.
    fn start_stalled(daemon: &Daemon, args: &[&str]) -> Self {
        Self::start_with(daemon, args, true)
    }

    fn resume(&mut self) {
        if let Some(go) = self.go.take() {
            let _ = go.send(());
        }
    }

    fn start_with(daemon: &Daemon, args: &[&str], stalled: bool) -> Self {
        let mut child = Command::new(cli_bin())
            .arg("--socket")
            .arg(daemon.sock())
            .arg("tmux")
            .args(args)
            .env("SHELL", "/bin/bash")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let (go, wait) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            if stalled {
                let _ = wait.recv();
            }
            let mut buf = vec![0u8; 65536];
            let mut pending = Vec::new();
            loop {
                let n = match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                pending.extend_from_slice(&buf[..n]);
                while let Some(i) = pending.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=i).collect();
                    let line = String::from_utf8_lossy(&line[..line.len() - 1]).trim_end_matches('\r').to_owned();
                    if tx.send(line).is_err() {
                        return;
                    }
                }
                if pending == b"\x1b\\" {
                    let _ = tx.send("\x1b\\".into());
                    pending.clear();
                }
            }
            if !pending.is_empty() {
                let _ = tx.send(String::from_utf8_lossy(&pending).into_owned());
            }
        });
        Cc {
            child,
            stdin,
            rx,
            lines: vec![],
            queue: VecDeque::new(),
            current: None,
            done: vec![],
            notes: vec![],
            exited: false,
            go: stalled.then_some(go),
        }
    }

    /// One line; several commands are joined with `; ` as iTerm2 does.
    fn send(&mut self, cmds: &[&str]) {
        self.queue.extend(cmds.iter().map(|c| c.to_string()));
        let line = format!("{}\r", cmds.join("; "));
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.stdin.write_all(bytes).unwrap();
        self.stdin.flush().unwrap();
    }

    /// Read lines for `timeout` (no longer, even while lines keep coming:
    /// callers' deadlines hold under a flood).
    fn pump(&mut self, timeout: Duration) {
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            match self.rx.recv_timeout(left) {
                Ok(l) => self.on_line(l),
                Err(mpsc::RecvTimeoutError::Timeout) => return,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.exited = true;
                    return;
                }
            }
        }
    }

    fn on_line(&mut self, mut line: String) {
        if let Some(rest) = line.strip_prefix("\x1bP1000p") {
            self.lines.push("DCS".into());
            line = rest.to_owned();
        }
        if line == "\x1b\\" {
            self.lines.push("ST".into());
            self.exited = true;
            return;
        }
        let shown = line.replace('\x1b', "\\033").replace('\t', "\\t");
        self.lines.push(shown.clone());
        // Once, not per line: a flood is hundreds of thousands of lines.
        static GUARD: std::sync::LazyLock<Regex> =
            std::sync::LazyLock::new(|| Regex::new(r"^%(begin|end|error) (\d+) (\d+) (\d+)$").unwrap());
        if let Some(m) = GUARD.captures(&line) {
            let flags: u32 = m[4].parse().unwrap();
            if &m[1] == "begin" {
                let cmd = if flags & 1 == 1 { self.queue.pop_front().unwrap_or_default() } else { "(server)".into() };
                self.current = Some((cmd, vec![]));
            } else {
                let (cmd, body) = self.current.take().expect("%end without %begin");
                self.done.push((cmd, &m[1] == "end", body));
            }
            return;
        }
        if let Some((_, body)) = &mut self.current {
            body.push(shown);
            return;
        }
        if line.starts_with("%exit") {
            self.exited = true;
        }
        self.notes.push(shown);
    }

    /// Until every command is answered and the line goes quiet.
    fn wait_idle(&mut self) {
        // A deadline for a failure, not a wait: generous for a loaded
        // machine.
        let end = Instant::now() + Duration::from_secs(60);
        while Instant::now() < end {
            let n = self.lines.len();
            self.pump(Duration::from_millis(300));
            if self.queue.is_empty() && self.current.is_none() && self.lines.len() == n {
                return;
            }
            if self.exited {
                return;
            }
        }
        panic!("no answer: waiting for {:?}", self.queue);
    }

    fn answer(&self, prefix: &str) -> (bool, Vec<String>) {
        let r = self.done.iter().rev().find(|(c, _, _)| c.starts_with(prefix)).unwrap_or_else(|| panic!("no {prefix}"));
        (r.1, r.2.clone())
    }

    fn wait_note(&mut self, prefix: &str) -> String {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(n) = self.notes.iter().find(|n| n.starts_with(prefix)) {
                return n.clone();
            }
            assert!(Instant::now() < end, "no {prefix} in {:#?}", self.notes);
            self.pump(Duration::from_millis(100));
        }
    }

    fn close(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Pane ids in a layout string, depth first (TmuxLayoutParser).
fn panes_in_layout(layout: &str) -> Vec<u32> {
    Regex::new(r"\d+x\d+,\d+,\d+,(\d+)").unwrap().captures_iter(layout).map(|c| c[1].parse().unwrap()).collect()
}

/// TmuxWindowOpener's requests for one pane (pause mode is on).
fn pane_requests(p: u32) -> Vec<String> {
    vec![
        format!("capture-pane -peqJN -t \"%{p}\" -S -{MAX_HISTORY}"),
        format!("capture-pane -peqJN -a -t \"%{p}\" -S -{MAX_HISTORY}"),
        format!("list-panes -t \"%{p}\" -F \"{}\"", state_fmt()),
        format!("capture-pane -p -P -C -t \"%{p}\""),
        format!("refresh-client -A '%{p}:continue'"),
        format!("show-options -v -q -p -t %{p} @uservars"),
    ]
}

fn send_all(c: &mut Cc, cmds: &[String]) {
    c.send(&cmds.iter().map(String::as_str).collect::<Vec<_>>());
}

/// iTerm2's attach: PTYSession -startTmuxMode through TmuxWindowOpener.
/// Returns the session id and the windows (id, layout).
fn attach_like_iterm2(c: &mut Cc) -> (u32, Vec<(u32, String)>) {
    let note = c.wait_note("%session-changed");
    let sid: u32 = Regex::new(r"%session-changed \$(\d+)").unwrap().captures(&note).unwrap()[1].parse().unwrap();
    c.wait_idle();
    c.raw(b"\x03");
    for cmd in [
        "phony-command",
        "refresh-client -fpause-after=0,wait-exit",
        "show-window-options -g aggressive-resize",
        "show-option -g -v status",
        "list-sessions -F \"\t\"",
        "show-options -v -s default-terminal",
        "list-keys",
        "copy-mode -q",
        "display-message -p \"#{version}\"",
        "show-window-options pane-border-format",
        "list-windows -F \"#{socket_path}\"",
        "list-windows -F \"#{pid}\"",
        "show-options -g message-style",
    ] {
        c.send(&[cmd]);
    }
    c.wait_idle();
    for cmd in [
        "refresh-client -fpause-after=120",
        "display-message -p \"#{socket_path},#{pid}\"",
        "display-message -p '#{client_name}'",
        "show-options -v -g set-titles",
    ] {
        c.send(&[cmd]);
    }
    c.wait_idle();
    c.send(&[&format!("list-clients -t '${sid}' -F '#{{client_name}}\t#{{client_control_mode}}'")]);
    c.send(&["refresh-client -B 'it2_1::#{T:set-clipboard}'"]);
    c.send(&["display-message -t '' -p '#{T:set-clipboard}'"]);
    c.send(&[&format!("show -v -q -t ${sid} @iterm2_size")]);
    c.wait_idle();
    let mut list: Vec<String> =
        vec![format!("show -v -q -t ${sid} @iterm2_id"), format!("refresh-client -C {COLS},{ROWS}")];
    for o in [
        "hidden",
        "buried_indexes",
        "affinities",
        "per_window_settings",
        "per_tab_settings",
        "origins",
        "hotkeys",
        "tab_colors",
    ] {
        list.push(format!("show -v -q -t ${sid} @{o}"));
    }
    list.push("list-sessions -F \"#{session_id} #{session_name}\"".into());
    list.push(format!("list-windows -F {}", detailed()));
    send_all(c, &list);
    c.wait_idle();
    let (_, id) = c.answer(&format!("show -v -q -t ${sid} @iterm2_id"));
    if id.is_empty() || id[0].is_empty() {
        c.send(&[&format!("set -t ${sid} @iterm2_id \"84FDDA15-6C28-4681-B61C-309C06C9351D\"")]);
    }
    let (ok, body) = c.answer("list-windows -F \"#{session_name}");
    assert!(ok);
    let windows: Vec<(u32, String)> = body
        .iter()
        .map(|row| {
            let f: Vec<&str> = row.split("\\t").collect();
            (f[1][1..].parse().unwrap(), f[5].to_owned())
        })
        .collect();
    for (_, layout) in &windows {
        let reqs: Vec<String> = panes_in_layout(layout).into_iter().flat_map(pane_requests).collect();
        send_all(c, &reqs);
    }
    c.wait_idle();
    let sizes: Vec<String> = windows.iter().map(|(w, _)| format!("refresh-client -C @{w}:{COLS}x{ROWS}")).collect();
    send_all(c, &sizes);
    c.wait_idle();
    (sid, windows)
}

/// TmuxGateway -sendCodePoints, one list per keystroke.
fn type_keys(c: &mut Cc, p: u32, text: &str) {
    for ch in text.chars() {
        let cmd = if ch.is_ascii_alphanumeric() || "+/):,_".contains(ch) {
            format!("send -lt %{p} {ch}")
        } else if (ch as u32) < 0x20 {
            format!("send -H -t %{p} {:02x}", ch as u32)
        } else {
            format!("send -t %{p} 0x{:x}", ch as u32)
        };
        c.send(&[&cmd]);
        c.pump(Duration::from_millis(30));
    }
}

/// A paste: run-length grouped into one list.
fn paste(c: &mut Cc, p: u32, text: &str) {
    let kind = |ch: char| {
        if ch.is_ascii_alphanumeric() || "+/):,_".contains(ch) {
            'l'
        } else if (ch as u32) < 0x20 {
            'H'
        } else {
            'x'
        }
    };
    let mut groups: Vec<(char, String)> = vec![];
    for ch in text.chars() {
        match groups.last_mut() {
            Some((k, s)) if *k == kind(ch) => s.push(ch),
            _ => groups.push((kind(ch), ch.to_string())),
        }
    }
    let cmds: Vec<String> = groups
        .into_iter()
        .map(|(k, s)| match k {
            'l' => format!("send -lt %{p} {s}"),
            'H' => format!(
                "send -H -t %{p} {}",
                s.chars().map(|c| format!("{:02x}", c as u32)).collect::<Vec<_>>().join(" ")
            ),
            _ => {
                format!("send -t %{p} {}", s.chars().map(|c| format!("0x{:x}", c as u32)).collect::<Vec<_>>().join(" "))
            }
        })
        .collect();
    send_all(c, &cmds);
}

/// Capture the panes a `%layout-change` shows that iTerm2 hasn't seen.
fn on_layout_change(c: &mut Cc, known: &mut Vec<u32>) {
    let re = Regex::new(r"^%layout-change @\d+ (\S+)").unwrap();
    let mut new = vec![];
    for n in c.notes.clone() {
        if let Some(m) = re.captures(&n) {
            for p in panes_in_layout(&m[1]) {
                if !known.contains(&p) {
                    known.push(p);
                    new.push(p);
                }
            }
        }
    }
    let reqs: Vec<String> = new.into_iter().flat_map(pane_requests).collect();
    if !reqs.is_empty() {
        send_all(c, &reqs);
        c.wait_idle();
    }
}

// ---- comparing with tmux

/// `layout_checksum()`.
fn checksum(body: &str) -> String {
    let mut csum: u16 = 0;
    for b in body.bytes() {
        csum = (csum >> 1) + ((csum & 1) << 15);
        csum = csum.wrapping_add(b as u16);
    }
    format!("{csum:04x}")
}

/// A layout with its panes numbered 0, 1, ... in reading order and the
/// checksum redone, so ours and tmux's compare by shape and size.
fn canonical_layout(l: &str) -> String {
    let body = &l[5..];
    let mut n = 0;
    let body = Regex::new(r"(\d+x\d+,\d+,\d+),(\d+)")
        .unwrap()
        .replace_all(body, |c: &regex::Captures| {
            n += 1;
            format!("{},{}", &c[1], n - 1)
        })
        .into_owned();
    format!("{},{body}", checksum(&body))
}

/// A line with layouts made canonical and ids, UUIDs and times blanked.
fn normalize(line: &str) -> String {
    let layouts = Regex::new(r"[0-9a-f]{4},\d+x\d+,\d+,\d+[^\s\\]*").unwrap();
    let line = layouts.replace_all(line, |c: &regex::Captures| canonical_layout(&c[0])).into_owned();
    let line = Regex::new(r"([%@$])\d+").unwrap().replace_all(&line, "$1#").into_owned();
    Regex::new(r"[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{12}")
        .unwrap()
        .replace_all(&line, "UUID")
        .into_owned()
}

/// A captured line without escapes or trailing blanks (tmux writes only
/// SGR changes, we write whole SGRs; trailing spaces depend on how a
/// program cleared the line).
fn plain(line: &str) -> String {
    let s = Regex::new(r"\\033\[[0-9;:?]*[A-Za-z]").unwrap().replace_all(line, "");
    s.trim_end().to_owned()
}

/// tmux's replies from the transcript, by normalized command, in order.
fn tmux_replies() -> HashMap<String, VecDeque<(bool, Vec<String>)>> {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/s11-iterm2-vs-tmux-3.6.txt"),
    )
    .unwrap();
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut current: Option<(String, Vec<String>)> = None;
    let mut out: HashMap<String, VecDeque<(bool, Vec<String>)>> = HashMap::new();
    let re = Regex::new(r"^%(begin|end|error) (\d+) (\d+) (\d+)$").unwrap();
    for line in text.lines() {
        if let Some(cmds) = line.strip_prefix("> ") {
            if cmds != "^C" && !cmds.is_empty() {
                queue.extend(cmds.split("; ").map(str::to_owned));
            }
            continue;
        }
        let Some(l) = line.strip_prefix("< ") else { continue };
        if l.starts_with("\\033P1000p") || l.starts_with("\\033\\") {
            continue;
        }
        if let Some(m) = re.captures(l) {
            let flags: u32 = m[4].parse().unwrap();
            if &m[1] == "begin" {
                let cmd = if flags & 1 == 1 { queue.pop_front().unwrap() } else { "(server)".into() };
                current = Some((cmd, vec![]));
            } else {
                let (cmd, body) = current.take().unwrap();
                out.entry(normalize(&cmd)).or_default().push_back((&m[1] == "end", body));
            }
            continue;
        }
        if let Some((_, body)) = &mut current {
            body.push(l.to_owned());
        }
    }
    out
}

/// Replies that differ from tmux's on purpose, with why.
fn known_difference(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        c if c.starts_with("list-keys") => "no key bindings to report",
        c if c.contains("#{version}") => "we report 3.5a",
        c if c.starts_with("show-option -g -v status") => "status is off: there is no status line",
        c if c.contains("default-terminal") => "panes run with xterm-256color",
        c if c.contains("#{socket_path}") || c.contains("#{pid}") || c.contains("client_name") => "per process",
        c if c.starts_with("phony-command") => "tmux keeps the ^C in the message",
        // tmux writes these inside the reply; we hold them until after its
        // %end, as HTM does (checked as notifications above).
        c if c.starts_with("refresh-client -A") => "%pause and %continue come after %end",
        _ => return None,
    })
}

/// Compare one of our replies with tmux's.
fn compare(cmd: &str, ours: &(bool, Vec<String>), theirs: &(bool, Vec<String>)) -> Result<(), String> {
    if ours.0 != theirs.0 {
        return Err(format!("{cmd}: ok={} but tmux ok={}\n ours {:?}\n tmux {:?}", ours.0, theirs.0, ours.1, theirs.1));
    }
    if let Some(why) = known_difference(cmd) {
        let _ = why;
        return Ok(());
    }
    let norm = |b: &[String]| -> Vec<String> {
        let mut v: Vec<String> = b
            .iter()
            .map(|l| if cmd.starts_with("capture-pane") { plain(l) } else { normalize(l) })
            .map(|l| {
                // The window's name follows its active pane, which is per
                // client here and shared in tmux.
                if cmd.contains("#{window_name}") {
                    let mut f: Vec<&str> = l.split("\\t").collect();
                    if f.len() > 2 {
                        f[2] = "NAME";
                    }
                    f.join("\\t")
                } else {
                    l
                }
            })
            .map(|l| {
                // Ghostty keeps the tab stops it had before a resize; tmux
                // resets them. Ours run out sooner, never differ.
                Regex::new(r"pane_tabs=[0-9,]*").unwrap().replace(&l, "pane_tabs=").into_owned()
            })
            .collect();
        // tmux keeps the saved (primary) screen at its old size while the
        // alternate one is up; Ghostty resizes both.
        if cmd.contains(" -a ") {
            while v.last().is_some_and(String::is_empty) {
                v.pop();
            }
        }
        v
    };
    let tabs = |b: &[String]| -> Vec<String> {
        let re = Regex::new(r"pane_tabs=([0-9,]*)").unwrap();
        b.iter().filter_map(|l| re.captures(l).map(|c| c[1].to_owned())).collect()
    };
    for (o, t) in tabs(&ours.1).iter().zip(tabs(&theirs.1)) {
        if !t.starts_with(o.as_str()) {
            return Err(format!("{cmd}: tab stops {o} vs tmux {t}"));
        }
    }
    let (a, b) = (norm(&ours.1), norm(&theirs.1));
    if a != b {
        return Err(format!("{cmd}:\n ours {a:#?}\n tmux {b:#?}"));
    }
    Ok(())
}

#[test]
fn iterm2s_conversation_gets_tmuxs_answers() {
    let daemon = start();
    // The daemon starts with a session; tmux's transcript starts with none.
    {
        let mut c = Cc::start(&daemon, &["-CC"]);
        let note = c.wait_note("%session-changed");
        let first = note.split_whitespace().nth(1).unwrap().to_owned();
        c.send(&[&format!("kill-session -t {first}")]);
        c.wait_idle();
        c.close();
    }

    // ---- attach 1: tmux -CC new -s s11
    let mut c = Cc::start(&daemon, &["-CC", "new", "-s", "s11"]);
    let (sid, windows) = attach_like_iterm2(&mut c);
    assert_eq!(&c.lines[..3], &["DCS".to_owned(), c.lines[1].clone(), c.lines[2].clone()]);
    assert!(c.lines[1].starts_with("%begin ") && c.lines[1].ends_with(" 0"), "{:?}", c.lines);
    assert!(c.lines[2].starts_with("%end ") && c.lines[2].ends_with(" 0"));
    assert!(c.notes.contains(&"%sessions-changed".to_owned()));
    assert!(c.notes.contains(&format!("%session-changed ${sid} s11")));
    let mut known: Vec<u32> = windows.iter().flat_map(|(_, l)| panes_in_layout(l)).collect();
    let (wid, p0) = (windows[0].0, known[0]);
    c.notes.clear();

    // ---- split vertically (iTerm2 "vertical" is tmux -h)
    c.send(&[&format!("list-panes -t %{p0} -F '#{{pane_id}}'")]);
    c.send(&[&format!("split-window -h -t \"%{p0}\"")]);
    c.send(&[&format!("list-panes -t %{p0} -F '#{{pane_id}}'")]);
    c.wait_idle();
    let changed = c.notes.iter().position(|n| n.starts_with("%window-pane-changed")).expect("%window-pane-changed");
    let layout = c.notes.iter().position(|n| n.starts_with("%layout-change")).expect("%layout-change");
    assert!(changed < layout, "tmux sends %window-pane-changed first: {:#?}", c.notes);
    let split = c.notes[layout].split_whitespace().nth(2).unwrap().to_owned();
    assert_eq!(canonical_layout(&split), canonical_layout("f91d,120x40,0,0{60x40,0,0,0,59x40,61,0,1}"));
    on_layout_change(&mut c, &mut known);
    let p1 = *known.last().unwrap();
    c.notes.clear();

    // ---- drag the divider 5 cells right
    c.send(&[&format!("resize-pane -R -t \"%{p0}\" 5"), LIST_WINDOWS]);
    c.wait_idle();
    let (_, body) = c.answer("list-windows -F \"#{window_id}");
    let dragged = body[0].split_whitespace().nth(1).unwrap();
    assert_eq!(canonical_layout(dragged), canonical_layout("2a7e,120x40,0,0{65x40,0,0,0,54x40,66,0,1}"));

    // ---- the iTerm2 window gets smaller
    c.notes.clear();
    c.send(&[&format!("refresh-client -C @{wid}:100x30")]);
    c.wait_idle();
    let shrunk = c.wait_note(&format!("%layout-change @{wid} "));
    // tmux takes 10 from each side (55|44); illogical keeps the ratio (54|45).
    let shrunk = canonical_layout(shrunk.split_whitespace().nth(2).unwrap());
    assert!(shrunk.ends_with(",100x30,0,0{54x30,0,0,0,45x30,55,0,1}"), "{shrunk}");

    // ---- typing, key by key
    type_keys(&mut c, p1, "echo hi there\r");
    c.wait_idle();
    // Output comes in however many notes the pane's reads made: join their
    // payloads, not the notes, or a line split across two never matches.
    let typed = output(&c.notes, p1);
    assert!(typed.contains("hi there\\015\\012"), "{typed}");

    // ---- pause and continue by hand
    c.notes.clear();
    c.send(&[&format!("refresh-client -A '%{p1}:pause'")]);
    c.wait_idle();
    assert!(c.notes.contains(&format!("%pause %{p1}")), "after the %end: {:#?}", c.notes);
    paste(&mut c, p1, "seq 1 3\r");
    c.wait_idle();
    assert!(!c.notes.iter().any(|n| n.starts_with(&format!("%extended-output %{p1} "))), "output while paused");
    send_all(&mut c, &pane_requests(p1));
    c.wait_idle();
    assert!(c.notes.contains(&format!("%continue %{p1}")));
    let (_, history) = c
        .done
        .iter()
        .rev()
        .find(|r| r.0.starts_with(&format!("capture-pane -peqJN -t \"%{p1}\"")))
        .map(|r| (r.1, r.2.clone()))
        .unwrap();
    assert_eq!(
        history.iter().map(|l| plain(l)).take(7).collect::<Vec<_>>(),
        ["$ echo hi there", "hi there", "$ seq 1 3", "1", "2", "3", "$"]
    );

    // ---- vi, typed into, left running
    // A run that died left vi's swap file, and every vi after it opened on
    // its question: none of those (the fixture has this file's name).
    for f in std::fs::read_dir("/tmp").unwrap().flatten() {
        if f.file_name().to_string_lossy().starts_with(".s11-vim.txt.sw") {
            let _ = std::fs::remove_file(f.path());
        }
    }
    paste(&mut c, p0, "vi -u NONE -N /tmp/s11-vim.txt\r");
    c.pump(Duration::from_secs(1));
    c.wait_idle();
    type_keys(&mut c, p0, "ihello from s11\x1b");
    c.wait_idle();

    // ---- new tab, then close it
    c.notes.clear();
    c.send(&[&format!("new-window -PF '#{{window_id}}' -a -t \"${sid}:+\"")]);
    c.wait_idle();
    let (ok, body) = c.answer("new-window");
    assert!(ok);
    let w2: u32 = body[0][1..].parse().unwrap();
    let swc =
        c.notes.iter().position(|n| *n == format!("%session-window-changed ${sid} @{w2}")).expect("window changed");
    let add = c.notes.iter().position(|n| *n == format!("%window-add @{w2}")).expect("%window-add");
    assert!(swc < add, "{:#?}", c.notes);
    c.send(&[&format!("display -p -F {} -t @{w2}", detailed())]);
    c.wait_idle();
    let (_, body) = c.answer("display -p -F");
    let layout = body[0].split("\\t").nth(5).unwrap().to_owned();
    let reqs: Vec<String> = panes_in_layout(&layout).into_iter().flat_map(pane_requests).collect();
    send_all(&mut c, &reqs);
    c.send(&[&format!("refresh-client -C @{w2}:{COLS}x{ROWS}")]);
    c.wait_idle();
    c.notes.clear();
    c.send(&[&format!("kill-window -t @{w2}")]);
    c.wait_idle();
    // tmux says %unlinked-window-close for the current window; the plan
    // (and every client) takes %window-close.
    assert!(c.notes.contains(&format!("%session-window-changed ${sid} @{wid}")), "{:#?}", c.notes);
    assert!(c.notes.contains(&format!("%window-close @{w2}")), "{:#?}", c.notes);

    // ---- detach: %exit, an empty line (wait-exit), then ST
    c.send(&["detach"]);
    c.pump(Duration::from_millis(500));
    assert!(c.notes.iter().any(|n| n == "%exit"), "{:#?}", c.notes);
    assert!(!c.lines.contains(&"ST".to_owned()), "ST waits for the empty line");
    c.raw(b"\r");
    c.pump(Duration::from_millis(500));
    assert_eq!(c.lines.last().map(String::as_str), Some("ST"));
    let first = std::mem::take(&mut c.done);
    c.close();

    // ---- attach 2: vi is still there, on the alternate screen
    let mut c = Cc::start(&daemon, &["-CC", "attach", "-t", "s11"]);
    attach_like_iterm2(&mut c);
    assert_eq!(c.lines[0], "DCS");
    assert!(!c.notes.iter().any(|n| n.starts_with("%window-add")), "reattach is lean: {:#?}", c.notes);
    let state =
        c.done.iter().find(|r| r.0.starts_with(&format!("list-panes -t \"%{p0}\" -F \"pane_id"))).unwrap().2.clone();
    let mine = state.iter().find(|l| l.starts_with(&format!("pane_id=%{p0}\\t"))).unwrap();
    assert!(
        mine.contains("\\talternate_on=1\\talternate_saved_x=0\\talternate_saved_y=1\\tcursor_x=13\\tcursor_y=0\\t"),
        "{mine}"
    );
    paste(&mut c, p0, "\x1b:q!\r");
    c.wait_idle();
    c.pump(Duration::from_millis(500));
    let out = output(&c.notes, p0);
    assert!(out.contains("\\033[?1049l"), "vi left the alternate screen: {out}");
    paste(&mut c, p0, "exit\r");
    c.pump(Duration::from_secs(1));
    c.wait_idle();
    let last = c.notes.iter().rev().find(|n| n.starts_with(&format!("%layout-change @{wid} "))).expect("layout change");
    assert_eq!(panes_in_layout(last.split_whitespace().nth(2).unwrap()), vec![p1], "%{p1} takes the window");
    c.send(&["detach"]);
    c.pump(Duration::from_millis(300));
    c.raw(b"\r");
    c.pump(Duration::from_millis(300));
    let second = std::mem::take(&mut c.done);
    c.close();

    // ---- every reply, against tmux's
    let mut tmux = tmux_replies();
    let mut failures = vec![];
    let mut compared = 0;
    for (cmd, ok, body) in first.into_iter().chain(second) {
        let key = normalize(&cmd);
        let Some(theirs) = tmux.get_mut(&key).and_then(|q| q.pop_front()) else {
            failures.push(format!("tmux never answered {key}"));
            continue;
        };
        compared += 1;
        // The fixture's vi screen is Debian's vim (ruler on by default);
        // other systems' vim draws it differently.
        if !cfg!(any(target_os = "linux", target_os = "android")) && key.starts_with("capture-pane") {
            continue;
        }
        if let Err(e) = compare(&cmd, &(ok, body), &theirs) {
            failures.push(e);
        }
    }
    assert!(compared > 150, "compared only {compared}");
    assert!(failures.is_empty(), "{} differences:\n{}", failures.len(), failures.join("\n\n"));
}

/// Attached to the daemon's first session at 120x40: the client, its
/// session, window and pane.
fn attached(daemon: &Daemon, flags: &[&str]) -> (Cc, u32, u32, u32) {
    let mut c = Cc::start(daemon, flags);
    let note = c.wait_note("%session-changed");
    let sid: u32 = note.split_whitespace().nth(1).unwrap()[1..].parse().unwrap();
    c.wait_idle();
    c.send(&[&format!("refresh-client -C {COLS},{ROWS}"), "list-panes -F '#{window_id} #{pane_id}'"]);
    c.wait_idle();
    let (_, body) = c.answer("list-panes");
    let mut f = body[0].split(' ');
    let wid = f.next().unwrap()[1..].parse().unwrap();
    let pane = f.next().unwrap()[1..].parse().unwrap();
    (c, sid, wid, pane)
}

/// Wait for a pane's shell prompt (`$ `), through the client.
fn wait_prompt(c: &mut Cc, pane: u32) {
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        c.send(&[&format!("display -p -t %{pane} '#{{cursor_x}}'")]);
        c.wait_idle();
        if c.answer("display -p -t").1 == ["2"] {
            return;
        }
        assert!(Instant::now() < end, "no prompt in %{pane}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Lines from the server outside replies are notifications: never blank,
/// never anything but `%...` (WezTerm ends control mode otherwise).
fn assert_clean(c: &Cc) {
    for n in &c.notes {
        assert!(n.starts_with('%') && n.len() > 1, "a stray line outside a reply: {n:?}");
    }
}

/// What WezTerm (its main branch plus MisterTea's PRs) and MisterTea's
/// Ghostty branch send, from their source (S11): the replies they parse.
#[test]
fn wezterm_and_ghostty_get_what_they_parse() {
    let daemon = start();

    // ---- WezTerm: no pause mode, strict parsing
    let (mut c, sid, wid, pane) = attached(&daemon, &["-CC"]);
    c.send(&["list-commands"]);
    c.wait_idle();
    let (ok, cmds) = c.answer("list-commands");
    assert!(ok && cmds.iter().any(|l| l.starts_with("resize-window")), "WezTerm resizes only if listed");
    c.send(&[&format!(
        "list-windows -F '#{{session_id}} #{{window_id}} #{{window_width}} #{{window_height}} #{{window_active}} #{{window_name}} #{{window_layout}} #{{history_limit}}' -t ${sid}"
    )]);
    c.wait_idle();
    let (_, windows) = c.answer("list-windows");
    for w in &windows {
        let f: Vec<&str> = w.split(' ').collect();
        assert_eq!(f.len(), 8, "exactly 8 fields: {w}");
        assert!(f[2].parse::<u16>().is_ok() && f[7].parse::<u32>().is_ok(), "{w}");
    }
    c.send(&[&format!("capture-pane -p -t %{pane} -e -C -S -2000")]);
    c.send(&[&format!(
        "list-panes -F '#{{session_id}} #{{window_id}} #{{pane_id}} #{{pane_index}} #{{cursor_x}} #{{cursor_y}} #{{pane_width}} #{{pane_height}} #{{pane_left}} #{{pane_top}} #{{pane_active}}' -t @{wid}"
    )]);
    c.send(&["list-session"]);
    c.wait_idle();
    assert!(c.done.iter().all(|r| r.1), "no errors: {:#?}", c.done.iter().filter(|r| !r.1).collect::<Vec<_>>());
    let (_, panes) = c.answer("list-panes -F '#{session_id}");
    assert_eq!(panes[0].split(' ').count(), 11, "{panes:?}");
    // A split waits for %window-pane-changed.
    c.notes.clear();
    c.send(&[&format!("split-window -h -t %{pane}")]);
    c.wait_idle();
    let changed = c.wait_note(&format!("%window-pane-changed @{wid} %"));
    let new: u32 = changed.rsplit('%').next().unwrap().parse().unwrap();
    assert_ne!(new, pane);
    // Keys as hex bytes; output as plain %output (it can't read
    // %extended-output).
    wait_prompt(&mut c, new);
    c.send(&[&format!("send-keys -H -t %{new} 0x65 0x63 0x68 0x6F 0x20 0x77 0x7A 0x2D 0x6F 0x6B 0x0D")]);
    c.wait_idle();
    c.pump(Duration::from_millis(500));
    let out: String = c.notes.iter().filter(|n| n.starts_with(&format!("%output %{new} "))).cloned().collect();
    assert!(out.contains("wz-ok\\015\\012"), "{out}");
    assert!(!c.notes.iter().any(|n| n.starts_with("%extended-output")));
    c.notes.clear();
    c.send(&[&format!("resize-window -x 100 -y 30 -t @{wid}"), &format!("resize-pane -x 30 -y 20 -t %{new}")]);
    c.wait_idle();
    let layout = c.notes.iter().rev().find(|n| n.starts_with("%layout-change")).expect("layout change").clone();
    let l = layout.split_whitespace().nth(2).unwrap();
    assert!(l[5..].starts_with("100x30,0,0{69x30,0,0,") && l.contains(",30x30,70,0,"), "{layout}");
    assert_clean(&c);
    c.close();

    // ---- Ghostty (MisterTea's branch): strict about errors and field counts
    let (mut c, sid, wid, pane) = attached(&daemon, &["-CC"]);
    c.send(
        &["set -t $".to_owned() + &sid.to_string() + " @affinities 'a b'"]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    c.send(&["display-message -p '#{version}'"]);
    c.send(&["list-windows -F '#{session_id}\t#{window_id}\t#{window_width}\t#{window_height}\t#{window_layout}\tA#{@affinities}'"]);
    for screen in ["", "-a "] {
        c.send(&[&format!("capture-pane -p -e -C -q {screen}-S - -E -1 -t %{pane}")]);
        c.send(&[&format!("capture-pane -p -e -C -q {screen}-t %{pane}")]);
    }
    let vars = [
        "pane_id",
        "cursor_x",
        "cursor_y",
        "cursor_flag",
        "cursor_shape",
        "cursor_colour",
        "cursor_blinking",
        "alternate_on",
        "alternate_saved_x",
        "alternate_saved_y",
        "insert_flag",
        "wrap_flag",
        "keypad_flag",
        "keypad_cursor_flag",
        "origin_flag",
        "mouse_all_flag",
        "mouse_any_flag",
        "mouse_button_flag",
        "mouse_standard_flag",
        "mouse_utf8_flag",
        "mouse_sgr_flag",
        "focus_flag",
        "bracketed_paste",
        "scroll_region_upper",
        "scroll_region_lower",
        "pane_tabs",
    ];
    let fmt: Vec<String> = vars.iter().map(|v| format!("#{{{v}}}")).collect();
    c.send(&[&format!("list-panes -F '{}'", fmt.join(";"))]);
    c.wait_idle();
    assert!(
        c.done.iter().all(|r| r.1),
        "an %error ends Ghostty's session: {:#?}",
        c.done.iter().filter(|r| !r.1).collect::<Vec<_>>()
    );
    assert_eq!(c.answer("display-message").1, ["3.5a"], "one token");
    let (_, w) = c.answer("list-windows");
    let f: Vec<&str> = w[0].split("\\t").collect();
    assert_eq!(f.len(), 6);
    assert_eq!(f[5], "Aa b");
    let (sum, body) = f[4].split_once(',').unwrap();
    assert_eq!(sum, checksum(body), "Ghostty checks the layout's checksum");
    let (_, p) = c.answer("list-panes");
    assert_eq!(p[0].split(';').count(), 26, "{p:?}");
    // send-keys -H carries code points to it: a snowman.
    wait_prompt(&mut c, pane);
    c.notes.clear();
    c.send(&[&format!("send-keys -t %{pane} -H 0x65 0x63 0x68 0x6f 0x20 0x2603 0xd")]);
    c.wait_idle();
    c.pump(Duration::from_millis(500));
    let out: String = c.notes.iter().filter(|n| n.starts_with(&format!("%output %{pane} "))).cloned().collect();
    assert!(out.contains("\u{2603}\\015\\012"), "{out}");
    // A new window and a split from its menus.
    c.notes.clear();
    c.send(&["new-window"]);
    c.wait_idle();
    let add = c.wait_note("%window-add @");
    assert!(c.notes.iter().any(|n| n.starts_with(&format!("%session-window-changed ${sid} @"))), "{:#?}", c.notes);
    let w2 = add.trim_start_matches("%window-add @");
    c.send(&[&format!("kill-window -t @{w2}")]);
    c.wait_idle();
    c.wait_note(&format!("%window-close @{w2}"));
    let _ = wid;
    assert_clean(&c);
    c.close();
}

/// Formats against real tmux 3.6 on the same layout: a 120x40 window split
/// side by side, each pane at a `$ ` prompt.
#[test]
fn formats_match_real_tmux() {
    // `tmux 3.6`, `tmux 3.5a`, `tmux next-3.7`: the formats compared are 3.6's.
    let Ok(v) = Command::new("tmux").arg("-V").output() else {
        eprintln!("tmux not installed; skipping");
        return;
    };
    let v = String::from_utf8_lossy(&v.stdout);
    let num: String = v
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .trim_start_matches("next-")
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = num.split('.').map(|n| n.parse::<u32>().unwrap_or(0));
    if (parts.next().unwrap_or(0), parts.next().unwrap_or(0)) < (3, 6) {
        eprintln!("{} is older than 3.6; skipping", v.trim());
        return;
    }
    let daemon = start();
    let (mut c, _, _, first) = attached(&daemon, &["-CC"]);
    c.send(&["rename-session s11", &format!("split-window -h -t %{first}")]);
    c.wait_idle();
    let second = c.wait_note("%window-pane-changed").rsplit('%').next().unwrap().parse::<u32>().unwrap();
    wait_prompt(&mut c, first);
    wait_prompt(&mut c, second);

    let sock = format!("illogical-m5-fmt-{}", std::process::id());
    let conf = std::env::temp_dir().join(format!("{sock}.conf"));
    std::fs::write(&conf, "set -g default-command \"env PS1='$ ' bash --norc --noprofile\"\n").unwrap();
    let tmux = |args: &[&str]| -> String {
        let o = Command::new("tmux").arg("-L").arg(&sock).arg("-f").arg(&conf).args(args).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_owned()
    };
    tmux(&["kill-server"]);
    tmux(&["new", "-d", "-s", "s11", "-x", "120", "-y", "40"]);
    tmux(&["set", "-g", "window-size", "manual"]);
    tmux(&["split-window", "-h", "-t", "%0"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while ["%0", "%1"].iter().any(|p| tmux(&["display", "-p", "-t", p, "#{cursor_x}"]) != "2") {
        assert!(Instant::now() < deadline, "tmux's shells didn't start");
        std::thread::sleep(Duration::from_millis(100));
    }

    let formats = [
        "#{session_name} #{session_windows}",
        "#{window_index} #{window_name} #{window_width}x#{window_height} #{window_panes}",
        "#{window_layout}",
        "#{window_visible_layout} #{window_zoomed_flag}",
        "#{window_flags} #{window_active} #{window_start_flag} #{window_end_flag} #{window_last_flag}",
        "#{pane_index} #{pane_active} #{pane_width}x#{pane_height}",
        "#{pane_left},#{pane_top},#{pane_right},#{pane_bottom}",
        "#{pane_at_left}#{pane_at_right}#{pane_at_top}#{pane_at_bottom}",
        "#{cursor_x},#{cursor_y} #{alternate_on} #{alternate_saved_x},#{alternate_saved_y}",
        "#{scroll_region_upper} #{scroll_region_lower} #{pane_tabs}",
        "#{cursor_flag}#{insert_flag}#{keypad_cursor_flag}#{keypad_flag}#{wrap_flag}#{origin_flag}",
        "#{mouse_standard_flag}#{mouse_button_flag}#{mouse_any_flag}#{mouse_utf8_flag}#{mouse_sgr_flag}#{mouse_all_flag}",
        "[#{focus_flag}] [#{bracketed_paste}] [#{bracket_paste_flag}]",
        "#{cursor_shape} #{cursor_colour} #{cursor_blinking} #{pane_key_mode} #{pane_dead} #{history_limit}",
        "#{?window_active,yes,no} #{?pane_active,#{pane_id},-}",
        "#{?#{==:#{pane_index},1},second,first} #{!=:a,b} #{||:0,#{pane_active}} #{&&:1,0}",
        "#{=3:session_name}|#{=-2:session_name}|#{n:session_name}|#{s/1/one/:session_name}",
        "#{l:#{pane_id}} ## #S:#I.#P #W #{pane-border-status} #{T:set-clipboard}",
        "#{q:window_layout}",
        "#{unknown_variable}|#{@unset_option}",
    ];
    let mut failures = vec![];
    for (ours_pane, their_pane) in [(first, "%0"), (second, "%1")] {
        for f in formats {
            c.send(&[&format!("display -p -t %{ours_pane} '{f}'")]);
            c.wait_idle();
            let (ok, body) = c.answer("display -p -t");
            assert!(ok, "{f}");
            let ours = normalize(&body.join("\n"));
            let theirs = normalize(&tmux(&["display", "-p", "-t", their_pane, f]).replace('\t', "\\t"));
            if ours != theirs {
                failures.push(format!("{f}\n  ours {ours}\n  tmux {theirs}"));
            }
        }
    }
    tmux(&["kill-server"]);
    let _ = std::fs::remove_file(&conf);
    c.close();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A client that stops reading falls behind; the daemon drops it from the
/// pane, and with pause mode on that becomes `%pause`. Capturing again and
/// `continue` carries on from the capture.
#[test]
fn falling_behind_pauses_the_pane() {
    let daemon = start();
    let (mut c, _, _, pane) = attached(&daemon, &["-CC"]);
    c.send(&["refresh-client -fpause-after=1"]);
    c.wait_idle();
    wait_prompt(&mut c, pane);
    c.close();

    // Not reading: the front end blocks writing to us.
    let mut c = Cc::start_stalled(&daemon, &["-CC"]);
    c.send(&["refresh-client -fpause-after=1", &format!("refresh-client -C {COLS},{ROWS}")]);
    // A flood that lasts until it's stopped: how long a fixed amount takes
    // depends on the machine's load, and one still running after `continue`
    // would make the client fall behind (and pause) again.
    paste(&mut c, pane, "yes m5-flood; echo flood-done\r");
    std::thread::sleep(Duration::from_secs(6));
    c.resume();
    let end = Instant::now() + Duration::from_secs(120);
    while !c.notes.contains(&format!("%pause %{pane}")) {
        assert!(Instant::now() < end, "no %pause");
        c.pump(Duration::from_millis(200));
    }
    // Paused: stop the flood (^C) and wait for the prompt, which the
    // mirror sees though the client isn't sent it.
    c.send(&[&format!("send -H -t %{pane} 03")]);
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        c.send(&[&format!("capture-pane -p -t %{pane}")]);
        c.wait_idle();
        let (_, body) = c.answer("capture-pane -p -t");
        if body.iter().rev().find(|l| !l.trim().is_empty()).is_some_and(|l| l.trim() == "$") {
            break;
        }
        assert!(Instant::now() < end, "the flood didn't stop: {:?}", body.last());
        std::thread::sleep(Duration::from_millis(200));
    }
    // Nothing more for it until continue.
    c.notes.clear();
    c.pump(Duration::from_secs(1));
    assert!(!c.notes.iter().any(|n| n.starts_with(&format!("%extended-output %{pane} "))), "output while paused");
    send_all(&mut c, &pane_requests(pane));
    c.wait_idle();
    assert!(c.notes.contains(&format!("%continue %{pane}")), "{:#?}", c.notes);
    // The capture shows where it got to; the rest follows.
    paste(&mut c, pane, "echo after-$((6*7))\r");
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        let out: String = c.notes.iter().filter(|n| n.contains(&format!(" %{pane} "))).cloned().collect();
        if out.contains("after-42") {
            break;
        }
        assert!(
            Instant::now() < end,
            "no output after continue: {:#?}",
            c.notes.iter().filter(|n| !n.starts_with("%extended-output")).collect::<Vec<_>>()
        );
        c.pump(Duration::from_millis(200));
    }
    c.close();
}

/// On a terminal (ssh -t), control mode is raw: nothing is echoed, CR ends
/// a command as iTerm2 sends it, and lines go out with CRLF.
#[test]
fn on_a_terminal_nothing_is_echoed() {
    use std::os::fd::{AsRawFd, FromRawFd};
    let daemon = start();
    let pty = nix::pty::openpty(None, None).unwrap();
    let slave = |_| unsafe { Stdio::from_raw_fd(nix::libc::dup(pty.slave.as_raw_fd())) };
    let mut child = Command::new(cli_bin())
        .arg("--socket")
        .arg(daemon.sock())
        .args(["tmux", "-CC"])
        .stdin(slave(0))
        .stdout(slave(1))
        .stderr(slave(2))
        .spawn()
        .unwrap();
    drop(pty.slave);
    let mut master = std::fs::File::from(pty.master);
    let read_until = |master: &mut std::fs::File, want: &str| -> String {
        let mut out = Vec::new();
        let end = Instant::now() + Duration::from_secs(10);
        let fd = master.as_raw_fd();
        while !String::from_utf8_lossy(&out).contains(want) {
            assert!(Instant::now() < end, "no {want:?} in {:?}", String::from_utf8_lossy(&out));
            let mut fds = [nix::poll::PollFd::new(
                unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) },
                nix::poll::PollFlags::POLLIN,
            )];
            if nix::poll::poll(&mut fds, nix::poll::PollTimeout::from(200u16)).unwrap() > 0 {
                let mut buf = [0u8; 4096];
                let n = master.read(&mut buf).unwrap();
                out.extend_from_slice(&buf[..n]);
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    let hello = read_until(&mut master, "%session-changed");
    assert!(hello.starts_with("\x1bP1000p%begin "), "{hello:?}");
    master.write_all(b"\x03display -p 'm5-#{version}'\r").unwrap();
    let out = read_until(&mut master, "%end");
    assert!(!out.contains("display -p"), "echoed: {out:?}");
    assert!(out.contains("\r\nm5-3.5a\r\n%end "), "{out:?}");
    master.write_all(b"\r").unwrap();
    let bye = read_until(&mut master, "%exit");
    assert!(bye.contains("%exit\r\n"), "{bye:?}");
    let _ = child.kill();
    let _ = child.wait();
}

/// A block that isn't a terminal is a read-only pane drawn from its text.
#[test]
fn a_browser_block_is_a_read_only_pane() {
    let daemon = start();
    let (mut c, _, wid, pane) = attached(&daemon, &["-CC"]);
    c.notes.clear();
    let body = format!(r#"{{"type":"browser","config":{{"url":"http://127.0.0.1:9/"}},"split":{pane}}}"#);
    let mut s = UnixStream::connect(daemon.sock()).unwrap();
    s.write_all(
        format!(
            "POST /api/blocks HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )
    .unwrap();
    let mut res = String::new();
    s.read_to_string(&mut res).unwrap();
    let block: u32 = Regex::new(r#""block":(\d+)"#).unwrap().captures(&res).expect(&res)[1].parse().unwrap();
    let layout = c.wait_note(&format!("%layout-change @{wid} "));
    assert!(panes_in_layout(&layout).contains(&block), "{layout}");
    c.send(&[&format!("capture-pane -peqJN -t %{block}"), &format!("send -lt %{block} ignored")]);
    c.wait_idle();
    assert!(c.done.iter().all(|r| r.1));
    let (_, shown) = c.answer("capture-pane");
    assert!(shown.iter().any(|l| l.contains(&format!("%{block} is a block"))), "{shown:#?}");
    c.close();
}

/// What `%extended-output` notes for a pane carried, joined.
fn output(notes: &[String], pane: u32) -> String {
    let prefix = format!("%extended-output %{pane} ");
    notes.iter().filter_map(|n| n.strip_prefix(&prefix)?.split_once(" : ").map(|(_, data)| data)).collect()
}
