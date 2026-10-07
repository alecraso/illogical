//! A terminal pane in the TUI: a local libghostty engine fed with the same
//! snapshot and output frames the web client gets, acked as it takes them
//! in (#52), and drawn into the frame from a cache of its cells.

use std::time::{Duration, Instant};

use arugula_proto::{Frame, FrameKind};
use arugula_vt::{CellStyle, Color, Cursor, GhosttyEngine, VtEngine};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{self, Modifier, Style},
};

/// Rows of history a pane keeps, and asks for in a snapshot (#49).
pub const SCROLLBACK: u32 = 10_000;
/// Ack about this often (bytes taken in); the daemon allows 512 KB.
const ACK_EVERY: u64 = 64 * 1024;
/// The longest a program's synchronized-output frame may hold drawing back.
const MID_FRAME_MAX: Duration = Duration::from_millis(250);

/// What a frame from the daemon means for the connection.
pub enum Took {
    Nothing,
    /// A snapshot: the engine is new.
    Fresh,
    /// Tell the daemon we've taken in everything before this.
    Ack(u64),
    /// Output after a gap we can't fill: attach again for a snapshot.
    Gap,
}

pub struct TermPane {
    pub engine: GhosttyEngine,
    /// Just past the last byte we have; `None` until the first snapshot.
    pub offset: Option<u64>,
    acked: u64,
    /// We asked for the screen alone after a resync: keep the scrollback
    /// when it comes (#49).
    pub resync: bool,
    /// Copy mode's deeper history (M32): the pane's output log replayed,
    /// shown instead of `engine` while copy mode reads it, and fed what
    /// arrives meanwhile. Dropped when copy mode ends.
    pub archive: Option<GhosttyEngine>,
    /// The offset the log is being read up to, and the output taken in
    /// since, for the archive.
    pub pending: Option<(u64, Vec<u8>)>,
    /// What was last drawn, for while the program is mid-frame.
    cache: Buffer,
    cursor: Option<Cursor>,
    mid_since: Option<Instant>,
    /// The last draw showed the cache: draw again soon.
    pub held: bool,
}

impl TermPane {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            engine: GhosttyEngine::mirror(cols.max(1), rows.max(1), SCROLLBACK as usize),
            offset: None,
            acked: 0,
            resync: false,
            archive: None,
            pending: None,
            cache: Buffer::empty(Rect::new(0, 0, cols, rows)),
            cursor: None,
            mid_since: None,
            held: false,
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.engine.resize(cols.max(1), rows.max(1));
        if let Some(a) = &mut self.archive {
            a.resize(cols.max(1), rows.max(1));
        }
    }

    /// What the pane shows: the archive while copy mode reads it.
    pub fn view(&self) -> &GhosttyEngine {
        self.archive.as_ref().unwrap_or(&self.engine)
    }

    pub fn view_mut(&mut self) -> &mut GhosttyEngine {
        self.archive.as_mut().unwrap_or(&mut self.engine)
    }

    /// The log up to `until`, where `engine` was when it was asked for, has
    /// come: replay it, then what arrived since.
    pub fn open_archive(&mut self, until: u64, log: &[u8], bytes: usize) {
        let Some((_, pending)) = self.pending.take_if(|(u, _)| *u == until) else { return };
        let (cols, rows) = self.engine.size();
        let mut a = GhosttyEngine::archive(cols, rows, bytes);
        a.feed(log);
        a.feed(&pending);
        let _ = a.take_replies();
        self.archive = Some(a);
    }

    pub fn close_archive(&mut self) {
        self.archive = None;
        self.pending = None;
    }

    pub fn take(&mut self, f: Frame) -> Took {
        match f.kind {
            FrameKind::Snapshot | FrameKind::SnapshotZstd => {
                let data = if f.kind == FrameKind::SnapshotZstd {
                    zstd::decode_all(&f.data[..]).unwrap_or_default()
                } else {
                    f.data
                };
                if self.resync {
                    let rows = self.engine.size().1;
                    self.engine.feed(skip_gap(rows).as_bytes());
                } else {
                    let (cols, rows) = self.engine.size();
                    self.engine = GhosttyEngine::mirror(cols, rows, SCROLLBACK as usize);
                }
                // The log offsets an archive was read up to no longer follow.
                self.close_archive();
                self.resync = false;
                self.engine.feed(&data);
                let _ = self.engine.take_replies();
                self.offset = Some(f.offset);
                self.acked = f.offset;
                Took::Fresh
            }
            FrameKind::Output => {
                let Some(have) = self.offset else { return Took::Nothing };
                if f.offset > have {
                    self.offset = None;
                    return Took::Gap;
                }
                let end = f.offset + f.data.len() as u64;
                if end <= have {
                    return Took::Nothing;
                }
                let at = (have - f.offset) as usize;
                let data = &f.data[at..];
                feed(&mut self.engine, data);
                if let Some(a) = &mut self.archive {
                    feed(a, data);
                }
                if let Some((_, p)) = &mut self.pending {
                    p.extend_from_slice(data);
                }
                self.offset = Some(end);
                if end - self.acked >= ACK_EVERY {
                    self.acked = end;
                    Took::Ack(end)
                } else {
                    Took::Nothing
                }
            }
            FrameKind::Input => Took::Nothing,
        }
    }

    /// Draw into `area` of `buf`; where the cursor is (in `buf`), if it
    /// shows.
    pub fn draw(&mut self, buf: &mut Buffer, area: Rect) -> Option<Cursor> {
        let (cols, rows) = self.engine.size();
        let size = Rect::new(0, 0, cols, rows);
        let mid = self.view().mid_frame();
        let held = mid && {
            let since = *self.mid_since.get_or_insert_with(Instant::now);
            since.elapsed() < MID_FRAME_MAX && self.cache.area == size
        };
        if !mid {
            self.mid_since = None;
        }
        self.held = held;
        if !held {
            if self.cache.area != size {
                self.cache = Buffer::empty(size);
            }
            let cache = &mut self.cache;
            let view = self.archive.as_mut().unwrap_or(&mut self.engine);
            self.cursor = view.cells(|x, y, text, st| {
                if let Some(c) = cache.cell_mut((x, y)) {
                    c.set_symbol(if text.is_empty() { " " } else { text }).set_style(style_of(st));
                }
            });
        }
        let w = area.width.min(cols);
        let h = area.height.min(rows);
        for y in 0..h {
            for x in 0..w {
                if let (Some(src), Some(dst)) = (self.cache.cell((x, y)), buf.cell_mut((area.x + x, area.y + y))) {
                    *dst = src.clone();
                }
            }
        }
        self.cursor.filter(|c| c.x < w && c.y < h).map(|c| Cursor { x: area.x + c.x, y: area.y + c.y, ..c })
    }
}

/// Output into an engine, keeping the reader's place if it's scrolled back.
fn feed(e: &mut GhosttyEngine, data: &[u8]) {
    let back = e.scrolled_back();
    e.feed(data);
    if back > 0 {
        e.scroll_to_bottom();
        e.scroll(-(back as isize));
    }
    let _ = e.take_replies();
}

/// Before a snapshot of the screen alone (#49): keep the scrollback, push
/// the screen into it under a rule marking what was skipped, and start the
/// screen and modes over.
fn skip_gap(rows: u16) -> String {
    format!(
        "\x1b[?1049l\x1b[0m\x1b[{rows};1H\r\n\x1b[2m── output skipped here; arugula tail has it ──\x1b[0m{}\x1b[!p\x1b[?7h\x1b[?1l\x1b[?66l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[<u\x1b]104\x1b\\\x1b[H\x1b[2J",
        "\r\n".repeat(rows as usize)
    )
}

pub fn color(c: Color) -> style::Color {
    match c {
        Color::Default => style::Color::Reset,
        Color::Palette(i) => style::Color::Indexed(i),
        Color::Rgb(r, g, b) => style::Color::Rgb(r, g, b),
    }
}

fn style_of(st: &CellStyle) -> Style {
    let mut m = Modifier::empty();
    for (on, f) in [
        (st.bold, Modifier::BOLD),
        (st.italic, Modifier::ITALIC),
        (st.faint, Modifier::DIM),
        (st.blink, Modifier::SLOW_BLINK),
        (st.invisible, Modifier::HIDDEN),
        (st.strikethrough, Modifier::CROSSED_OUT),
        (st.underline, Modifier::UNDERLINED),
        // Selected: the other way round from how it's drawn otherwise.
        (st.inverse != st.selected, Modifier::REVERSED),
    ] {
        if on {
            m |= f;
        }
    }
    Style::default().fg(color(st.fg)).bg(color(st.bg)).add_modifier(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(kind: FrameKind, offset: u64, data: &[u8]) -> Frame {
        Frame { kind, pane: 1, offset, data: data.to_vec() }
    }

    fn text(e: &GhosttyEngine) -> String {
        e.plain_text().trim_end().to_owned()
    }

    #[test]
    fn an_archive_replays_the_log_then_what_came_meanwhile() {
        let mut p = TermPane::new(20, 3);
        p.take(frame(FrameKind::Snapshot, 100, b"recent\r\n"));
        // A search asks for the log up to 100; output goes on arriving.
        p.pending = Some((100, Vec::new()));
        p.take(frame(FrameKind::Output, 100, b"later\r\n"));
        assert!(p.archive.is_none());
        p.open_archive(99, b"stale", 1 << 20);
        assert!(p.archive.is_none(), "a reply to another search");
        p.open_archive(100, b"old\r\nrecent\r\n", 1 << 20);
        assert_eq!(text(p.view()), "old\nrecent\nlater");
        assert_eq!(text(&p.engine), "recent\nlater", "the live engine is left alone");
        // It keeps up while it's shown.
        p.take(frame(FrameKind::Output, 107, b"more\r\n"));
        assert_eq!(text(p.view()), "old\nrecent\nlater\nmore");
        // A snapshot means the offsets it was read to no longer follow.
        p.take(frame(FrameKind::Snapshot, 500, b"fresh"));
        assert!(p.archive.is_none() && p.pending.is_none());
        assert_eq!(text(p.view()), "fresh");
    }
}
