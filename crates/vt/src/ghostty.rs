//! [`VtEngine`] on libghostty-vt.
//!
//! libghostty's formatter does most of the snapshot work. The code here fills
//! in what it leaves out or gets wrong at the pinned commit (all found in
//! spike S1, `spikes/s1-ghostty/README.md`):
//!
//! - It formats only the active screen, so while a full-screen app runs the
//!   primary screen and its scrollback are missing. [`GhosttyEngine::snapshot`]
//!   flips to the primary with mode 47, formats it, and flips back.
//! - Its tab stops extra moves the cursor and does not restore it.
//! - It emits neither the title nor the cursor shape.
//! - It drops textless rows at the bottom of the screen, which shifts the
//!   screen up on replay and loses backgrounds left by erases.

use std::{cell::RefCell, rc::Rc};

use libghostty_vt::{
    RenderState, Terminal,
    fmt::{Format, Formatter, FormatterOptions},
    render::CursorVisualStyle,
    screen::{CellContentTag, Screen},
    snapshot::Decoder,
    style::{RgbColor, StyleColor},
    terminal::{
        ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode, ModeKind, Point, PointCoordinate,
        PrimaryDeviceAttributes, SecondaryDeviceAttributes, TertiaryDeviceAttributes,
    },
};

use crate::{Capabilities, VtEngine};

/// Scrollback kept per pane, as a byte budget.
const SCROLLBACK_BYTES: usize = 64 * 1024 * 1024;
/// Largest unfinished escape sequence a checkpoint can carry.
const CONTINUATION_BYTES: usize = 1024 * 1024;

const CHECKPOINT_MAGIC: &[u8] = b"ILLOGICAL-CKPT1\n";

/// Which engine wrote a checkpoint. GHOSTSNP has changed incompatibly
/// without bumping its version, so a checkpoint is only trusted by the
/// exact libghostty it came from.
pub fn engine_tag() -> String {
    format!("libghostty-rs@8953a74 ghostty@{}", libghostty_vt::build_info::version_string().unwrap_or("unknown"))
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
    term: Terminal<'static, 'static>,
    replies: Rc<RefCell<Vec<u8>>>,
    caps: Capabilities,
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
        Self { term, replies, caps }
    }

    fn format(&self, format: Format, extras: bool, modes: bool) -> Vec<u8> {
        let mut o = FormatterOptions::new().with_format(format).with_modes(modes);
        if extras {
            o = o
                .with_palette(true)
                .with_scrolling_region(true)
                .with_tabstops(true)
                .with_pwd(true)
                .with_keyboard(true)
                .with_cursor(true)
                .with_style(true)
                .with_hyperlink(true)
                .with_protection(true)
                .with_kitty_keyboard(true)
                .with_charsets(true);
        }
        let mut f = Formatter::new(&self.term, o).expect("formatter");
        let out = f.format_alloc(None).expect("format").to_vec();
        if extras { move_tabstops_to_end(out) } else { out }
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

    /// Re-add the dropped trailing rows, unless the whole screen is blank.
    fn pad_rows(&self, out: &mut Vec<u8>) -> Vec<u8> {
        let (pad, paint) = self.trailing_rows();
        if pad < self.size().1 {
            for _ in 0..pad {
                out.extend_from_slice(b"\r\n");
            }
        }
        paint
    }

    /// Title, cursor shape, dropped trailing rows and cursor position.
    fn trailer(&self) -> Vec<u8> {
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
        let paint = self.pad_rows(&mut out);
        if !paint.is_empty() {
            // DECSC/DECRC keeps the pen the formatter set up for the cursor.
            out.extend_from_slice(b"\x1b7");
            out.extend(paint);
            out.extend_from_slice(b"\x1b8");
        }
        let (x, y) = (self.term.cursor_x().unwrap_or(0), self.term.cursor_y().unwrap_or(0));
        out.extend_from_slice(format!("\x1b[{};{}H", y + 1, x + 1).as_bytes());
        out
    }

    #[cfg(test)]
    pub(crate) fn terminal(&self) -> &Terminal<'static, 'static> {
        &self.term
    }
}

/// The formatter writes tab stops (`CSI 3 g`, then `CSI n G` + `ESC H` per
/// stop) before the screen content and leaves the cursor on the last stop,
/// so the content starts mid-line and wraps. Move that block to the end;
/// the trailer positions the cursor afterwards.
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
        let mut out = Vec::new();
        if self.term.active_screen().ok() == Some(Screen::Alternate) {
            // Where leaving the alternate screen puts the cursor back: the
            // cursor 1049h below saves.
            let saved = self.alt_saved_cursor();
            // Mode 47 switches screens without clearing or saving the cursor.
            self.term.vt_write(b"\x1b[?47l");
            out.extend(self.format(Format::Vt, false, false));
            self.pad_rows(&mut out);
            if let Some((x, y)) = saved {
                out.extend_from_slice(format!("\x1b[{};{}H", y + 1, x + 1).as_bytes());
            }
            self.term.vt_write(b"\x1b[?47h");
            // Entering through 47 set its flag; the app entered through 1049.
            let _ = self.term.set_mode(Mode::new(47, ModeKind::Dec), false);
            // 1049h saves the cursor just left in place but does not home it,
            // and the formatter assumes alt content starts at 1;1.
            out.extend_from_slice(b"\x1b[?1049h\x1b[H");
            out.extend(self.format(Format::Vt, true, false));
            out.extend(self.modes());
        } else {
            out.extend(self.format(Format::Vt, true, true));
        }
        out.extend(self.trailer());
        // The pending replies belong to the live stream, not the snapshot;
        // the screen flip above never produces any.
        out
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

mod inspect;
pub use inspect::{CaptureOpts, Line};

#[cfg(test)]
mod tests;
