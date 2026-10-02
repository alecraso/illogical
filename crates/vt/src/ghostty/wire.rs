//! The wire snapshot: VT bytes that rebuild the terminal in a fresh one
//! (xterm.js on attach and resync, the tmux front end's mirror).
//!
//! libghostty's formatter writes most of it. What it leaves out or gets
//! wrong at the pinned commit is filled in here (spike S1,
//! `spikes/s1-ghostty/README.md`, and its follow-up,
//! `spikes/s1s5-followup/README.md`):
//!
//! - It formats only the active screen. Under a full-screen app the primary
//!   screen is formatted after flipping to it with mode 47, then the
//!   snapshot enters the alternate screen the way the app did (47, 1047 or
//!   1049). The primary's Kitty keyboard flags go with it.
//! - Its tab stops move the cursor without restoring it: they go last.
//! - It writes neither the title nor the cursor shape.
//! - It drops textless rows at the bottom of the screen. They are padded
//!   straight after the content, before the cursor move and scroll region
//!   that follow it (from there the newlines would start at the cursor, or
//!   be swallowed by the region), and their backgrounds are repainted.
//! - Its cursor move is not relative to the scroll region under origin mode.
//! - It has no saved cursor (DECSC, and what 1049 saved). A copy of the
//!   terminal through GHOSTSNP, up to READY, restores it, and the snapshot
//!   sets that state and saves it with `ESC 7` before setting the real one.
//! - It gives blank cells in a styled row the previous run's colors, and
//!   writes no per-cell hyperlinks or protection. So the snapshot is
//!   replayed into a scratch terminal, and any visible cell that differs is
//!   repainted.
//! - A pending wrap (the cursor past the last column) is lost. The cell
//!   under the cursor is printed again to recreate it.

use std::io;

use libghostty_vt::{
    RenderState, Terminal,
    error::Error,
    fmt::{Format, Formatter, FormatterOptions},
    render::CursorVisualStyle,
    screen::{Cell, CellContentTag, CellWide, GridRef, Screen},
    selection::Selection,
    snapshot::Decoder,
    style::{RgbColor, Style, StyleColor, Underline},
    terminal::{Mode, ModeKind, Point, PointCoordinate},
};

use super::GhosttyEngine;
use crate::VtEngine;

/// GHOSTSNP framing (`src/terminal/snapshot/record.zig`): an envelope of
/// magic and version, then records, each a header of tag (u16), payload
/// length (u32) and CRC (u32), all little-endian. READY ends what a
/// terminal needs to be usable; the scrollback follows it.
const ENVELOPE_LEN: usize = 10;
const RECORD_HEADER_LEN: usize = 10;
const READY_TAG: u16 = 5;

/// Leave the pen plain: SGR, protection, hyperlink and charset (G0 in GL).
const NEUTRAL: &[u8] = b"\x1b[0m\x1b[0\"q\x1b]8;;\x1b\\\x1b(B\x0f";
/// [`NEUTRAL`], and every charset designation and invocation back to the
/// default (the formatter writes only those that aren't).
const DEFAULTS: &[u8] = b"\x1b[0m\x1b[0\"q\x1b]8;;\x1b\\\x1b(B\x1b)B\x1b*B\x1b+B\x0f\x1b}";

/// What snapshots keep between them: the last one's GHOSTSNP up to READY
/// (screens, cursors, modes; not the scrollback), and what it gave. While
/// that's the same (more viewers, a resync, a reattach), so are these, and
/// the copy and the check are skipped.
#[derive(Default)]
pub(super) struct Cache {
    ready: Option<Vec<u8>>,
    saved: SavedCursors,
    patch: Vec<u8>,
}

/// A saved cursor (DECSC), as restoring it on a copy shows it.
#[derive(Clone)]
struct SavedCursor {
    x: u16,
    y: u16,
    origin: bool,
    /// Its pen, protection and charsets as the formatter writes them.
    pen: Vec<u8>,
    /// With a pending wrap: the column of the cell to print again, and its
    /// bytes, so that the cursor ends up past it.
    wrap: Option<(u16, Vec<u8>)>,
}

#[derive(Clone, Default)]
struct SavedCursors {
    /// The active screen's.
    active: Option<SavedCursor>,
    /// The primary screen's, while the alternate one is active.
    primary: Option<SavedCursor>,
}

impl GhosttyEngine {
    /// [`crate::VtEngine::snapshot_history`].
    pub(super) fn wire_snapshot(&mut self, history: Option<usize>) -> Vec<u8> {
        // Check the active screen with as little history as gives the same
        // bytes for it: the formatter's state at the first screen row only
        // depends on the rows it continues (a soft wrap). The alternate
        // screen has no history; what's above it is the primary's.
        let (check, whole) = if self.alt_screen() {
            (0, history == Some(0))
        } else {
            let want = self.history_rows(history);
            let check = want.min(self.wrapped_into_screen());
            (check, check == want)
        };
        let ready = ready_prefix(&mut self.term);
        // A continued first row looks the way history the prefix doesn't
        // cover makes it, so only checks without any are kept.
        if check == 0 && ready.is_some() && ready == self.wire.ready {
            let (saved, patch) = (self.wire.saved.clone(), std::mem::take(&mut self.wire.patch));
            let out = self.build(history, &saved, &patch);
            self.wire.patch = patch;
            return out;
        }
        let (saved, copy) = ready.as_deref().map(saved_cursors).unwrap_or_default();
        let first = self.build(Some(check), &saved, &[]);
        let patch = self.repaint(copy, &first);
        let out = if patch.is_empty() && whole { first } else { self.build(history, &saved, &patch) };
        self.wire = Cache { ready: ready.filter(|_| check == 0), saved, patch };
        out
    }

    /// Scrollback rows above the active screen that a snapshot keeping
    /// `history` of them writes.
    fn history_rows(&self, history: Option<usize>) -> usize {
        let have = self.term.scrollback_rows().unwrap_or(0);
        history.map_or(have, |h| h.min(have))
    }

    /// Snapshot bytes; `patch` repaints cells right after the active
    /// screen's content.
    fn build(&mut self, history: Option<usize>, saved: &SavedCursors, patch: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let alt = self.alt_screen();
        let (base, tail) = if alt {
            let modes = [47, 1047, 1049].map(|m| self.dec_mode(m));
            let enter = [1049, 1047, 47].into_iter().find(|m| self.dec_mode(*m)).unwrap_or(1049);
            // Mode 47 switches screens without clearing or saving the cursor.
            self.term.vt_write(b"\x1b[?47l");
            out.extend(self.format_history(Format::Vt, false, false, history));
            out.extend(self.pad(self.history_rows(history)));
            let kitty = self.term.kitty_keyboard_flags().map(|f| f.bits()).unwrap_or(0);
            if kitty != 0 {
                out.extend_from_slice(format!("\x1b[={kitty};1u").as_bytes());
            }
            if let Some(s) = &saved.primary {
                // Left as the current state: 1049h saves it again.
                out.extend(save(s, (0, 0)));
            }
            self.term.vt_write(b"\x1b[?47h");
            for (m, on) in [47, 1047, 1049].into_iter().zip(modes) {
                let _ = self.term.set_mode(Mode::new(m, ModeKind::Dec), on);
            }
            // The formatter assumes the content starts at 1;1 with no pen.
            out.extend_from_slice(format!("\x1b[?{enter}h\x1b[?6l\x1b[H").as_bytes());
            out.extend_from_slice(DEFAULTS);
            self.split(None, false)
        } else {
            self.split(history, true)
        };
        let region = region(&tail);
        out.extend(base);
        let insert = self.ansi_mode(4);
        if insert {
            out.extend_from_slice(b"\x1b[4l");
        }
        out.extend(self.pad(if alt { 0 } else { self.history_rows(history) }));
        out.extend_from_slice(patch);
        out.extend_from_slice(&tail);
        if let Some(s) = &saved.active {
            out.extend(save(s, region));
            out.extend_from_slice(DEFAULTS);
        }
        out.extend_from_slice(format!("\x1b[?6{}", if self.dec_mode(6) { 'h' } else { 'l' }).as_bytes());
        out.extend(self.modes());
        // The pen, charsets and cursor as they really are.
        out.extend_from_slice(&tail);
        out.extend(self.trailer(region, insert));
        // The pending replies belong to the live stream, not the snapshot;
        // the screen flip above never produces any.
        out
    }

    /// The formatter's output split where the content ends: (palette, modes
    /// and content; what it writes after the content, then the tab stops).
    /// The second part doesn't depend on the content, so it comes from a
    /// one-cell selection.
    fn split(&self, history: Option<usize>, modes: bool) -> (Vec<u8>, Vec<u8>) {
        let base = self.format_with(history, |o| o.with_palette(true).with_modes(modes));
        let after = extras(&self.term, |o| {
            o.with_scrolling_region(true)
                .with_tabstops(true)
                .with_pwd(true)
                .with_keyboard(true)
                .with_cursor(true)
                .with_style(true)
                .with_hyperlink(true)
                .with_protection(true)
                .with_kitty_keyboard(true)
                .with_charsets(true)
        });
        (base, after)
    }

    /// Re-add the textless rows at the bottom that the formatter drops,
    /// from the end of the content, and repaint their backgrounds.
    /// `history_rows`: the scrollback rows written above the screen.
    fn pad(&self, history_rows: usize) -> Vec<u8> {
        let (pad, paint) = self.trailing_rows();
        let mut out = Vec::new();
        // With nothing written at all, there is nothing to move down.
        if history_rows > 0 || pad < self.size().1 {
            out.extend(b"\r\n".repeat(pad.into()));
        }
        if !paint.is_empty() {
            out.extend(paint);
            out.extend_from_slice(b"\x1b[0m");
        }
        out
    }

    /// Title, cursor shape and cursor position (relative to the scroll
    /// region under origin mode), recreating a pending wrap.
    fn trailer(&self, (top, left): (u16, u16), insert: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let title = self.title();
        if !title.is_empty() {
            out.extend_from_slice(format!("\x1b]2;{title}\x1b\\").as_bytes());
        }
        let mut rs = RenderState::new().expect("render state");
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
        let (x, y) = self.cursor();
        let (top, left) = if self.dec_mode(6) { (top, left) } else { (0, 0) };
        let wrap = self.term.is_cursor_pending_wrap().unwrap_or(false).then(|| wrap_cell(&self.term, x, y)).flatten();
        let col = wrap.as_ref().map_or(x, |(c, _)| *c);
        out.extend_from_slice(
            format!("\x1b[{};{}H", y.saturating_sub(top) + 1, col.saturating_sub(left) + 1).as_bytes(),
        );
        if let Some((_, cell)) = wrap {
            if insert {
                out.extend_from_slice(b"\x1b[4l");
            }
            out.extend_from_slice(NEUTRAL);
            out.extend(cell);
            out.extend_from_slice(NEUTRAL);
            out.extend(pen(&self.term, true));
        }
        if insert {
            out.extend_from_slice(b"\x1b[4h");
        }
        out
    }

    /// Scrollback rows that the screen's first row continues (soft-wrapped
    /// into it).
    fn wrapped_into_screen(&self) -> usize {
        let continues = |p| self.term.grid_ref(p).and_then(|g| g.row()).and_then(|r| r.is_wrap_continuation());
        if !continues(Point::Active(PointCoordinate { x: 0, y: 0 })).unwrap_or(false) {
            return 0;
        }
        let have = self.term.scrollback_rows().unwrap_or(0);
        let mut n = 1;
        while n < have && continues(Point::History(PointCoordinate { x: 0, y: (have - n) as u32 })).unwrap_or(false) {
            n += 1;
        }
        n
    }

    /// Bytes that repaint the active screen's cells that `snapshot`,
    /// replayed into a fresh terminal, doesn't get right. `scratch`: a
    /// terminal of the same size to reset and replay into (else a new one).
    fn repaint(&self, scratch: Option<Terminal<'static, 'static>>, snapshot: &[u8]) -> Vec<u8> {
        let (cols, rows) = self.size();
        let scratch = match scratch {
            Some(mut t) if (t.cols().ok(), t.rows().ok()) == (Some(cols), Some(rows)) => {
                t.reset();
                Ok(t)
            }
            _ => Terminal::new(cols, rows),
        };
        let Ok(mut scratch) = scratch else { return Vec::new() };
        scratch.vt_write(snapshot);
        let mut out = Vec::new();
        for y in 0..rows {
            for x in 0..cols {
                let p = Point::Active(PointCoordinate { x, y: y.into() });
                let (Ok(want), Ok(got)) = (self.term.grid_ref(p), scratch.grid_ref(p)) else { continue };
                let (Ok(a), Ok(b)) = (want.cell(), got.cell()) else { continue };
                if a == b && plain(a) {
                    continue;
                }
                // Spacers go with the wide character they belong to.
                if matches!(a.wide(), Ok(CellWide::SpacerTail | CellWide::SpacerHead))
                    || Look::of(&want, a) == Look::of(&got, b)
                {
                    continue;
                }
                out.extend_from_slice(format!("\x1b[{};{}H", y + 1, x + 1).as_bytes());
                out.extend(print_cell(&want));
            }
        }
        if !out.is_empty() {
            out.extend_from_slice(NEUTRAL);
        }
        out
    }
}

/// Each screen's saved cursor, read on a copy of the terminal decoded from
/// its GHOSTSNP up to READY. Also the copy, which is costly to make.
fn saved_cursors(ready: &[u8]) -> (SavedCursors, Option<Terminal<'static, 'static>>) {
    let Ok(copy) = Decoder::new_buf(ready).and_then(|d| d.ready::<'static>()) else {
        return Default::default();
    };
    let mut t = copy.into_terminal();
    // CAN ends whatever sequence the copy's parser was in the middle of.
    t.vt_write(b"\x18\x1b8");
    let active = Some(read_saved(&t));
    let primary = (t.active_screen().ok() == Some(Screen::Alternate)).then(|| {
        // Leaving through 1049 restores the primary's saved cursor.
        t.vt_write(b"\x1b[?1049l");
        read_saved(&t)
    });
    (SavedCursors { active, primary }, Some(t))
}

/// The terminal's GHOSTSNP up to READY: everything but the scrollback,
/// which an encoder stopped there never writes. `None` if READY isn't
/// where the framing says (the format changed).
fn ready_prefix(term: &mut Terminal<'_, '_>) -> Option<Vec<u8>> {
    let mut w = UntilReady { buf: Vec::new(), next: ENVELOPE_LEN, done: false };
    let _ = term.encode_snapshot(&mut w);
    w.done.then_some(w.buf)
}

struct UntilReady {
    buf: Vec<u8>,
    /// Where the next record starts.
    next: usize,
    done: bool,
}

impl io::Write for UntilReady {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.done {
            return Err(io::Error::other("past READY"));
        }
        self.buf.extend_from_slice(data);
        while let Some(h) = self.buf.get(self.next..self.next + RECORD_HEADER_LEN) {
            let tag = u16::from_le_bytes([h[0], h[1]]);
            let end = self.next + RECORD_HEADER_LEN + u32::from_le_bytes([h[2], h[3], h[4], h[5]]) as usize;
            if self.buf.len() < end {
                break;
            }
            self.next = end;
            if tag == READY_TAG {
                self.buf.truncate(end);
                self.done = true;
                return Err(io::Error::other("READY reached"));
            }
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn read_saved(t: &Terminal<'_, '_>) -> SavedCursor {
    let (x, y) = (t.cursor_x().unwrap_or(0), t.cursor_y().unwrap_or(0));
    SavedCursor {
        x,
        y,
        origin: t.mode(Mode::new(6, ModeKind::Dec)).unwrap_or(false),
        pen: pen(t, false),
        wrap: t.is_cursor_pending_wrap().unwrap_or(false).then(|| wrap_cell(t, x, y)).flatten(),
    }
}

/// Set a saved cursor's state and save it with `ESC 7`; `region` is the
/// scroll region's top and left. Leaves that state current.
fn save(s: &SavedCursor, (top, left): (u16, u16)) -> Vec<u8> {
    let (top, left) = if s.origin { (top, left) } else { (0, 0) };
    let col = s.wrap.as_ref().map_or(s.x, |(c, _)| *c);
    let mut out = format!(
        "\x1b[?6{}\x1b[{};{}H",
        if s.origin { 'h' } else { 'l' },
        s.y.saturating_sub(top) + 1,
        col.saturating_sub(left) + 1
    )
    .into_bytes();
    out.extend_from_slice(NEUTRAL);
    if let Some((_, cell)) = &s.wrap {
        out.extend_from_slice(cell);
        out.extend_from_slice(NEUTRAL);
    }
    out.extend_from_slice(&s.pen);
    out.extend_from_slice(b"\x1b7");
    out
}

/// The cursor's column and the bytes that print its cell again, for a
/// cursor with a pending wrap (on the last column, or on the spacer of a
/// wide character there).
fn wrap_cell(t: &Terminal<'_, '_>, x: u16, y: u16) -> Option<(u16, Vec<u8>)> {
    let g = t.grid_ref(Point::Active(PointCoordinate { x, y: y.into() })).ok()?;
    if g.cell().and_then(Cell::wide).ok() == Some(CellWide::SpacerTail) && x > 0 {
        let g = t.grid_ref(Point::Active(PointCoordinate { x: x - 1, y: y.into() })).ok()?;
        return Some((x - 1, print_cell(&g)));
    }
    Some((x, print_cell(&g)))
}

/// What the formatter writes after the content (with `opts`'s extras),
/// then the tab stops. None of it depends on the content.
fn extras(
    t: &Terminal<'_, '_>,
    opts: impl for<'t, 's> Fn(FormatterOptions<'t, 's>) -> FormatterOptions<'t, 's>,
) -> Vec<u8> {
    let Ok(at) = t.grid_ref(Point::Active(PointCoordinate { x: 0, y: 0 })) else { return Vec::new() };
    let one = Selection::new(at.clone(), at, false);
    let vt = || FormatterOptions::new().with_format(Format::Vt).with_selection(&one);
    let content = format(t, vt());
    let with = move_tabstops_to_end(format(t, opts(vt())));
    with.strip_prefix(content.as_slice()).map(<[u8]>::to_vec).unwrap_or_default()
}

/// The cursor's pen (SGR), protection, charsets and, with `link`, its
/// hyperlink, as the formatter writes them.
fn pen(t: &Terminal<'_, '_>, link: bool) -> Vec<u8> {
    extras(t, |o| o.with_style(true).with_protection(true).with_charsets(true).with_hyperlink(link))
}

fn format(t: &Terminal<'_, '_>, opts: FormatterOptions<'_, '_>) -> Vec<u8> {
    Formatter::new(t, opts).and_then(|mut f| f.format_alloc(None).map(|b| b.to_vec())).unwrap_or_default()
}

/// The formatter writes tab stops (`CSI 3 g`, then `CSI n G` + `ESC H` per
/// stop) before the screen content and leaves the cursor on the last stop,
/// so the content starts mid-line and wraps. Move that block to the end;
/// the trailer positions the cursor afterwards.
pub(super) fn move_tabstops_to_end(out: Vec<u8>) -> Vec<u8> {
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

/// The scroll region's top and left (from 0) in the formatter's extras:
/// DECSTBM (`CSI t;b r`) and DECSLRM (`CSI l;r s`), written only when not
/// the whole screen.
fn region(extras: &[u8]) -> (u16, u16) {
    let mut found = (0, 0);
    for seq in extras.split(|b| *b == 0x1b).filter_map(|s| s.strip_prefix(b"[")) {
        let n = seq.iter().take_while(|c| c.is_ascii_digit() || **c == b';').count();
        let (params, fin) = (&seq[..n], seq.get(n));
        let Some(first) = params.split(|b| *b == b';').next().filter(|_| params.contains(&b';')) else { continue };
        let first = std::str::from_utf8(first).ok().and_then(|s| s.parse::<u16>().ok()).unwrap_or(1).saturating_sub(1);
        match fin {
            Some(b'r') => found.0 = first,
            Some(b's') => found.1 = first,
            _ => {}
        }
    }
    found
}

/// A cell without style, hyperlink or grapheme cluster: two such cells
/// look the same if they are the same.
fn plain(c: Cell) -> bool {
    !c.has_styling().unwrap_or(true)
        && !c.has_hyperlink().unwrap_or(true)
        && c.content_tag().ok() == Some(CellContentTag::Codepoint)
}

/// A cell as it looks: empty equals a space, and a background stored on the
/// cell (by an erase) equals the same background set through SGR.
#[derive(PartialEq, Eq)]
struct Look {
    text: Text,
    wide: Option<CellWide>,
    style: Style,
    protected: bool,
    link: Option<Vec<u8>>,
}

#[derive(PartialEq, Eq)]
enum Text {
    One(u32),
    Cluster(Vec<char>),
}

impl Look {
    fn of(g: &GridRef<'_>, c: Cell) -> Self {
        let mut style = g.style().unwrap_or_default();
        let text = match c.content_tag() {
            Ok(CellContentTag::CodepointGrapheme) => Text::Cluster(graphemes(g)),
            Ok(CellContentTag::BgColorPalette) => {
                if let Ok(p) = c.bg_color_palette() {
                    style.bg_color = StyleColor::Palette(p);
                }
                Text::One(' '.into())
            }
            Ok(CellContentTag::BgColorRgb) => {
                if let Ok(rgb) = c.bg_color_rgb() {
                    style.bg_color = StyleColor::Rgb(rgb);
                }
                Text::One(' '.into())
            }
            _ => Text::One(c.codepoint().ok().filter(|cp| *cp != 0).unwrap_or(' '.into())),
        };
        let link = c.has_hyperlink().unwrap_or(false).then(|| hyperlink(g));
        Self { text, wide: c.wide().ok(), style, protected: c.is_protected().unwrap_or(false), link }
    }
}

fn graphemes(g: &GridRef<'_>) -> Vec<char> {
    let mut buf = vec!['\0'; 16];
    loop {
        match g.graphemes(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                return buf;
            }
            Err(Error::OutOfSpace { required }) if required > buf.len() => buf.resize(required, '\0'),
            Err(_) => return Vec::new(),
        }
    }
}

fn hyperlink(g: &GridRef<'_>) -> Vec<u8> {
    let mut buf = vec![0; 512];
    loop {
        match g.hyperlink_uri(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                return buf;
            }
            Err(Error::OutOfSpace { required }) if required > buf.len() => buf.resize(required, 0),
            // The binding reports no size for this one: grow until it fits.
            Err(Error::OutOfSpace { .. }) if buf.len() < 1 << 16 => buf.resize(buf.len() * 4, 0),
            Err(_) => return Vec::new(),
        }
    }
}

/// Bytes that print the cell at `g` as it is, from a plain pen: SGR,
/// protection, hyperlink, text. Leaves the cell's SGR set.
fn print_cell(g: &GridRef<'_>) -> Vec<u8> {
    let Ok(c) = g.cell() else { return Vec::new() };
    let look = Look::of(g, c);
    let mut out = sgr(&look.style).into_bytes();
    if look.protected {
        out.extend_from_slice(b"\x1b[1\"q");
    }
    if let Some(link) = &look.link {
        out.extend_from_slice(b"\x1b]8;;");
        out.extend_from_slice(link);
        out.extend_from_slice(b"\x1b\\");
    }
    let mut text = String::new();
    match &look.text {
        Text::One(cp) => text.push(char::from_u32(*cp).unwrap_or(' ')),
        Text::Cluster(cs) => text.extend(cs),
    }
    out.extend_from_slice(text.as_bytes());
    if look.link.is_some() {
        out.extend_from_slice(b"\x1b]8;;\x1b\\");
    }
    if look.protected {
        out.extend_from_slice(b"\x1b[0\"q");
    }
    out
}

/// SGR that sets exactly `s`.
fn sgr(s: &Style) -> String {
    let mut p: Vec<String> = vec!["0".into()];
    let color = |base: u8, c: &StyleColor| match c {
        StyleColor::Palette(i) => Some(format!("{base};5;{}", i.0)),
        StyleColor::Rgb(RgbColor { r, g, b }) => Some(format!("{base};2;{r};{g};{b}")),
        StyleColor::None => None,
    };
    for (on, code) in [
        (s.bold, "1"),
        (s.faint, "2"),
        (s.italic, "3"),
        (s.blink, "5"),
        (s.inverse, "7"),
        (s.invisible, "8"),
        (s.strikethrough, "9"),
        (s.overline, "53"),
    ] {
        if on {
            p.push(code.into());
        }
    }
    match s.underline {
        Underline::None => {}
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
