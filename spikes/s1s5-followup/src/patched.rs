//! A copy of `crates/vt`'s `GhosttyEngine::snapshot()` with the fixes this
//! spike found, to check that they work before anyone changes the crate.
//! Differences from the crate (each marked FIX below):
//!
//! 1. Dropped trailing rows are padded right after the content, before the
//!    formatter's post-content extras (cursor CUP, DECSTBM/DECSLRM). The
//!    crate pads after them, so the CRLFs start from the cursor (not the
//!    content end) and can be swallowed by a scroll region. The split uses
//!    two formatter calls: palette+modes+content, and everything; the first
//!    is a prefix of the second.
//! 2. The final CUP is relative to the scroll region when origin mode is on.
//! 3. The saved cursor (DECSC) of each screen is carried: GHOSTSNP clones the
//!    terminal (exact, S5), `ESC 8` (or `?1049l` for the primary under an
//!    alt screen) on the clone reveals it, and the snapshot sets that state
//!    and saves it with `ESC 7` before setting the real state.
//! 4. Under an alt screen: the primary's Kitty keyboard flags are emitted,
//!    and the alt screen is entered with the mode the source used (47, 1047
//!    or 1049).
//! 5. Verify and patch: the snapshot is replayed into a scratch terminal and
//!    every visible cell that differs from the source is repainted. This
//!    catches the formatter giving blank cells the previous run's style.

use libghostty_vt::{
    RenderState, Terminal,
    fmt::{Format, Formatter, FormatterOptions},
    render::CursorVisualStyle,
    screen::{CellContentTag, CellWide, Screen},
    snapshot::Decoder,
    style::{RgbColor, Style, StyleColor, Underline},
    terminal::{Mode, ModeKind, Point, PointCoordinate},
};

type Term = Terminal<'static, 'static>;

const DEC_MODES: &[u16] = &[
    1, 5, 6, 7, 12, 25, 45, 66, 69, 1000, 1002, 1003, 1004, 1005, 1006, 1007, 1015, 1016, 1035, 1036, 1039, 2004, 2026,
    2027, 2031, 2048,
];
const DEC_DEFAULT_ON: &[u16] = &[7, 25];
const ANSI_MODES: &[u16] = &[4, 20];

pub struct Patched {
    pub term: Term,
    /// Cells repainted by the last snapshot (verify and patch).
    pub patched_cells: usize,
}

/// A saved cursor, read back from a clone.
struct Saved {
    x: u16,
    y: u16,
    origin: bool,
    /// Formatter's style/protection/charset extras for that cursor.
    extras: Vec<u8>,
    /// FIX 6: when the saved cursor has a pending wrap, the cell under it,
    /// rewritten to recreate the pending wrap before saving.
    wrap_cell: Option<Vec<u8>>,
}

/// SGR + grapheme that rewrite the cell at (x, y) as it is.
fn cell_bytes(t: &Term, x: u16, y: u16) -> Vec<u8> {
    let g = t.grid_ref(Point::Active(PointCoordinate { x, y: y.into() })).unwrap();
    let mut buf = ['\0'; 16];
    let k = g.graphemes(&mut buf).unwrap_or(0);
    let text: String = if k == 0 { " ".into() } else { buf[..k].iter().collect() };
    format!("{}{text}", sgr(&g.style().unwrap())).into_bytes()
}

fn fmt(t: &Term, o: FormatterOptions) -> Vec<u8> {
    Formatter::new(t, o).unwrap().format_alloc(None).unwrap().to_vec()
}

fn base_opts(modes: bool) -> FormatterOptions<'static, 'static> {
    FormatterOptions::new().with_format(Format::Vt).with_palette(true).with_modes(modes)
}

fn full_opts(modes: bool) -> FormatterOptions<'static, 'static> {
    base_opts(modes)
        .with_scrolling_region(true)
        .with_tabstops(true)
        .with_pwd(true)
        .with_keyboard(true)
        .with_cursor(true)
        .with_style(true)
        .with_hyperlink(true)
        .with_protection(true)
        .with_kitty_keyboard(true)
        .with_charsets(true)
}

fn move_tabstops_to_end(out: Vec<u8>) -> Vec<u8> {
    let Some(start) = out.windows(4).position(|w| w == b"\x1b[3g") else {
        return out;
    };
    let mut end = start + 4;
    loop {
        let rest = &out[end..];
        let Some(digits) = rest.strip_prefix(b"\x1b[").map(|r| r.iter().take_while(|c| c.is_ascii_digit()).count())
        else {
            break;
        };
        if digits == 0 || !rest[2 + digits..].starts_with(b"G\x1bH") {
            break;
        }
        end += 2 + digits + 3;
    }
    let mut moved = Vec::with_capacity(out.len());
    moved.extend_from_slice(&out[..start]);
    moved.extend_from_slice(&out[end..]);
    moved.extend_from_slice(&out[start..end]);
    moved
}

/// FIX 1: (palette + modes + content, post-content extras + tab stops).
fn split(t: &Term, modes: bool) -> (Vec<u8>, Vec<u8>) {
    let base = fmt(t, base_opts(modes));
    let full = move_tabstops_to_end(fmt(t, full_opts(modes)));
    assert!(full.starts_with(&base), "formatter output is not prefix-stable");
    let tail = full[base.len()..].to_vec();
    (base, tail)
}

/// Region top and left (0-based) from the DECSTBM/DECSLRM in a tail.
fn region(tail: &[u8]) -> (u16, u16) {
    let s = String::from_utf8_lossy(tail);
    let find = |fin: char| -> u16 {
        let mut top = 0;
        for part in s.split("\x1b[").skip(1) {
            let n: String = part.chars().take_while(|c| c.is_ascii_digit() || *c == ';').collect();
            if part[n.len()..].starts_with(fin) && n.contains(';') {
                top = n.split(';').next().unwrap().parse::<u16>().unwrap_or(1).saturating_sub(1);
            }
        }
        top
    };
    (find('r'), find('s'))
}

/// FIX 3: the saved cursor, found by restoring it on a clone.
fn saved(snap: &[u8], probe: &[u8]) -> Saved {
    let mut inc = Decoder::new_buf(snap).unwrap().ready::<'static>().unwrap();
    let t = inc.terminal_mut();
    t.vt_write(probe);
    let plain = fmt(t, FormatterOptions::new().with_format(Format::Vt));
    let with = fmt(t, FormatterOptions::new().with_format(Format::Vt).with_style(true).with_protection(true).with_charsets(true));
    assert!(with.starts_with(&plain));
    let (x, y) = (t.cursor_x().unwrap(), t.cursor_y().unwrap());
    Saved {
        x,
        y,
        origin: t.mode(Mode::new(6, ModeKind::Dec)).unwrap(),
        extras: with[plain.len()..].to_vec(),
        wrap_cell: t.is_cursor_pending_wrap().unwrap().then(|| cell_bytes(t, x, y)),
    }
}

/// Set the saved state and save it; leaves that state current.
fn save_block(s: &Saved, (top, left): (u16, u16)) -> Vec<u8> {
    let (y, x) = if s.origin { (s.y - top, s.x - left) } else { (s.y, s.x) };
    let mut out = format!("\x1b[?6{}\x1b[{};{}H", if s.origin { 'h' } else { 'l' }, y + 1, x + 1).into_bytes();
    if let Some(c) = &s.wrap_cell {
        out.extend_from_slice(c);
    }
    out.extend_from_slice(b"\x1b[0m\x1b[0\"q");
    out.extend_from_slice(&s.extras);
    out.extend_from_slice(b"\x1b7");
    // Undo what the saved state set that the real state's extras would not
    // (they only emit non-defaults).
    out.extend_from_slice(b"\x1b[0\"q");
    if s.extras.windows(2).any(|w| w[0] == 0x1b && b"()*+".contains(&w[1])) {
        out.extend_from_slice(b"\x1b(B\x1b)B\x1b*B\x1b+B");
    }
    if s.extras.contains(&0x0e) {
        out.push(0x0f);
    }
    out
}

pub fn sgr(s: &Style) -> String {
    let mut p: Vec<String> = vec!["0".into()];
    let color = |base: u8, c: &StyleColor| match c {
        StyleColor::Palette(i) => Some(format!("{base};5;{}", i.0)),
        StyleColor::Rgb(RgbColor { r, g, b }) => Some(format!("{base};2;{r};{g};{b}")),
        StyleColor::None => None,
    };
    for (on, code) in [(s.bold, "1"), (s.faint, "2"), (s.italic, "3"), (s.blink, "5"), (s.inverse, "7"), (s.invisible, "8"), (s.strikethrough, "9"), (s.overline, "53")] {
        if on {
            p.push(code.into());
        }
    }
    match s.underline {
        Underline::None => {}
        Underline::Single => p.push("4".into()),
        Underline::Double => p.push("4:2".into()),
        Underline::Curly => p.push("4:3".into()),
        Underline::Dotted => p.push("4:4".into()),
        Underline::Dashed => p.push("4:5".into()),
        _ => p.push("4".into()),
    }
    p.extend(color(38, &s.fg_color));
    p.extend(color(48, &s.bg_color));
    p.extend(color(58, &s.underline_color));
    format!("\x1b[{}m", p.join(";"))
}

/// Bytes that repaint visible cells of `want` that differ in `got`.
fn patches(want: &Term, got: &Term) -> (usize, Vec<u8>) {
    let (cols, rows) = (want.cols().unwrap(), want.rows().unwrap());
    let mut out = Vec::new();
    let mut n = 0;
    for y in 0..rows {
        for x in 0..cols {
            if crate::cell_key(want, x, y) == crate::cell_key(got, x, y) {
                continue;
            }
            let g = want.grid_ref(Point::Active(PointCoordinate { x, y: y.into() })).unwrap();
            let cell = g.cell().unwrap();
            if cell.wide().ok() == Some(CellWide::SpacerTail) {
                continue;
            }
            let mut style = g.style().unwrap();
            if matches!(style.bg_color, StyleColor::None) {
                match cell.content_tag().unwrap() {
                    CellContentTag::BgColorPalette => style.bg_color = StyleColor::Palette(cell.bg_color_palette().unwrap()),
                    CellContentTag::BgColorRgb => style.bg_color = StyleColor::Rgb(cell.bg_color_rgb().unwrap()),
                    _ => {}
                }
            }
            let mut buf = ['\0'; 16];
            let k = g.graphemes(&mut buf).unwrap_or(0);
            let text: String = if k == 0 { " ".into() } else { buf[..k].iter().collect() };
            let prot = cell.is_protected().unwrap_or(false);
            let mut link = [0u8; 4096];
            let ln = g.hyperlink_uri(&mut link).unwrap_or(0);
            out.extend(format!("\x1b[{};{}H{}{}", y + 1, x + 1, sgr(&style), if prot { "\x1b[1\"q" } else { "" }).bytes());
            if ln > 0 {
                out.extend_from_slice(b"\x1b]8;;");
                out.extend_from_slice(&link[..ln]);
                out.extend_from_slice(b"\x1b\\");
            }
            out.extend(text.bytes());
            if ln > 0 {
                out.extend_from_slice(b"\x1b]8;;\x1b\\");
            }
            if prot {
                out.extend_from_slice(b"\x1b[0\"q");
            }
            n += 1;
        }
    }
    (n, out)
}

impl Patched {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { term: crate::new_term(cols, rows), patched_cells: 0 }
    }

    pub fn feed(&mut self, b: &[u8]) {
        self.term.vt_write(b);
    }

    pub fn resize(&mut self, c: u16, r: u16) {
        self.term.resize(c, r, 8, 16).unwrap();
    }

    fn dec(&self, m: u16) -> bool {
        self.term.mode(Mode::new(m, ModeKind::Dec)).unwrap_or(false)
    }

    fn modes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for &m in DEC_MODES {
            let on = self.dec(m);
            if on != DEC_DEFAULT_ON.contains(&m) {
                out.extend_from_slice(format!("\x1b[?{m}{}", if on { 'h' } else { 'l' }).as_bytes());
            }
        }
        for &m in ANSI_MODES {
            if self.term.mode(Mode::new(m, ModeKind::Ansi)).unwrap_or(false) {
                out.extend_from_slice(format!("\x1b[{m}h").as_bytes());
            }
        }
        out
    }

    /// Dropped trailing rows as CRLFs, and their backgrounds (absolute CUPs).
    fn pad_and_paint(&self) -> Vec<u8> {
        let (cols, rows) = (self.term.cols().unwrap(), self.term.rows().unwrap());
        let mut n = 0;
        let mut paint = Vec::new();
        'rows: for y in (0..rows).rev() {
            for x in 0..cols {
                let g = self.term.grid_ref(Point::Active(PointCoordinate { x, y: y.into() })).unwrap();
                let cell = g.cell().unwrap();
                if cell.has_text().unwrap_or(false) {
                    break 'rows;
                }
                let bg = match (g.style().map(|s| s.bg_color).unwrap_or(StyleColor::None), cell.content_tag()) {
                    (c @ (StyleColor::Rgb(_) | StyleColor::Palette(_)), _) => Some(c),
                    (StyleColor::None, Ok(CellContentTag::BgColorRgb)) => cell.bg_color_rgb().ok().map(StyleColor::Rgb),
                    (StyleColor::None, Ok(CellContentTag::BgColorPalette)) => {
                        cell.bg_color_palette().ok().map(StyleColor::Palette)
                    }
                    _ => None,
                };
                if let Some(bg) = bg {
                    let s = Style { bg_color: bg, ..Style::default() };
                    paint.extend(format!("\x1b[{};{}H{} ", y + 1, x + 1, sgr(&s)).bytes());
                }
            }
            n += 1;
        }
        let mut out = Vec::new();
        if n < rows {
            for _ in 0..n {
                out.extend_from_slice(b"\r\n");
            }
        }
        out.extend(paint);
        out
    }

    fn trailer(&self, (top, left): (u16, u16)) -> Vec<u8> {
        let mut out = Vec::new();
        let title = self.term.title().unwrap_or("");
        if !title.is_empty() {
            out.extend_from_slice(format!("\x1b]2;{title}\x1b\\").as_bytes());
        }
        let mut rs = RenderState::new().unwrap();
        if let Ok(snap) = rs.update(&self.term) {
            let blink = snap.cursor_blinking().unwrap_or(false);
            let n = match snap.cursor_visual_style() {
                Ok(CursorVisualStyle::Underline) => 4 - blink as u8,
                Ok(CursorVisualStyle::Bar) => 6 - blink as u8,
                Ok(_) => 2 - blink as u8,
                Err(_) => 0,
            };
            out.extend_from_slice(format!("\x1b[{n} q").as_bytes());
        }
        // FIX 2: CUP is relative to the region in origin mode.
        let (mut x, mut y) = (self.term.cursor_x().unwrap(), self.term.cursor_y().unwrap());
        if self.dec(6) {
            y -= top;
            x -= left;
        }
        out.extend_from_slice(format!("\x1b[{};{}H", y + 1, x + 1).as_bytes());
        // FIX 6: recreate a pending wrap by rewriting the last cell, then
        // put the pen back.
        if self.term.is_cursor_pending_wrap().unwrap_or(false) {
            let (cx, cy) = (self.term.cursor_x().unwrap(), self.term.cursor_y().unwrap());
            out.extend(cell_bytes(&self.term, cx, cy));
            out.extend(sgr(&self.term.cursor_style().unwrap()).bytes());
        }
        out
    }

    /// The snapshot, with `patch` inserted after the active screen's
    /// content (before its extras).
    fn build(&mut self, snap: &[u8], patch: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        if self.term.active_screen().unwrap() == Screen::Alternate {
            // FIX 4: enter the alt screen the way the source did.
            let mode = [1049, 1047, 47].into_iter().find(|&m| self.dec(m)).unwrap_or(1049);
            let saved_primary = saved(snap, b"\x1b[?1049l");
            let saved_alt = saved(snap, b"\x1b8");
            self.term.vt_write(b"\x1b[?47l");
            out.extend(fmt(&self.term, FormatterOptions::new().with_format(Format::Vt)));
            out.extend(self.pad_and_paint());
            let kitty = self.term.kitty_keyboard_flags().unwrap();
            if !kitty.is_empty() {
                out.extend(format!("\x1b[={};1u", kitty.bits()).bytes());
            }
            out.extend(save_block(&saved_primary, (0, 0)));
            self.term.vt_write(b"\x1b[?47h");
            let _ = self.term.set_mode(Mode::new(47, ModeKind::Dec), false);
            for m in [47, 1047, 1049] {
                let _ = self.term.set_mode(Mode::new(m, ModeKind::Dec), m == mode);
            }
            // Reset the pen after the switch (1049h must save the saved pen).
            out.extend(format!("\x1b[?{mode}h\x1b[H\x1b[0m").bytes());
            let (base, tail) = split(&self.term, false);
            let reg = region(&tail);
            out.extend(base);
            out.extend(self.pad_and_paint());
            out.extend_from_slice(patch);
            out.extend(&tail);
            out.extend(self.modes());
            out.extend(save_block(&saved_alt, reg));
            out.extend(format!("\x1b[?6{}", if self.dec(6) { 'h' } else { 'l' }).bytes());
            out.extend(&tail);
            out.extend(self.trailer(reg));
        } else {
            let s = saved(snap, b"\x1b8");
            let (base, tail) = split(&self.term, true);
            let reg = region(&tail);
            out.extend(base);
            out.extend(self.pad_and_paint());
            out.extend_from_slice(patch);
            out.extend(&tail);
            out.extend(save_block(&s, reg));
            out.extend(format!("\x1b[?6{}", if self.dec(6) { 'h' } else { 'l' }).bytes());
            out.extend(&tail);
            out.extend(self.trailer(reg));
        }
        out
    }

    pub fn snapshot(&mut self) -> Vec<u8> {
        let snap = self.term.encode_snapshot_alloc(None).unwrap().unwrap().to_vec();
        let first = self.build(&snap, &[]);
        // FIX 5: verify and patch the visible cells.
        let (c, r) = (self.term.cols().unwrap(), self.term.rows().unwrap());
        let mut scratch = crate::new_term(c, r);
        scratch.vt_write(&first);
        let (n, patch) = patches(&self.term, &scratch);
        self.patched_cells = n;
        if n == 0 { first } else { self.build(&snap, &patch) }
    }
}
