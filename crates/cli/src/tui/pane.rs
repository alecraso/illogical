//! A terminal pane in the TUI: a local libghostty engine fed with the same
//! snapshot and output frames the web client gets, acked as it takes them
//! in (#52), and drawn into the frame from a cache of its cells.

use std::time::{Duration, Instant};

use illogical_proto::{Frame, FrameKind};
use illogical_vt::{CellStyle, Color, Cursor, GhosttyEngine, VtEngine};
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
            cache: Buffer::empty(Rect::new(0, 0, cols, rows)),
            cursor: None,
            mid_since: None,
            held: false,
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.engine.resize(cols.max(1), rows.max(1));
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
                self.resync = false;
                self.engine.feed(&data);
                let _ = self.engine.take_replies();
                self.offset = Some(f.offset);
                self.acked = f.offset;
                Took::Nothing
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
                if self.engine.scrolled_back() == 0 {
                    self.engine.feed(&f.data[at..]);
                } else {
                    // Keep the reader's place while new output arrives.
                    let back = self.engine.scrolled_back();
                    self.engine.feed(&f.data[at..]);
                    self.engine.scroll_to_bottom();
                    self.engine.scroll(-(back as isize));
                }
                let _ = self.engine.take_replies();
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
        let held = self.engine.mid_frame() && {
            let since = *self.mid_since.get_or_insert_with(Instant::now);
            since.elapsed() < MID_FRAME_MAX && self.cache.area == size
        };
        if !self.engine.mid_frame() {
            self.mid_since = None;
        }
        self.held = held;
        if !held {
            if self.cache.area != size {
                self.cache = Buffer::empty(size);
            }
            let cache = &mut self.cache;
            self.cursor = self.engine.cells(|x, y, text, st| {
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

/// Before a snapshot of the screen alone (#49): keep the scrollback, push
/// the screen into it under a rule marking what was skipped, and start the
/// screen and modes over.
fn skip_gap(rows: u16) -> String {
    format!(
        "\x1b[?1049l\x1b[0m\x1b[{rows};1H\r\n\x1b[2m── output skipped here; illogical tail has it ──\x1b[0m{}\x1b[!p\x1b[?7h\x1b[?1l\x1b[?66l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[<u\x1b]104\x1b\\\x1b[H\x1b[2J",
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
        (st.inverse, Modifier::REVERSED),
        (st.invisible, Modifier::HIDDEN),
        (st.strikethrough, Modifier::CROSSED_OUT),
        (st.underline, Modifier::UNDERLINED),
    ] {
        if on {
            m |= f;
        }
    }
    Style::default().fg(color(st.fg)).bg(color(st.bg)).add_modifier(m)
}
