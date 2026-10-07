//! A terminal as a client draws and drives it (M31: `arugula tui`): the
//! visible cells and cursor through libghostty's render state, the local
//! scrollback, and input encoded for the modes the pane's program has set
//! (cursor keys, the kitty keyboard protocol, mouse reporting, bracketed
//! paste, focus reports).

use libghostty_vt::{
    RenderState, focus, key, mouse, paste,
    render::{CellIterator, CursorVisualStyle, RowIterator},
    screen::{CellContentTag, CellWide},
    style::{StyleColor, Underline},
    terminal::{Mode, ModeKind, ScrollViewport},
};

use super::GhosttyEngine;

/// A color as the program asked for it: palette colors stay indexes, so the
/// client's own theme decides what they look like.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    Default,
    Palette(u8),
    Rgb(u8, u8, u8),
}

/// How a cell looks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CellStyle {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub underline: bool,
    /// Inside the selection (M32).
    pub selected: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// The cursor, when it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub shape: CursorShape,
    pub blink: bool,
    /// Set by the program (OSC 12); `None` is the client's own.
    pub color: Option<(u8, u8, u8)>,
}

/// What drawing reuses from frame to frame.
pub(super) struct View {
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    mouse: mouse::Encoder<'static>,
    text: String,
}

impl View {
    fn new() -> Self {
        Self {
            render: RenderState::new().expect("render state"),
            rows: RowIterator::new().expect("row iterator"),
            cells: CellIterator::new().expect("cell iterator"),
            mouse: mouse::Encoder::new().expect("mouse encoder"),
            text: String::new(),
        }
    }
}

fn color(c: StyleColor) -> Color {
    match c {
        StyleColor::None => Color::Default,
        StyleColor::Palette(p) => Color::Palette(p.0),
        StyleColor::Rgb(c) => Color::Rgb(c.r, c.g, c.b),
    }
}

impl GhosttyEngine {
    /// Every cell of what's visible (the viewport, so scrolled back when the
    /// client has scrolled), row by row: `f(x, y, text, style)`, with empty
    /// text for the second half of a wide character. Returns the cursor if
    /// it shows.
    pub fn cells(&mut self, mut f: impl FnMut(u16, u16, &str, &CellStyle)) -> Option<Cursor> {
        let view = self.view.get_or_insert_with(|| Box::new(View::new()));
        let View { render, rows, cells, text, .. } = &mut **view;
        let snap = render.update(&self.term).ok()?;
        {
            let mut ri = rows.update(&snap).ok()?;
            let mut y = 0;
            while let Some(row) = ri.next() {
                let sel = row.selection().ok().flatten().map(|s| s.start_x..=s.end_x);
                if let Ok(mut ci) = cells.update(row) {
                    let mut x: u16 = 0;
                    while let Some(cell) = ci.next() {
                        let raw = cell.raw_cell().ok();
                        let st = cell.style().unwrap_or_default();
                        let mut bg = color(st.bg_color);
                        // An erase leaves its background on the cell, not in
                        // a style.
                        if bg == Color::Default
                            && let Some(raw) = raw
                        {
                            bg = match raw.content_tag() {
                                Ok(CellContentTag::BgColorPalette) => {
                                    raw.bg_color_palette().map(|p| Color::Palette(p.0)).unwrap_or_default()
                                }
                                Ok(CellContentTag::BgColorRgb) => {
                                    raw.bg_color_rgb().map(|c| Color::Rgb(c.r, c.g, c.b)).unwrap_or_default()
                                }
                                _ => Color::Default,
                            };
                        }
                        let style = CellStyle {
                            fg: color(st.fg_color),
                            bg,
                            bold: st.bold,
                            italic: st.italic,
                            faint: st.faint,
                            blink: st.blink,
                            inverse: st.inverse,
                            invisible: st.invisible,
                            strikethrough: st.strikethrough,
                            underline: st.underline != Underline::None,
                            selected: sel.as_ref().is_some_and(|s| s.contains(&x)),
                        };
                        text.clear();
                        let spacer = matches!(raw.and_then(|r| r.wide().ok()), Some(CellWide::SpacerTail));
                        if !spacer {
                            let _ = cell.graphemes_utf8(text);
                            if text.is_empty() {
                                text.push(' ');
                            }
                        }
                        f(x, y, text, &style);
                        x += 1;
                    }
                }
                let _ = row.set_dirty(false);
                y += 1;
            }
        }
        if !snap.cursor_visible().unwrap_or(false) {
            return None;
        }
        let at = snap.cursor_viewport().ok()??;
        let shape = match snap.cursor_visual_style() {
            Ok(CursorVisualStyle::Underline) => CursorShape::Underline,
            Ok(CursorVisualStyle::Bar) => CursorShape::Bar,
            _ => CursorShape::Block,
        };
        let color = snap.colors().ok().and_then(|c| c.cursor).map(|c| (c.r, c.g, c.b));
        Some(Cursor { x: at.x, y: at.y, shape, blink: snap.cursor_blinking().unwrap_or(false), color })
    }

    /// Move the viewport `rows` up (negative) or down through the scrollback.
    pub fn scroll(&mut self, rows: isize) {
        self.term.scroll_viewport(ScrollViewport::Delta(rows));
    }

    /// Back to the live screen.
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_viewport(ScrollViewport::Bottom);
    }

    /// How many rows up the viewport is (0: the live screen).
    pub fn scrolled_back(&self) -> u64 {
        self.term.scrollbar().map(|s| s.total.saturating_sub(s.offset + s.len)).unwrap_or(0)
    }

    /// Whether the program asked for mouse reports.
    pub fn mouse_reporting(&self) -> bool {
        [9, 1000, 1002, 1003].iter().any(|m| self.dec_on(*m))
    }

    fn dec_on(&self, m: u16) -> bool {
        self.term.mode(Mode::new(m, ModeKind::Dec)).unwrap_or(false)
    }

    /// A key, as the program expects it: cursor and keypad modes,
    /// modifyOtherKeys and the kitty keyboard protocol, from what it has set.
    /// Alt is always Alt: the outer terminal already decided whether macOS
    /// Option typed a character or meant Alt, so libghostty's macOS default
    /// (Option makes text, no ESC prefix) mustn't decide again.
    pub fn encode_key(&self, event: &key::Event) -> Vec<u8> {
        let mut out = Vec::new();
        if let Ok(mut enc) = key::Encoder::new() {
            enc.set_options_from_terminal(&self.term);
            enc.set_macos_option_as_alt(key::OptionAsAlt::True);
            let _ = enc.encode_to_vec(event, &mut out);
        }
        out
    }

    /// A mouse event at cell `x`, `y` of the screen, as the program asked for
    /// reports (nothing if it didn't, or doesn't want this one).
    pub fn encode_mouse(&mut self, event: &mut mouse::Event, x: u16, y: u16, any_pressed: bool) -> Vec<u8> {
        let (cols, rows) = (self.term.cols().unwrap_or(1), self.term.rows().unwrap_or(1));
        let view = self.view.get_or_insert_with(|| Box::new(View::new()));
        // One pixel a cell: positions are cells.
        let size = mouse::EncoderSize {
            screen_width: cols as u32,
            screen_height: rows as u32,
            cell_width: 1,
            cell_height: 1,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        };
        view.mouse.set_options_from_terminal(&self.term).set_size(size).set_any_button_pressed(any_pressed);
        event.set_position(mouse::Position { x: x as f32 + 0.5, y: y as f32 + 0.5 });
        let mut out = Vec::new();
        let _ = view.mouse.encode_to_vec(event, &mut out);
        out
    }

    /// Pasted text: bracketed if the program asked for that, otherwise with
    /// newlines as Enter; control characters that could end a bracket are
    /// made harmless either way.
    pub fn encode_paste(&self, text: &str) -> Vec<u8> {
        let mut data = text.as_bytes().to_vec();
        let mut buf = vec![0u8; data.len() + 16];
        loop {
            match paste::encode(&mut data, self.dec_on(2004), &mut buf) {
                Ok(n) => {
                    buf.truncate(n);
                    return buf;
                }
                Err(libghostty_vt::Error::OutOfSpace { required }) => buf.resize(required, 0),
                Err(_) => return Vec::new(),
            }
        }
    }

    /// The window gained or lost focus, if the program asked to be told.
    pub fn encode_focus(&self, gained: bool) -> Vec<u8> {
        if !self.dec_on(1004) {
            return Vec::new();
        }
        let mut buf = [0u8; 8];
        let ev = if gained { focus::Event::Gained } else { focus::Event::Lost };
        ev.encode(&mut buf).map(|n| buf[..n].to_vec()).unwrap_or_default()
    }

    /// The program is in the middle of a frame (synchronized output, mode
    /// 2026): wait for the rest before drawing.
    pub fn mid_frame(&self) -> bool {
        self.dec_on(2026)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VtEngine;

    fn key(k: key::Key, mods: key::Mods, text: Option<&str>) -> key::Event<'static> {
        let mut e = key::Event::new().unwrap();
        e.set_action(key::Action::Press).set_key(k).set_mods(mods).set_utf8(text);
        if let Some(c) = text.and_then(|t| t.chars().next()) {
            e.set_unshifted_codepoint(c.to_ascii_lowercase());
        }
        e
    }

    #[test]
    fn keys_follow_the_programs_modes() {
        let mut e = GhosttyEngine::mirror(80, 24, 100);
        let up = key(key::Key::ArrowUp, key::Mods::empty(), None);
        assert_eq!(e.encode_key(&up), b"\x1b[A");
        e.feed(b"\x1b[?1h"); // application cursor keys
        assert_eq!(e.encode_key(&up), b"\x1bOA");
        // Alt is ESC-prefixed on macOS too (not Option's own character).
        let alt_b = key(key::Key::B, key::Mods::ALT, Some("b"));
        assert_eq!(e.encode_key(&alt_b), b"\x1bb");

        let shift_enter = key(key::Key::Enter, key::Mods::SHIFT, None);
        // As Ghostty sends it outside the kitty protocol.
        assert_eq!(e.encode_key(&shift_enter), b"\x1b[27;2;13~");
        e.feed(b"\x1b[>1u"); // kitty keyboard: disambiguate
        assert_eq!(e.encode_key(&shift_enter), b"\x1b[13;2u");
        let a = key(key::Key::A, key::Mods::empty(), Some("a"));
        assert_eq!(e.encode_key(&a), b"a");
        let ctrl_a = key(key::Key::A, key::Mods::CTRL, Some("a"));
        assert_eq!(e.encode_key(&ctrl_a), b"\x1b[97;5u");
    }

    #[test]
    fn mouse_reports_only_when_asked() {
        let mut e = GhosttyEngine::mirror(80, 24, 100);
        let mut ev = mouse::Event::new().unwrap();
        ev.set_action(mouse::Action::Press).set_button(Some(mouse::Button::Left));
        assert!(!e.mouse_reporting());
        assert!(e.encode_mouse(&mut ev, 4, 2, true).is_empty());
        e.feed(b"\x1b[?1000h\x1b[?1006h");
        assert!(e.mouse_reporting());
        assert_eq!(e.encode_mouse(&mut ev, 4, 2, true), b"\x1b[<0;5;3M");
    }

    #[test]
    fn paste_is_bracketed_when_asked() {
        let mut e = GhosttyEngine::mirror(80, 24, 100);
        assert_eq!(e.encode_paste("a\nb"), b"a\rb");
        e.feed(b"\x1b[?2004h");
        assert_eq!(e.encode_paste("a\nb"), b"\x1b[200~a\nb\x1b[201~");
        assert!(e.encode_focus(true).is_empty());
        e.feed(b"\x1b[?1004h");
        assert_eq!(e.encode_focus(true), b"\x1b[I");
    }

    #[test]
    fn cells_keep_palette_colors_and_scroll_back() {
        let mut e = GhosttyEngine::mirror(10, 3, 100);
        e.feed(b"\x1b[31mab\x1b[0m\x1b[48;2;1;2;3mc");
        let mut seen = Vec::new();
        let cursor = e.cells(|x, y, t, s| seen.push((x, y, t.to_owned(), s.fg, s.bg)));
        assert_eq!(seen.len(), 30);
        assert_eq!(seen[0], (0, 0, "a".into(), Color::Palette(1), Color::Default));
        assert_eq!(seen[2], (2, 0, "c".into(), Color::Default, Color::Rgb(1, 2, 3)));
        assert_eq!(cursor.map(|c| (c.x, c.y)), Some((3, 0)));

        for i in 0..10 {
            e.feed(format!("\r\nline {i}").as_bytes());
        }
        assert_eq!(e.scrolled_back(), 0);
        e.scroll(-2);
        assert_eq!(e.scrolled_back(), 2);
        let mut first = String::new();
        e.cells(|_, y, t, _| {
            if y == 0 {
                first.push_str(t)
            }
        });
        assert_eq!(first.trim_end(), "line 5", "the screen's top was line 7");
        e.scroll_to_bottom();
        assert_eq!(e.scrolled_back(), 0);
    }
}
