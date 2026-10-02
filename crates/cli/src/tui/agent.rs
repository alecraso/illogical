//! Agent blocks (M6b) in the TUI: the run as a plain transcript of
//! messages, thoughts and tool calls, with its permission requests at the
//! bottom to allow or deny from the keyboard. Questions and forms show as a
//! line; they're answered in the web client (or dismissed from the sidebar).

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use serde_json::Value;

/// A permission request the agent is waiting on.
pub struct Perm {
    pub id: String,
    pub title: String,
}

pub fn pending(state: &Value) -> Vec<Perm> {
    state["pending"]
        .as_array()
        .map(|ps| {
            ps.iter()
                .filter_map(|p| {
                    Some(Perm {
                        id: p["id"].as_str()?.to_owned(),
                        title: p["title"].as_str().unwrap_or("a tool").to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Lines of the transcript, oldest first, wrapped to `width`.
fn transcript(state: &Value, width: usize) -> Vec<(String, Style)> {
    let mut out = Vec::new();
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut push = |text: &str, style: Style, indent: &str| {
        for raw in text.lines() {
            wrap(raw, width.saturating_sub(indent.chars().count()).max(8), |l| {
                out.push((format!("{indent}{l}"), style));
            });
        }
    };
    for e in state["entries"].as_array().into_iter().flatten() {
        let text = e["text"].as_str().unwrap_or("");
        match e["type"].as_str() {
            Some("user") => push(text, Style::default().add_modifier(Modifier::BOLD), "› "),
            Some("agent") => push(text, Style::default(), ""),
            Some("thought") => push(text, dim.add_modifier(Modifier::ITALIC), "· "),
            Some("note") => push(text, dim, "  "),
            Some("tool") => {
                let title = e["title"].as_str().unwrap_or("tool");
                let status = e["status"].as_str().unwrap_or("");
                let mark = match status {
                    "completed" => "✓",
                    "failed" => "✗",
                    _ => "▸",
                };
                push(&format!("{mark} {title}"), Style::default().fg(Color::Cyan), "");
                if let Some(cmd) = e["command"].as_str() {
                    push(&format!("$ {cmd}"), dim, "  ");
                }
                let output = plain(e["output"].as_str().unwrap_or(""));
                let tail: Vec<&str> = output.lines().rev().take(6).collect();
                for l in tail.into_iter().rev() {
                    push(l, dim, "  ");
                }
            }
            _ => continue,
        }
    }
    out
}

/// Terminal output as plain text: escape sequences (colors, cursor moves)
/// dropped, carriage returns too.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: parameters, then a final byte in @..~.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ST.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// Wrap one line at `width` characters.
fn wrap(line: &str, width: usize, mut f: impl FnMut(&str)) {
    if line.is_empty() {
        f("");
        return;
    }
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut start = 0;
    while start < chars.len() {
        let end = (start + width).min(chars.len());
        let from = chars[start].0;
        let to = chars.get(end).map_or(line.len(), |c| c.0);
        f(&line[from..to]);
        start = end;
    }
}

/// Draw the block into `area`, `scroll` lines up from the newest.
pub fn draw(buf: &mut Buffer, area: Rect, state: &Value, scroll: usize, focused: bool) {
    let w = area.width as usize;
    let label = state["label"].as_str().or(state["agent"].as_str()).unwrap_or("agent");
    let status = state["status"].as_str().unwrap_or("");
    // M33: a conversation opened here and not continued yet.
    let imported = &state["import"];
    let opened = imported.is_object() && imported["continued"] != true;
    let held = imported["held"]["place"].as_str();
    let head = match (opened, held) {
        // Its title says which conversation.
        (true, Some(_)) => format!(" {} · open elsewhere", state["title"].as_str().unwrap_or(label)),
        (true, None) => format!(" {} · conversation", state["title"].as_str().unwrap_or(label)),
        _ => format!(" {label} · {status}"),
    };
    buf.set_stringn(area.x, area.y, &head, w, Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED));
    let fill = w.saturating_sub(head.chars().count());
    buf.set_stringn(
        area.x + (w - fill) as u16,
        area.y,
        " ".repeat(fill),
        fill,
        Style::default().add_modifier(Modifier::REVERSED),
    );

    // What it waits on goes at the bottom, above the hint.
    let mut foot: Vec<(String, Style)> = Vec::new();
    let warn = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let perms = pending(state);
    for (i, p) in perms.iter().enumerate() {
        let keys = if i == 0 { "  a allow · A always · d deny" } else { "" };
        foot.push((format!("⚠ allow {}?{keys}", p.title), warn));
    }
    for a in state["asks"].as_array().into_iter().flatten() {
        let q = a["questions"][0]["question"].as_str().or(a["message"].as_str()).unwrap_or("a question");
        foot.push((format!("? {q}  (answer in the web client)"), warn));
    }
    if opened {
        let from = match imported["source"].as_str() {
            Some("desktop") => "the desktop app",
            Some("terminal") => "a terminal",
            _ => "elsewhere",
        };
        foot.push((
            match held {
                Some(place) => format!("From {from}, {place}: F fork it to go on here"),
                None => format!("From {from}: C continue it here · F fork it"),
            },
            Style::default().fg(Color::Cyan),
        ));
    }
    if focused {
        let hint = if state["status"] == "working" {
            "i type a message (queued) · PgUp/PgDn scroll"
        } else {
            "i type a message · PgUp/PgDn scroll"
        };
        foot.push((hint.to_owned(), Style::default().fg(Color::DarkGray)));
    }

    let body = area.height.saturating_sub(1 + foot.len() as u16) as usize;
    let lines = transcript(state, w);
    let end = lines.len().saturating_sub(scroll);
    let start = end.saturating_sub(body);
    for (i, (text, style)) in lines[start..end].iter().enumerate() {
        buf.set_stringn(area.x, area.y + 1 + i as u16, text, w, *style);
    }
    let top = area.y + area.height - foot.len() as u16;
    for (i, (text, style)) in foot.iter().enumerate() {
        buf.set_stringn(area.x, top + i as u16, text, w, *style);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn transcript_and_pending() {
        let state = json!({
            "label": "claude", "status": "working",
            "entries": [
                {"type": "user", "text": "fix the build", "at_ms": 1},
                {"type": "agent", "text": "Looking."},
                {"type": "tool", "id": "t1", "title": "Run cargo build", "kind": "execute", "status": "completed",
                 "command": "cargo build", "output": "a\nb\nerror: x", "text": "", "locations": [], "started_ms": 1},
            ],
            "pending": [{"id": "p1", "tool_call_id": "t2", "tool": "Bash", "title": "rm -rf target", "kind": "execute",
                         "options": [], "at_ms": 2}],
            "asks": [],
        });
        let lines: Vec<String> = transcript(&state, 40).into_iter().map(|l| l.0).collect();
        assert_eq!(lines[0], "› fix the build");
        assert!(lines.contains(&"✓ Run cargo build".to_owned()));
        assert!(lines.contains(&"  error: x".to_owned()));
        let p = pending(&state);
        assert_eq!((p[0].id.as_str(), p[0].title.as_str()), ("p1", "rm -rf target"));

        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        draw(&mut buf, Rect::new(0, 0, 40, 10), &state, 0, true);
        let row = |y: u16| (0..40).map(|x| buf[(x, y)].symbol().to_owned()).collect::<String>();
        assert!(row(8).starts_with("⚠ allow rm -rf target?"), "{}", row(8));
    }

    #[test]
    fn an_opened_conversation_says_how_to_go_on() {
        let mut state = json!({
            "label": "Claude Code", "title": "Fix the build", "status": "stopped",
            "entries": [{"type": "user", "text": "fix the build", "at_ms": 1}],
            "pending": [], "asks": [],
            "import": {"source": "terminal", "continued": false, "held": null},
        });
        let rows = |state: &Value| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 60, 6));
            draw(&mut buf, Rect::new(0, 0, 60, 6), state, 0, true);
            (0..6).map(|y| (0..60).map(|x| buf[(x, y)].symbol().to_owned()).collect::<String>()).collect::<Vec<_>>()
        };
        let r = rows(&state);
        assert!(r[0].starts_with(" Fix the build · conversation"), "{r:?}");
        assert!(r[4].starts_with("From a terminal: C continue it here · F fork it"), "{r:?}");
        state["import"]["held"] = json!({"place": "open in pane %4"});
        let r = rows(&state);
        assert!(r[0].starts_with(" Fix the build · open elsewhere"), "{r:?}");
        assert!(r[4].starts_with("From a terminal, open in pane %4: F fork it"), "{r:?}");
        state["import"]["continued"] = json!(true);
        assert!(rows(&state)[0].starts_with(" Claude Code · stopped"));
    }

    #[test]
    fn output_is_plain_text() {
        assert_eq!(plain("\x1b[32mran\x1b[0m: ok\r\n\x1b]0;title\x07done"), "ran: ok\ndone");
    }

    #[test]
    fn long_lines_wrap() {
        let mut got = Vec::new();
        wrap("abcdefghij", 4, |l| got.push(l.to_owned()));
        assert_eq!(got, ["abcd", "efgh", "ij"]);
    }
}
