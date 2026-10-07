//! `arugula tui` (M31): the daemon's tabs and splits in the terminal
//! you're in, as herdr does, with a sidebar of sessions, tabs and what needs
//! you. A client like the web one: it attaches each pane of the shown tab,
//! keeps a libghostty engine per pane, and draws them with ratatui. Ctrl-]
//! is the menu key.

mod agent;
mod app;
mod conn;
mod copy;
mod draw;
mod keys;
mod pane;

use std::{
    io::Write,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use arugula_vt::{Cursor, CursorShape};
use ratatui::{
    Terminal,
    crossterm::{
        cursor::SetCursorStyle,
        event::{
            self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
            EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
            PushKeyboardEnhancementFlags,
        },
        execute,
        terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
    },
    layout::Position,
    prelude::CrosstermBackend,
};

use crate::http::Target;
use app::App;
use conn::{Conn, Wake};

/// A cursor's shape, blink and color: what the outer terminal is told.
type CursorLook = (CursorShape, bool, Option<(u8, u8, u8)>);

/// At most this often.
const FRAME: Duration = Duration::from_millis(16);

/// The outer terminal as we found it, put back however we leave.
struct Screen {
    kitty: bool,
}

impl Screen {
    fn enter() -> anyhow::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut out = std::io::stdout();
        execute!(out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste, EnableFocusChange)?;
        // Keys as the kitty protocol reports them, where the terminal has it:
        // Shift+Enter, Ctrl+I and Tab and the rest told apart.
        let kitty = terminal::supports_keyboard_enhancement().unwrap_or(false);
        if kitty {
            execute!(
                out,
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                )
            )?;
        }
        Ok(Self { kitty })
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        if self.kitty {
            let _ = execute!(out, PopKeyboardEnhancementFlags);
        }
        let _ = execute!(
            out,
            DisableFocusChange,
            DisableBracketedPaste,
            DisableMouseCapture,
            SetCursorStyle::DefaultUserShape,
            LeaveAlternateScreen
        );
        // The cursor's own color, if a pane set one.
        let _ = out.write_all(b"\x1b]112\x1b\\\x1b[?25h");
        let _ = out.flush();
        let _ = terminal::disable_raw_mode();
    }
}

pub fn run(target: &Target, session: Option<String>) -> anyhow::Result<i32> {
    let (mut wake, waker) = Wake::pair()?;
    let (err_tx, err_rx) = mpsc::channel();
    let (conn, hello) = Conn::open(target, err_tx, waker.clone())?;

    let screen = Screen::enter()?;
    let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    term.clear()?;

    // Keys, mouse and resizes, read on their own thread.
    let (ev_tx, ev_rx) = mpsc::channel();
    {
        let waker = waker.clone();
        thread::Builder::new().name("tui-input".into()).spawn(move || {
            while let Ok(ev) = event::read() {
                if ev_tx.send(ev).is_err() {
                    break;
                }
                waker.wake();
            }
        })?;
    }

    let mut app = App::new(conn, hello, err_rx, session);
    let stats = std::env::var_os("ARUGULA_TUI_STATS");
    let mut shown_cursor: Option<CursorLook> = None;
    let mut last_draw = Instant::now() - FRAME;
    while !app.quit {
        let wait = if app.dirty { FRAME.saturating_sub(last_draw.elapsed()) } else { Duration::from_millis(500) };
        let (sock, woke) = wake.wait(Some(app.conn.stream()), wait)?;
        if sock {
            let mut got = Vec::new();
            app.conn.read(|m| got.push(m))?;
            for m in got {
                app.take(m);
            }
        }
        if woke {
            wake.drain();
        }
        while let Ok(ev) = ev_rx.try_recv() {
            app.event(ev);
        }
        app.take_errors();
        app.take_fetched();
        app.take_conversations();
        if app.toast.as_ref().is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(6)) {
            // Redraw once it should be gone.
            app.dirty |= last_draw.elapsed() > Duration::from_secs(1);
        }
        if app.dirty && last_draw.elapsed() >= FRAME && !app.quit {
            let t0 = Instant::now();
            let mut cursor: Option<Cursor> = None;
            term.draw(|f| {
                let b0 = Instant::now();
                cursor = draw::draw(&mut app, f);
                if let Some(c) = cursor {
                    f.set_cursor_position(Position::new(c.x, c.y));
                }
                app.stats.build.push(b0.elapsed());
            })?;
            if let Some(c) = cursor {
                let want = (c.shape, c.blink, c.color);
                if shown_cursor != Some(want) {
                    set_cursor(c)?;
                    shown_cursor = Some(want);
                }
            }
            app.stats.draw.push(t0.elapsed());
            if let Some(text) = app.clip.take() {
                let mut out = std::io::stdout();
                out.write_all(&copy::osc52(&text))?;
                out.flush()?;
            }
            last_draw = Instant::now();
            // A pane held mid-frame (synchronized output) draws when it ends.
            app.dirty = app.panes.values().any(|p| p.held);
        }
        app.conn.flush()?;
    }
    drop(term);
    drop(screen);
    if let Some(path) = stats {
        std::fs::write(path, report(&mut app.stats))?;
    }
    Ok(0)
}

/// The focused pane's cursor shape and color, on the outer terminal.
fn set_cursor(c: Cursor) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    let style = match (c.shape, c.blink) {
        (CursorShape::Block, true) => SetCursorStyle::BlinkingBlock,
        (CursorShape::Block, false) => SetCursorStyle::SteadyBlock,
        (CursorShape::Underline, true) => SetCursorStyle::BlinkingUnderScore,
        (CursorShape::Underline, false) => SetCursorStyle::SteadyUnderScore,
        (CursorShape::Bar, true) => SetCursorStyle::BlinkingBar,
        (CursorShape::Bar, false) => SetCursorStyle::SteadyBar,
    };
    execute!(out, style)?;
    match c.color {
        Some((r, g, b)) => write!(out, "\x1b]12;rgb:{r:02x}/{g:02x}/{b:02x}\x1b\\")?,
        None => out.write_all(b"\x1b]112\x1b\\")?,
    }
    out.flush()
}

fn report(s: &mut app::Stats) -> String {
    fn pct(v: &mut [Duration], p: f64) -> f64 {
        if v.is_empty() {
            return 0.0;
        }
        v.sort();
        v[((v.len() - 1) as f64 * p) as usize].as_secs_f64() * 1e3
    }
    format!(
        "frames {}\nbytes_in {}\nbuild_ms p50 {:.3} p99 {:.3} max {:.3}\ndraw_ms p50 {:.3} p99 {:.3} max {:.3}\n",
        s.build.len(),
        s.bytes,
        pct(&mut s.build, 0.5),
        pct(&mut s.build, 0.99),
        pct(&mut s.build, 1.0),
        pct(&mut s.draw, 0.5),
        pct(&mut s.draw, 0.99),
        pct(&mut s.draw, 1.0),
    )
}
