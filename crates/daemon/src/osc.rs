//! Picks the structure out of a pane's output: where prompts and commands
//! begin and end (OSC 133, and VS Code's OSC 633 with the command line), the
//! working directory (OSC 7), notifications (OSC 9, OSC 777, OSC 99) and the
//! bell. The bytes themselves pass through untouched; this only watches.
//!
//! It is a small state machine that survives sequences split across reads,
//! and it ignores BEL inside strings (an OSC's terminator, or DCS/APC data).

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Signal {
    /// A prompt is being drawn (133;A).
    Prompt,
    /// The command line, as the shell integration reports it (633;E).
    CommandLine {
        text: String,
    },
    /// The command started running (133;C).
    CommandStart,
    /// The command finished (133;D[;exit]).
    CommandEnd {
        exit: Option<i32>,
    },
    /// The working directory (OSC 7 `file://host/path`).
    Cwd {
        path: String,
    },
    /// A program asked for attention (OSC 9, OSC 777 notify, OSC 99).
    Notify {
        title: String,
        body: String,
    },
    Bell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    Osc,
    OscEsc,
    /// DCS, SOS, PM or APC data: skipped until ST.
    Str,
    StrEsc,
}

/// OSC payloads longer than this are dropped (they're images or clipboard
/// data, not anything we read).
const MAX_OSC: usize = 64 * 1024;

pub struct Scanner {
    state: State,
    buf: Vec<u8>,
    overflow: bool,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    pub fn new() -> Self {
        Self { state: State::Ground, buf: Vec::new(), overflow: false }
    }

    /// Feed output that starts at stream offset `base`; returns each signal
    /// with the offset just past the byte that completed it.
    pub fn feed(&mut self, data: &[u8], base: u64) -> Vec<(u64, Signal)> {
        let mut out = Vec::new();
        for (i, &b) in data.iter().enumerate() {
            let at = base + i as u64 + 1;
            self.state = match (self.state, b) {
                (State::Ground, 0x1b) => State::Esc,
                (State::Ground, 0x07) => {
                    out.push((at, Signal::Bell));
                    State::Ground
                }
                (State::Ground, _) => State::Ground,
                (State::Esc, b']') => {
                    self.buf.clear();
                    self.overflow = false;
                    State::Osc
                }
                (State::Esc, b'P' | b'X' | b'^' | b'_') => State::Str,
                (State::Esc, 0x1b) => State::Esc,
                (State::Esc, _) => State::Ground,
                (State::Osc, 0x07) => {
                    self.finish(at, &mut out);
                    State::Ground
                }
                (State::Osc, 0x1b) => State::OscEsc,
                (State::Osc, 0x18 | 0x1a) => State::Ground, // CAN/SUB abort
                (State::Osc, b) => {
                    if self.buf.len() < MAX_OSC {
                        self.buf.push(b);
                    } else {
                        self.overflow = true;
                    }
                    State::Osc
                }
                (State::OscEsc, b'\\') => {
                    self.finish(at, &mut out);
                    State::Ground
                }
                (State::OscEsc, b']') => {
                    // ESC ] inside an OSC: a new one begins.
                    self.buf.clear();
                    self.overflow = false;
                    State::Osc
                }
                (State::OscEsc, _) => State::Ground,
                (State::Str, 0x1b) => State::StrEsc,
                (State::Str, 0x18 | 0x1a) => State::Ground,
                (State::Str, _) => State::Str,
                (State::StrEsc, b'\\') => State::Ground,
                (State::StrEsc, 0x1b) => State::StrEsc,
                (State::StrEsc, _) => State::Str,
            };
        }
        out
    }

    fn finish(&mut self, at: u64, out: &mut Vec<(u64, Signal)>) {
        if self.overflow {
            return;
        }
        if let Some(s) = parse_osc(&self.buf) {
            out.push((at, s));
        }
    }
}

fn parse_osc(payload: &[u8]) -> Option<Signal> {
    let text = String::from_utf8_lossy(payload);
    let (code, rest) = text.split_once(';').unwrap_or((&text, ""));
    match code {
        "133" | "633" => {
            let (kind, args) = rest.split_once(';').unwrap_or((rest, ""));
            match kind {
                "A" => Some(Signal::Prompt),
                "C" => Some(Signal::CommandStart),
                "D" => Some(Signal::CommandEnd { exit: args.split(';').next().and_then(|c| c.parse().ok()) }),
                // 633;E;<command line>[;<nonce>]: the line is escaped so that
                // ';' never appears raw.
                "E" if code == "633" => {
                    Some(Signal::CommandLine { text: unescape_633(args.split(';').next().unwrap_or("")) })
                }
                _ => None,
            }
        }
        "7" => {
            let url = rest.strip_prefix("file://")?;
            let path = &url[url.find('/')?..];
            Some(Signal::Cwd { path: local_path(percent_decode(path)) })
        }
        // OSC 9;<message>; but 9;4;… is ConEmu/Ghostty progress, not a
        // notification.
        "9" if !rest.starts_with("4;") && !rest.is_empty() => {
            Some(Signal::Notify { title: String::new(), body: rest.to_owned() })
        }
        "777" => {
            let mut parts = rest.splitn(3, ';');
            (parts.next()? == "notify").then(|| Signal::Notify {
                title: parts.next().unwrap_or("").to_owned(),
                body: parts.next().unwrap_or("").to_owned(),
            })
        }
        // Kitty's OSC 99: metadata ; payload. Only the plain, unencoded
        // payload is read; chunked or base64 notifications show as a title.
        "99" => {
            let (meta, body) = rest.split_once(';')?;
            let is_title = meta.split(':').any(|kv| kv == "p=title");
            let encoded = meta.split(':').any(|kv| kv == "e=1");
            let body = if encoded { String::new() } else { body.to_owned() };
            Some(if is_title {
                Signal::Notify { title: body, body: String::new() }
            } else {
                Signal::Notify { title: String::new(), body }
            })
        }
        _ => None,
    }
}

/// VS Code's 633 escaping: `\\` for a backslash, `\xHH` for anything else
/// that would break the sequence (`;`, control characters).
fn unescape_633(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            if b[i + 1] == b'\\' {
                out.push(b'\\');
                i += 2;
                continue;
            }
            if b[i + 1] == b'x'
                && i + 3 < b.len()
                && let Ok(v) = u8::from_str_radix(&s[i + 2..i + 4], 16)
            {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A file URL's path as this system writes it: `/C:/Users/x` is `C:\\Users\\x`
/// on Windows.
fn local_path(path: String) -> String {
    let b = path.as_bytes();
    let drive = b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':';
    if cfg!(windows) && drive {
        let p = path[1..].replace('/', "\\");
        // `C:` alone is the drive's current directory, not its root.
        if p.len() == 2 { p + "\\" } else { p }
    } else {
        path
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(Ok(v)) = s.get(i + 1..i + 3).map(|h| u8::from_str_radix(h, 16))
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Output with escape sequences and control characters removed (for search
/// and `--text`): what a reader would see, line by line.
pub fn strip(data: &[u8]) -> String {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        if b == 0x1b {
            i += 1;
            match data.get(i) {
                Some(b'[') => {
                    i += 1;
                    while i < data.len() && !(0x40..=0x7e).contains(&data[i]) {
                        i += 1;
                    }
                    i += 1;
                }
                Some(b']' | b'P' | b'X' | b'^' | b'_') => {
                    while i < data.len() && data[i] != 0x07 && !(data[i] == 0x1b && data.get(i + 1) == Some(&b'\\')) {
                        i += 1;
                    }
                    i += if data.get(i) == Some(&0x07) { 1 } else { 2 };
                }
                Some(_) => i += 1,
                None => {}
            }
            continue;
        }
        match b {
            b'\n' | b'\t' => out.push(b),
            b'\r' => {
                if let Some(used) = eol_mark(&mut out, &data[i + 1..]) {
                    i += 1 + used;
                    continue;
                }
                // CR LF is a newline; a bare CR rewrites the line, which for
                // reading purposes we treat as a newline too (but not right
                // after one, where it moves nothing; at the start of `data`
                // it may end a line from the read before). CRs in a row are
                // one: a macOS pty sometimes writes CR CR LF for a LF.
                if out.last() != Some(&b'\n') && !matches!(data.get(i + 1), Some(b'\n' | b'\r')) {
                    out.push(b'\n');
                }
            }
            0x08 => {
                out.pop();
            }
            0x00..=0x1f | 0x7f => {}
            _ => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// zsh's PROMPT_SP (#348): before each prompt it writes PROMPT_EOL_MARK
/// (a reverse-video `%`, or `#` for root), pads the line to the screen's
/// width, then `\r`, as many spaces as the mark is wide, and `\r` again.
/// If the output ended with a newline that blanks the mark; if not, the
/// padding wraps and the mark stays after the output. It isn't the
/// program's output either way.
///
/// Called at the first `\r` with `out` stripped so far and `rest` the bytes
/// after it: when they're `<w spaces>\r` and `out`'s line ends with a mark
/// `w` columns wide and its padding, drop both and say how many bytes of
/// `rest` that used. A `%` the program wrote stays: only the `w` columns
/// right before the padding are the mark.
fn eol_mark(out: &mut Vec<u8>, rest: &[u8]) -> Option<usize> {
    /// The fewest padding spaces taken for PROMPT_SP's (a pane narrower
    /// than this plus the mark gets its marker kept).
    const MIN_PAD: usize = 8;
    let w = rest.iter().take_while(|b| **b == b' ').count();
    if w > 8 || rest.get(w) != Some(&b'\r') {
        return None;
    }
    let line_start = out.iter().rposition(|b| *b == b'\n').map_or(0, |n| n + 1);
    let line = &out[line_start..];
    let pad = line.iter().rev().take_while(|b| **b == b' ').count();
    if pad < MIN_PAD {
        return None;
    }
    let before = std::str::from_utf8(&line[..line.len() - pad]).ok()?;
    let mut cut = before.len();
    let mut cols = 0;
    for (at, c) in before.char_indices().rev() {
        if cols >= w {
            break;
        }
        cols += if wide(c) { 2 } else { 1 };
        cut = at;
    }
    if cols != w {
        return None;
    }
    out.truncate(line_start + cut);
    // Output with no newline: the padding wrapped, so the prompt is on the
    // next line.
    if cut > 0 {
        out.push(b'\n');
    }
    Some(w + 1)
}

/// A character two columns wide (East Asian wide and emoji), near enough
/// for an end-of-line mark.
fn wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115f | 0x2e80..=0xa4cf | 0xac00..=0xd7a3 | 0xf900..=0xfaff
        | 0xfe30..=0xfe4f | 0xff00..=0xff60 | 0xffe0..=0xffe6 | 0x1f300..=0x1faff | 0x20000..=0x3fffd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<Signal> {
        let mut s = Scanner::new();
        let mut base = 0;
        let mut out = vec![];
        for c in chunks {
            out.extend(s.feed(c, base).into_iter().map(|(_, sig)| sig));
            base += c.len() as u64;
        }
        out
    }

    #[test]
    fn prompts_commands_and_exit_codes() {
        let s = scan(&[b"\x1b]133;A\x07$ \x1b]633;E;echo hi\\x3b ls\x07\x1b]133;C\x07hi\r\n\x1b]133;D;3\x07"]);
        assert_eq!(
            s,
            vec![
                Signal::Prompt,
                Signal::CommandLine { text: "echo hi; ls".into() },
                Signal::CommandStart,
                Signal::CommandEnd { exit: Some(3) }
            ]
        );
    }

    #[test]
    fn sequences_split_across_reads() {
        let s = scan(&[b"\x1b]13", b"3;D;", b"0\x1b", b"\\", b"\x1b]7;file://geek/tmp/a%20b", b"\x07"]);
        assert_eq!(s, vec![Signal::CommandEnd { exit: Some(0) }, Signal::Cwd { path: "/tmp/a b".into() }]);
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_directory_from_powershell() {
        let s = scan(&[b"\x1b]7;file://WIN/C:/Program%20Files\x07\x1b]7;file://WIN/D:/\x07"]);
        assert_eq!(s, vec![Signal::Cwd { path: r"C:\Program Files".into() }, Signal::Cwd { path: r"D:\".into() }]);
    }

    #[test]
    fn notifications_and_bells() {
        let s = scan(&[b"\x1b]9;build done\x07\x1b]9;4;1;50\x07\x07\x1b]777;notify;Claude;needs you\x1b\\"]);
        assert_eq!(
            s,
            vec![
                Signal::Notify { title: "".into(), body: "build done".into() },
                Signal::Bell,
                Signal::Notify { title: "Claude".into(), body: "needs you".into() },
            ]
        );
    }

    #[test]
    fn bel_inside_strings_is_not_a_bell() {
        assert_eq!(scan(&[b"\x1bPq\x07data\x1b\\\x1b]2;title\x07"]), vec![]);
    }

    #[test]
    fn offsets_point_past_the_sequence() {
        let mut s = Scanner::new();
        let got = s.feed(b"ab\x1b]133;A\x07cd", 100);
        assert_eq!(got, vec![(110, Signal::Prompt)]);
    }

    #[test]
    fn strip_leaves_what_a_reader_sees() {
        let raw = b"\x1b[1;31mred\x1b[0m plain\r\nnext\x1b]133;A\x07 line\rover\x08x\n";
        assert_eq!(strip(raw), "red plain\nnext line\novex\n");
        assert_eq!(strip(b"491\r\r\n492\r\r493\r\n"), "491\n492\n493\n");
    }

    /// Bytes from zsh 5.9 in a 40-column pty (`zsh -f -i`, PS1='$ '): its
    /// PROMPT_SP mark and padding, after output with and without a newline.
    #[test]
    fn strip_drops_zshs_end_of_line_mark() {
        const MARK: &[u8] = b"\x1b[1m\x1b[7m%\x1b[27m\x1b[1m\x1b[0m                                       \r \r";
        let cat = |parts: &[&[u8]]| parts.concat();
        // A command's range, C to D: the mark comes before precmd's D.
        let ran = cat(&[b"\x1b]133;C\x07hi\r\n", MARK, b"\x1b]133;D;0\x07"]);
        assert_eq!(strip(&ran), "hi\n");
        // `printf abc`: no newline, so the mark followed the output.
        let abc = cat(&[b"abc", MARK, b"\r\x1b[0m\x1b[27m\x1b[24m\x1b[J$ \x1b[K"]);
        assert_eq!(strip(&abc), "abc\n$ ");
        // `printf '100%'`: the program's % stays.
        let pct = cat(&[b"100%", MARK, b"\r$ "]);
        assert_eq!(strip(&pct), "100%\n$ ");
        // A line of just %, from the program, then zsh's mark.
        let only = cat(&[b"%\r\n", MARK, b"\r$ "]);
        assert_eq!(strip(&only), "%\n$ ");
        // Root's #, and PROMPT_EOL_MARK set to '<-' and to ''.
        let root = b"ok\r\n\x1b[1m\x1b[7m#\x1b[27m\x1b[1m\x1b[0m                                       \r \r\r# ";
        assert_eq!(strip(root), "ok\n# ");
        assert_eq!(strip(b"abc<-                                      \r  \r\r$ "), "abc\n$ ");
        assert_eq!(strip(b"abc                                        \r\r\r$ "), "abc\n$ ");
        // A read that starts at the mark, the prompt's CR after a newline,
        // and a mark split from its last \r (a read that ends there), which
        // stays as it was.
        assert_eq!(strip(&cat(&[MARK, b"$ "])), "$ ");
        assert_eq!(strip(&cat(&[b"\n", MARK, b"\r$ "])), "\n$ ");
        assert_eq!(
            strip(b"hi\r\n%                                       \r "),
            "hi\n%                                       \n "
        );
        // A line cleared with CR, spaces and CR isn't a mark.
        assert_eq!(strip(b"50%\r          \r60%\n"), "50%\n          \n60%\n");
        assert_eq!(strip(b"\r60%"), "\n60%");
    }
}
