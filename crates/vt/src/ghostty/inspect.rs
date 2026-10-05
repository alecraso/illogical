//! Reading a terminal's state the way tmux reports it: cursor, scroll
//! region, tab stops, the cursor the alternate screen saved, and screen
//! lines (`capture-pane`). The tmux front end keeps one of these per pane as
//! a mirror of what it has sent its client, and answers from it.
//!
//! libghostty-vt has no getters for the scroll region, tab stops or saved
//! cursor; its formatter writes the first two as escape sequences, and the
//! saved cursor is where a copy of the terminal lands after leaving the
//! alternate screen.

use libghostty_vt::{
    Terminal,
    fmt::{Format, Formatter, FormatterOptions},
    screen::{CellWide, Screen},
    snapshot::Decoder,
    style::{StyleColor, Underline},
    terminal::{Mode, ModeKind, Point, PointCoordinate},
};

use super::GhosttyEngine;
use crate::Capabilities;

/// A line of a pane for `capture-pane -S`/`-E`: 0 is the top of the visible
/// screen and negative numbers count back into the scrollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    At(i64),
    /// `-`: the start of the scrollback (for `-S`) or the end of the screen
    /// (for `-E`).
    Edge,
}

/// `capture-pane`'s options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureOpts {
    /// `-a`: the screen that isn't showing (the primary while a full-screen
    /// program has the alternate one).
    pub other: bool,
    pub start: Line,
    pub end: Line,
    /// `-e`: colors and attributes as SGR sequences.
    pub escapes: bool,
    /// `-J`: join wrapped lines (and keep trailing spaces).
    pub join: bool,
    /// `-N`: keep trailing spaces.
    pub spaces: bool,
}

impl Default for CaptureOpts {
    fn default() -> Self {
        Self { other: false, start: Line::At(0), end: Line::Edge, escapes: false, join: false, spaces: false }
    }
}

impl GhosttyEngine {
    /// A terminal that only records: it answers no queries (the real one in
    /// the daemon does) and keeps `history` lines of scrollback.
    pub fn mirror(cols: u16, rows: u16, history: usize) -> Self {
        let mut e = Self::with_capabilities(cols, rows, Capabilities::ALL);
        e.term.set_scrollback_max_bytes(None).expect("scrollback limit");
        e.term.set_scrollback_max_lines(Some(history)).expect("scrollback lines");
        e
    }

    /// Cursor column and row on the active screen, from 0.
    pub fn cursor(&self) -> (u16, u16) {
        (self.term.cursor_x().unwrap_or(0), self.term.cursor_y().unwrap_or(0))
    }

    /// Whether an ANSI mode (`CSI n h`, e.g. 4 for insert) is set.
    pub fn ansi_mode(&self, mode: u16) -> bool {
        self.term.mode(Mode::new(mode, ModeKind::Ansi)).unwrap_or(false)
    }

    /// Lines of scrollback above the screen.
    pub fn history_lines(&self) -> usize {
        self.term.scrollback_rows().unwrap_or(0)
    }

    /// The scrolling region's top and bottom rows, from 0, inclusive.
    pub fn scroll_region(&self) -> (u16, u16) {
        let rows = self.term.rows().unwrap_or(0);
        let out = self.extras(true, false);
        let out = String::from_utf8_lossy(&out);
        // The formatter writes DECSTBM (`CSI t;b r`) after the content, and
        // only when the region isn't the whole screen.
        let decstbm = |seq: &str| {
            let (t, b) = seq.split_once('r')?.0.split_once(';')?;
            Some((t.parse::<u16>().ok()?.saturating_sub(1), b.parse::<u16>().ok()?.saturating_sub(1)))
        };
        out.rsplit("\x1b[").find_map(decstbm).unwrap_or((0, rows.saturating_sub(1)))
    }

    /// Tab stop columns, from 0.
    pub fn tab_stops(&self) -> Vec<u16> {
        let out = self.extras(false, true);
        let Some(start) = out.windows(4).position(|w| w == b"\x1b[3g") else { return vec![] };
        let mut stops = Vec::new();
        let mut rest = &out[start + 4..];
        // `CSI n G` then `ESC H`, per stop.
        while let Some(r) = rest.strip_prefix(b"\x1b[") {
            let digits = r.iter().take_while(|c| c.is_ascii_digit()).count();
            if digits == 0 || !r[digits..].starts_with(b"G\x1bH") {
                break;
            }
            let n: u16 = std::str::from_utf8(&r[..digits]).ok().and_then(|d| d.parse().ok()).unwrap_or(1);
            stops.push(n.saturating_sub(1));
            rest = &r[digits + 3..];
        }
        stops
    }

    /// Where the cursor goes back to when the full-screen program leaves the
    /// alternate screen, or `None` when the primary screen is showing.
    pub fn alt_saved_cursor(&self) -> Option<(u16, u16)> {
        if !self.on_alt() {
            return None;
        }
        let mut copy = self.copy()?;
        copy.vt_write(b"\x1b[?1049l");
        Some((copy.cursor_x().unwrap_or(0), copy.cursor_y().unwrap_or(0)))
    }

    fn on_alt(&self) -> bool {
        self.term.active_screen().ok() == Some(Screen::Alternate)
    }

    /// An independent copy, through Ghostty's own snapshot format.
    fn copy(&self) -> Option<Terminal<'static, 'static>> {
        let snap = self.term.encode_snapshot_alloc(None).ok().flatten()?.to_vec();
        Decoder::new_buf(&snap).ok()?.decode().ok()
    }

    /// The formatter's output with only the scroll region or tab stops
    /// added, over an empty selection-free screen read.
    fn extras(&self, region: bool, tabs: bool) -> Vec<u8> {
        let o = FormatterOptions::new()
            .with_format(Format::Vt)
            .with_scrolling_region(region)
            .with_tabstops(tabs)
            .with_cursor(false);
        let mut f = Formatter::new(&self.term, o).expect("formatter");
        f.format_alloc(None).map(|b| b.to_vec()).unwrap_or_default()
    }

    /// The active screen's lines as text, top to bottom: never the
    /// scrollback, wherever a viewer has scrolled to.
    pub fn screen_lines(&self) -> Vec<String> {
        capture(&self.term, &CaptureOpts::default())
            .into_iter()
            .map(|l| String::from_utf8_lossy(&l).into_owned())
            .collect()
    }

    /// `capture-pane`: lines of the screen (and scrollback), each without
    /// its newline. `None` for `other` when the alternate screen isn't on.
    pub fn capture(&self, opts: &CaptureOpts) -> Option<Vec<Vec<u8>>> {
        if opts.other {
            if !self.on_alt() {
                return None;
            }
            let mut copy = self.copy()?;
            // Mode 47 shows the primary screen without restoring a cursor.
            copy.vt_write(b"\x1b[?47l");
            return Some(capture(&copy, opts));
        }
        Some(capture(&self.term, opts))
    }
}

fn capture(term: &Terminal<'_, '_>, opts: &CaptureOpts) -> Vec<Vec<u8>> {
    let rows = term.rows().unwrap_or(0) as i64;
    let cols = term.cols().unwrap_or(0);
    let history = term.scrollback_rows().unwrap_or(0) as i64;
    let total = term.total_rows().map(|t| t as i64).unwrap_or(history + rows);
    let clamp = |l: i64| l.clamp(-history, rows - 1);
    let start = match opts.start {
        Line::At(n) => clamp(n),
        Line::Edge => -history,
    };
    let end = match opts.end {
        Line::At(n) => clamp(n),
        Line::Edge => rows - 1,
    };
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut pen = Pen::default();
    let mut joining = false;
    let keep_spaces = opts.join || opts.spaces;
    for line in start..=end {
        let y = total - rows + line;
        if y < 0 {
            continue;
        }
        let at = |x: u16| term.grid_ref(Point::Screen(PointCoordinate { x, y: y as u32 }));
        let wrapped = at(0).ok().and_then(|g| g.row().ok()).and_then(|r| r.is_wrapped().ok()).unwrap_or(false);
        let mut text = Vec::new();
        // Up to the last cell anything was written to (tmux's "used" cells).
        let mut used = 0;
        let mut cells = Vec::with_capacity(cols as usize);
        for x in 0..cols {
            let Ok(g) = at(x) else { break };
            let Ok(cell) = g.cell() else { break };
            let wide = cell.wide().unwrap_or(CellWide::Narrow);
            if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
                continue;
            }
            let mut buf = ['\0'; 16];
            let n = if cell.has_text().unwrap_or(false) { g.graphemes(&mut buf).unwrap_or(0) } else { 0 };
            let style = g.style().ok();
            let styled = style.is_some_and(|s| !s.is_default());
            let s: String = if n == 0 { " ".into() } else { buf[..n].iter().collect() };
            if n > 0 || styled {
                used = cells.len() + 1;
            }
            cells.push((s, style, n > 0));
        }
        // Trailing blanks go unless asked for; a blank with a background
        // still counts as written.
        let keep = if keep_spaces {
            used
        } else {
            cells.iter().rposition(|(s, st, has)| (*has && s != " ") || st.is_some_and(bg_set)).map_or(0, |i| i + 1)
        };
        for (s, style, _) in cells.into_iter().take(keep) {
            if opts.escapes {
                pen.change(style.unwrap_or_default(), &mut text);
            }
            text.extend_from_slice(s.as_bytes());
        }
        if joining {
            out.last_mut().expect("a line to join").extend(text);
        } else {
            out.push(text);
        }
        joining = opts.join && wrapped;
    }
    out
}

fn bg_set(s: libghostty_vt::style::Style) -> bool {
    s.bg_color != StyleColor::None
}

/// The SGR state written so far, so only changes are written (as tmux does,
/// across lines too).
#[derive(Default)]
struct Pen(Option<libghostty_vt::style::Style>);

impl Pen {
    fn change(&mut self, style: libghostty_vt::style::Style, out: &mut Vec<u8>) {
        let was = self.0.unwrap_or_default();
        if was == style {
            self.0 = Some(style);
            return;
        }
        self.0 = Some(style);
        out.extend_from_slice(sgr(&style).as_bytes());
    }
}

fn color(c: StyleColor, base: u8) -> Option<String> {
    match c {
        StyleColor::None => None,
        StyleColor::Palette(p) if p.0 < 8 => Some(format!("{}", base as u16 + p.0 as u16)),
        StyleColor::Palette(p) if p.0 < 16 => Some(format!("{}", base as u16 + 60 + p.0 as u16 - 8)),
        StyleColor::Palette(p) => Some(format!("{};5;{}", base + 8, p.0)),
        StyleColor::Rgb(c) => Some(format!("{};2;{};{};{}", base + 8, c.r, c.g, c.b)),
    }
}

/// A full SGR for a style: reset, then everything it sets.
fn sgr(s: &libghostty_vt::style::Style) -> String {
    let mut p = vec!["0".to_owned()];
    let flags = [
        (s.bold, "1"),
        (s.faint, "2"),
        (s.italic, "3"),
        (s.blink, "5"),
        (s.inverse, "7"),
        (s.invisible, "8"),
        (s.strikethrough, "9"),
        (s.overline, "53"),
    ];
    p.extend(flags.iter().filter(|(on, _)| *on).map(|(_, c)| (*c).to_owned()));
    match s.underline {
        Underline::None => {}
        Underline::Single => p.push("4".into()),
        Underline::Double => p.push("4:2".into()),
        Underline::Curly => p.push("4:3".into()),
        Underline::Dotted => p.push("4:4".into()),
        Underline::Dashed => p.push("4:5".into()),
        _ => p.push("4".into()),
    }
    p.extend(color(s.fg_color, 30));
    p.extend(color(s.bg_color, 40));
    format!("\x1b[{}m", p.join(";"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VtEngine;

    fn text(lines: Vec<Vec<u8>>) -> Vec<String> {
        lines.into_iter().map(|l| String::from_utf8(l).unwrap()).collect()
    }

    #[test]
    fn cursor_region_and_tab_stops() {
        let mut e = GhosttyEngine::mirror(40, 10, 100);
        e.feed(b"one\r\ntwo");
        assert_eq!(e.cursor(), (3, 1));
        assert_eq!(e.scroll_region(), (0, 9));
        assert_eq!(e.tab_stops(), vec![8, 16, 24, 32]);
        e.feed(b"\x1b[2;8r\x1b[3g\x1b[5G\x1bH\x1b[13G\x1bH\x1b[4;6H");
        assert_eq!(e.scroll_region(), (1, 7));
        assert_eq!(e.tab_stops(), vec![4, 12]);
        assert_eq!(e.cursor(), (5, 3));
    }

    #[test]
    fn the_alternate_screen_and_its_saved_cursor() {
        let mut e = GhosttyEngine::mirror(40, 5, 100);
        e.feed(b"$ vi\r\nabc\x1b[2;3H");
        assert_eq!(e.alt_saved_cursor(), None);
        assert_eq!(e.capture(&CaptureOpts { other: true, ..Default::default() }), None);
        e.feed(b"\x1b[?1049h\x1b[H\x1b[2Jhello\x1b[5;7H");
        assert_eq!(e.alt_saved_cursor(), Some((2, 1)));
        // The mirror itself is untouched.
        assert_eq!(e.cursor(), (6, 4));
        assert!(e.alt_screen());
        let shown = text(e.capture(&CaptureOpts::default()).unwrap());
        assert_eq!(shown, vec!["hello", "", "", "", ""]);
        let other = text(e.capture(&CaptureOpts { other: true, ..Default::default() }).unwrap());
        assert_eq!(other, vec!["$ vi", "abc", "", "", ""]);
    }

    #[test]
    fn a_snapshot_keeps_the_alternate_screens_saved_cursor() {
        let mut a = GhosttyEngine::mirror(40, 10, 100);
        a.feed(b"$ vi\r\n\x1b[?1049h\x1b[H\x1b[2Jhello\x1b[5;7H");
        let mut b = GhosttyEngine::mirror(40, 10, 100);
        b.feed(&a.snapshot());
        assert_eq!(b.alt_saved_cursor(), Some((0, 1)));
        assert_eq!(b.cursor(), (6, 4));
        b.feed(b"\x1b[?1049l");
        assert_eq!(b.cursor(), (0, 1), "the shell carries on below its command");
    }

    #[test]
    fn capture_takes_history_joins_wraps_and_keeps_spaces() {
        let mut e = GhosttyEngine::mirror(10, 3, 100);
        for i in 0..5 {
            e.feed(format!("line {i}\r\n").as_bytes());
        }
        e.feed(b"0123456789abc  ");
        assert_eq!(e.history_lines(), 4);
        let all = CaptureOpts { start: Line::At(-1000), ..Default::default() };
        assert_eq!(
            text(e.capture(&all).unwrap()),
            vec!["line 0", "line 1", "line 2", "line 3", "line 4", "0123456789", "abc"]
        );
        // The screen only (the default), three rows.
        assert_eq!(text(e.capture(&CaptureOpts::default()).unwrap()), vec!["line 4", "0123456789", "abc"]);
        let joined = CaptureOpts { join: true, ..Default::default() };
        assert_eq!(text(e.capture(&joined).unwrap()), vec!["line 4", "0123456789abc  "]);
        let edge = CaptureOpts { start: Line::Edge, end: Line::At(-3), ..Default::default() };
        assert_eq!(text(e.capture(&edge).unwrap()), vec!["line 0", "line 1"]);
    }

    #[test]
    fn capture_writes_style_changes() {
        let mut e = GhosttyEngine::mirror(20, 2, 100);
        e.feed(b"a\x1b[1;31mred\x1b[0m b\r\n\x1b[38;2;1;2;3mrgb");
        let lines = text(e.capture(&CaptureOpts { escapes: true, ..Default::default() }).unwrap());
        assert_eq!(lines, vec!["a\x1b[0;1;31mred\x1b[0m b", "\x1b[0;38;2;1;2;3mrgb"]);
    }
}
