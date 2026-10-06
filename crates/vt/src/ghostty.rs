//! [`VtEngine`] on libghostty-vt.
//!
//! libghostty's formatter does most of the snapshot work; `wire.rs` fills in
//! what it leaves out or gets wrong at the pinned commit. Checkpoints are
//! Ghostty's own snapshot format (GHOSTSNP), which needs no fix-ups.

use std::{cell::RefCell, rc::Rc, sync::OnceLock};

use libghostty_vt::{
    Terminal,
    fmt::{Format, Formatter, FormatterOptions},
    screen::{CellContentTag, Screen},
    selection::Selection,
    snapshot::Decoder,
    style::{RgbColor, StyleColor},
    terminal::{
        ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode, ModeKind, Point, PointCoordinate,
        PrimaryDeviceAttributes, SecondaryDeviceAttributes, TertiaryDeviceAttributes,
    },
};

use crate::{Capabilities, VtEngine};

/// Scrollback kept in memory per pane, as a byte budget: about 9.6k rows at
/// 200 columns, 24k at 80 (S9 measured 1.7 KB per 200-column row). The pane
/// log on disk keeps all of it (`history`, `tail`, `search`).
const SCROLLBACK_BYTES: usize = 16 * 1024 * 1024;
/// Largest unfinished escape sequence a checkpoint can carry.
const CONTINUATION_BYTES: usize = 1024 * 1024;

// Frozen (#504): checkpoints already on disk start with it.
const CHECKPOINT_MAGIC: &[u8] = b"ILLOGICAL-CKPT1\n";

/// Which engine wrote a checkpoint. GHOSTSNP has changed incompatibly
/// without bumping its version, and libghostty's `build_info` says
/// `0.1.0-dev` in every build, so a checkpoint is only trusted by an engine
/// that encodes a fixed canary terminal to the same bytes (spike S1/S5
/// follow-ups). A change that only touches decoding goes unnoticed; such a
/// checkpoint fails to decode and is discarded like any corrupt one.
pub fn engine_tag() -> String {
    static TAG: OnceLock<String> = OnceLock::new();
    TAG.get_or_init(|| {
        format!(
            "libghostty-rs@8953a74 ghostty@{} snap@{:016x}",
            libghostty_vt::build_info::version_string().unwrap_or("unknown"),
            fnv1a(&canary_snapshot())
        )
    })
    .clone()
}

/// GHOSTSNP of a small terminal that uses most of what the format carries:
/// both screens, scrollback, styles, a hyperlink, protection, charsets,
/// modes, margins, the saved cursor, title, pwd, palette and Kitty keyboard
/// flags, and an unfinished escape sequence.
fn canary_snapshot() -> Vec<u8> {
    let Ok(mut t) = Terminal::new(12, 4) else { return Vec::new() };
    let _ = t.set_continuation_max_bytes(64);
    t.vt_write(
        b"\x1b]2;canary\x1b\\\x1b]7;file:///c\x1b\\\x1b]4;1;rgb:12/34/56\x1b\\one\r\ntwo\r\nthree\r\nfour\r\nfive\r\n\
          \x1b[1;3;4:3;38;5;196;48;2;1;2;3mstyled\x1b[0m \x1b]8;id=a;https://x\x1b\\link\x1b]8;;\x1b\\\r\n\
          \x1b[1\"qkeep\x1b[0\"q\x1b(0qx\x1b(B\x1b[?2004h\x1b[?1000h\x1b[4h\x1b[2;3r\x1b[?6h\x1b[5 q\x1b[>1u\x1b7\
          \x1b[?1049h\x1b[2Jalt \xe2\x9c\x93 \xe4\xb8\xad\x1b[=3;1u\x1b[2;5H\x1b7\x1b[1;3",
    );
    t.encode_snapshot_alloc(None).ok().flatten().map(|b| b.to_vec()).unwrap_or_default()
}

/// FNV-1a, 64 bits: stable across Rust releases, unlike `DefaultHasher`.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointError {
    NotACheckpoint,
    OtherEngine(String),
    Corrupt,
}

impl std::fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotACheckpoint => f.write_str("not a checkpoint"),
            Self::OtherEngine(tag) => write!(f, "written by another engine ({tag})"),
            Self::Corrupt => f.write_str("corrupt checkpoint"),
        }
    }
}

impl std::error::Error for CheckpointError {}

/// Modes [`modes`] carries across. 47/1047/1049 are excluded: the snapshot
/// switches screens itself.
const DEC_MODES: &[u16] = &[
    1, 5, 6, 7, 12, 25, 45, 66, 69, 1000, 1002, 1003, 1004, 1005, 1006, 1007, 1015, 1016, 1035, 1036, 1039, 2004, 2026,
    2027, 2031, 2048,
];
const DEC_DEFAULT_ON: &[u16] = &[7, 25];
const ANSI_MODES: &[u16] = &[4, 20];

/// Default colors, reported in answer to OSC 10/11/12 queries. The web
/// client's theme uses the same values (`web/src/theme.ts`).
pub const DEFAULT_FG: RgbColor = RgbColor { r: 0xcd, g: 0xd6, b: 0xf4 };
pub const DEFAULT_BG: RgbColor = RgbColor { r: 0x1e, g: 0x1e, b: 0x2e };
pub const DEFAULT_CURSOR: RgbColor = RgbColor { r: 0xf5, g: 0xe0, b: 0xdc };

pub struct GhosttyEngine {
    /// Where a selection started (M32); before `term`, so it goes first.
    anchor: Option<Box<copy::Anchor>>,
    term: Terminal<'static, 'static>,
    replies: Rc<RefCell<Vec<u8>>>,
    caps: Capabilities,
    wire: wire::Cache,
    /// What a client drawing it reuses (M31), made on first use.
    view: Option<Box<view::View>>,
}

impl GhosttyEngine {
    /// An engine whose answers to programs are limited to what xterm.js
    /// (the web client) can draw.
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_capabilities(cols, rows, Capabilities::XTERM_JS)
    }

    pub fn with_capabilities(cols: u16, rows: u16, caps: Capabilities) -> Self {
        let term = Terminal::new(cols, rows).expect("libghostty terminal");
        Self::configure(term, caps)
    }

    /// Rebuild an engine from [`GhosttyEngine::checkpoint`] bytes. Fails if
    /// they are corrupt or were written by a different libghostty (the
    /// format has changed without a version bump; spike S5).
    pub fn from_checkpoint(bytes: &[u8]) -> Result<Self, CheckpointError> {
        let body = bytes.strip_prefix(CHECKPOINT_MAGIC).ok_or(CheckpointError::NotACheckpoint)?;
        let (tag, body) =
            body.split_at(body.iter().position(|b| *b == b'\n').ok_or(CheckpointError::NotACheckpoint)? + 1);
        if tag != format!("{}\n", engine_tag()).as_bytes() {
            return Err(CheckpointError::OtherEngine(String::from_utf8_lossy(&tag[..tag.len() - 1]).into_owned()));
        }
        let snap = zstd::decode_all(body).map_err(|_| CheckpointError::Corrupt)?;
        let decoder = Decoder::new_buf(&snap).map_err(|_| CheckpointError::Corrupt)?;
        let term: Terminal<'static, 'static> = decoder.decode().map_err(|_| CheckpointError::Corrupt)?;
        Ok(Self::configure(term, Capabilities::XTERM_JS))
    }

    /// Everything about the terminal in Ghostty's own snapshot format,
    /// zstd-compressed, behind a header naming the engine that wrote it.
    /// For checkpoints on disk, not for clients (xterm.js needs
    /// [`VtEngine::snapshot`]).
    /// Whether the program may be told the client speaks the kitty keyboard
    /// protocol (M31: while a client that does, the TUI, is attached).
    pub fn set_kitty_keyboard(&mut self, on: bool) {
        self.caps.kitty_keyboard = on;
    }

    pub fn checkpoint(&self) -> Vec<u8> {
        let snap = self.term.encode_snapshot_alloc(None).ok().flatten().map(|b| b.to_vec()).unwrap_or_default();
        let mut out = Vec::with_capacity(snap.len() / 20 + 64);
        out.extend_from_slice(CHECKPOINT_MAGIC);
        out.extend_from_slice(engine_tag().as_bytes());
        out.push(b'\n');
        out.extend(zstd::encode_all(&snap[..], 3).expect("zstd to memory"));
        out
    }

    fn configure(mut term: Terminal<'static, 'static>, caps: Capabilities) -> Self {
        term.set_scrollback_max_bytes(Some(SCROLLBACK_BYTES)).expect("scrollback limit");
        // Lets a checkpoint be taken in the middle of an escape sequence.
        term.set_continuation_max_bytes(CONTINUATION_BYTES).expect("continuation tracking");
        if !caps.kitty_graphics {
            // Disables the protocol: queries go unanswered, so programs don't
            // send images the client can't draw, and none are stored.
            term.set_kitty_image_storage_limit(0).expect("kitty image limit");
        }
        let replies = Rc::new(RefCell::new(Vec::new()));
        let sink = replies.clone();
        term.on_pty_write(move |_, data| sink.borrow_mut().extend_from_slice(data)).expect("pty write callback");
        term.on_device_attributes(|_| {
            Some(DeviceAttributes {
                primary: PrimaryDeviceAttributes::new(ConformanceLevel::VT220, &[DeviceAttributeFeature::ANSI_COLOR]),
                secondary: SecondaryDeviceAttributes {
                    device_type: DeviceType::VT220,
                    firmware_version: 1,
                    rom_cartridge: 0,
                },
                tertiary: TertiaryDeviceAttributes { unit_id: 0 },
            })
        })
        .expect("device attributes callback");
        term.on_xtversion(|_| Some(concat!("illogical ", env!("CARGO_PKG_VERSION")))).expect("xtversion callback");
        term.set_default_fg_color(Some(DEFAULT_FG))
            .and_then(|t| t.set_default_bg_color(Some(DEFAULT_BG)))
            .and_then(|t| t.set_default_cursor_color(Some(DEFAULT_CURSOR)))
            .expect("default colors");
        Self { anchor: None, term, replies, caps, wire: wire::Cache::default(), view: None }
    }

    fn format(&self, format: Format, extras: bool, modes: bool) -> Vec<u8> {
        self.format_history(format, extras, modes, None)
    }

    /// [`Self::format`], keeping at most `history` rows of scrollback above
    /// the active area (`None`: all of it).
    fn format_history(&self, format: Format, extras: bool, modes: bool, history: Option<usize>) -> Vec<u8> {
        let out = self.format_with(history, |o| {
            let o = o.with_format(format).with_modes(modes);
            if !extras {
                return o;
            }
            o.with_palette(true)
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
        });
        if extras { wire::move_tabstops_to_end(out) } else { out }
    }

    /// The formatter's VT output with `opts`, over the active area and at
    /// most `history` rows of scrollback above it (`None`: all of it).
    fn format_with(
        &self,
        history: Option<usize>,
        opts: impl for<'t, 's> FnOnce(FormatterOptions<'t, 's>) -> FormatterOptions<'t, 's>,
    ) -> Vec<u8> {
        // The formatter takes a range as a selection: from `history` rows up
        // to the active area's last cell.
        let have = self.term.scrollback_rows().unwrap_or(0);
        let from = history.filter(|h| *h < have).map(|h| (have - h) as u32);
        let (cols, rows) = self.size();
        let range = from.and_then(|y| {
            let start = self.term.grid_ref(Point::Screen(PointCoordinate { x: 0, y })).ok()?;
            let end = self
                .term
                .grid_ref(Point::Active(PointCoordinate {
                    x: cols.saturating_sub(1),
                    y: rows.saturating_sub(1).into(),
                }))
                .ok()?;
            Some(Selection::new(start, end, false))
        });
        let mut o = opts(FormatterOptions::new().with_format(Format::Vt));
        if let Some(range) = &range {
            o = o.with_selection(range);
        }
        let mut f = Formatter::new(&self.term, o).expect("formatter");
        f.format_alloc(None).expect("format").to_vec()
    }

    /// Non-default modes as CSI h/l.
    fn modes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for &m in DEC_MODES {
            let on = self.term.mode(Mode::new(m, ModeKind::Dec)).unwrap_or(false);
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

    /// Textless rows at the bottom of the active area (which the formatter
    /// drops), and bytes that repaint the backgrounds in them.
    fn trailing_rows(&self) -> (u16, Vec<u8>) {
        let (cols, rows) = self.size();
        let mut n = 0;
        let mut paint = Vec::new();
        for y in (0..rows).rev() {
            for x in 0..cols {
                let g = self.term.grid_ref(Point::Active(PointCoordinate { x, y: y.into() })).expect("grid ref");
                let cell = g.cell().expect("cell");
                if cell.has_text().unwrap_or(false) {
                    return (n, paint);
                }
                let bg = match (g.style().map(|s| s.bg_color).unwrap_or(StyleColor::None), cell.content_tag()) {
                    (StyleColor::Rgb(c), _) => Some(sgr_rgb(c)),
                    (StyleColor::Palette(p), _) => Some(format!("48;5;{}", p.0)),
                    (StyleColor::None, Ok(CellContentTag::BgColorRgb)) => cell.bg_color_rgb().ok().map(sgr_rgb),
                    (StyleColor::None, Ok(CellContentTag::BgColorPalette)) => {
                        cell.bg_color_palette().ok().map(|p| format!("48;5;{}", p.0))
                    }
                    _ => None,
                };
                if let Some(bg) = bg {
                    paint.extend_from_slice(format!("\x1b[{};{}H\x1b[0;{bg}m ", y + 1, x + 1).as_bytes());
                }
            }
            n += 1;
        }
        (n, paint)
    }

    #[cfg(test)]
    pub(crate) fn terminal(&self) -> &Terminal<'static, 'static> {
        &self.term
    }
}

fn sgr_rgb(c: RgbColor) -> String {
    format!("48;2;{};{};{}", c.r, c.g, c.b)
}

impl VtEngine for GhosttyEngine {
    fn feed(&mut self, bytes: &[u8]) {
        self.term.vt_write(bytes);
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        // Pixel size only matters for image protocols and size reports.
        self.term.resize(cols, rows, 8, 16).expect("resize");
    }

    fn size(&self) -> (u16, u16) {
        (self.term.cols().unwrap_or(0), self.term.rows().unwrap_or(0))
    }

    fn take_replies(&mut self) -> Vec<u8> {
        let raw = std::mem::take(&mut *self.replies.borrow_mut());
        if raw.is_empty() { raw } else { self.caps.filter_replies(&raw) }
    }

    fn snapshot(&mut self) -> Vec<u8> {
        self.snapshot_history(None)
    }

    fn snapshot_history(&mut self, history: Option<usize>) -> Vec<u8> {
        self.wire_snapshot(history)
    }

    fn screen_snapshot(&mut self) -> Vec<u8> {
        self.snapshot_history(Some(0))
    }

    fn plain_text(&self) -> String {
        String::from_utf8_lossy(&self.format(Format::Plain, false, false)).into_owned()
    }

    fn vt_text(&self) -> String {
        String::from_utf8_lossy(&self.format(Format::Vt, false, false)).into_owned()
    }

    fn html(&self) -> String {
        String::from_utf8_lossy(&self.format(Format::Html, false, false)).into_owned()
    }

    fn content_rows(&self) -> u16 {
        self.size().1 - self.trailing_rows().0
    }

    fn dec_mode(&self, mode: u16) -> bool {
        self.term.mode(Mode::new(mode, ModeKind::Dec)).unwrap_or(false)
    }

    fn alt_screen(&self) -> bool {
        self.term.active_screen().ok() == Some(Screen::Alternate)
    }

    fn title(&self) -> String {
        self.term.title().unwrap_or("").to_owned()
    }

    fn pwd(&self) -> String {
        self.term.pwd().unwrap_or("").to_owned()
    }
}

mod copy;
mod inspect;
mod view;
mod wire;
pub use copy::{Found, Unit};
pub use inspect::{CaptureOpts, Line};
pub use view::{CellStyle, Color, Cursor, CursorShape};

#[cfg(test)]
mod tests;
