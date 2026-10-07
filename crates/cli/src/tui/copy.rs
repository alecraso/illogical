//! Copy mode (M32): selecting, searching and copying inside one pane. The
//! outer terminal sees the whole frame, so it can't select one pane's text
//! or scroll one pane's history; the TUI does it on the pane's own engine.
//!
//! - **The mouse:** drag selects (Shift-drag when the program takes the
//!   mouse), a double-click a word, a triple-click a line. Letting go copies.
//! - **The keyboard:** Ctrl-] `[` moves a cursor through the pane's history:
//!   `v`/`V` select, `y` copies, `/` and `?` search, `[` and `]` jump between
//!   prompts, `o` selects a command's output by its OSC 133 marks.
//!
//! Copying is OSC 52 to the outer terminal's clipboard, so it works over
//! ssh. A search that finds nothing in the history the TUI holds (10k rows)
//! reads the pane's output log from the daemon, replays it into an archive
//! terminal, and looks again there: the daemon's own terminal keeps only
//! 16 MiB (about 20k rows at 90 columns), its log 256 MiB. The archive is
//! shown until copy mode ends.

use std::time::{Duration, Instant};

use arugula_proto::PaneId;
use arugula_vt::{Unit, VtEngine};
use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::Rect,
};

use super::{
    app::{App, Ask, Mode, Prompt},
    keys,
};

/// The most of a pane's output log a search reads when it runs out of
/// history.
const LOG_BYTES: u64 = 32 << 20;
/// What an archive terminal keeps of it: about 80k rows at 90 columns.
const ARCHIVE_BYTES: usize = 64 << 20;
/// Clicks closer together than this count as a double or triple click.
const MULTI_CLICK: Duration = Duration::from_millis(500);

/// The keyboard cursor in copy mode.
pub struct Copy {
    pub pane: PaneId,
    pub x: u16,
    /// A screen row of what the pane shows (its archive once a search has
    /// read the log): 0 is the oldest line.
    pub row: u32,
    /// After `v` or `V`: moving grows the selection.
    pub selecting: bool,
    /// The selection is a command's output (`o`): copied with its last
    /// newline, as `arugula capture --last-command` prints it.
    pub output: bool,
    /// The last search, and whether it went up.
    pub last: Option<(String, bool)>,
    /// A search waiting for the pane's log to arrive.
    pub deepening: Option<(String, bool)>,
}

/// A pane's output log, read for a search.
pub struct Fetched {
    pane: PaneId,
    until: u64,
    log: anyhow::Result<Vec<u8>>,
}

/// The last click, to count double and triple clicks.
pub struct Click {
    pane: PaneId,
    x: u16,
    row: u32,
    at: Instant,
    count: u8,
}

impl App {
    // ---- the mouse ----

    /// A left press in a pane that isn't taking the mouse (or with Shift):
    /// start a selection. True if it did.
    pub fn select_press(&mut self, pane: PaneId, r: Rect, m: &MouseEvent) -> bool {
        let MouseEventKind::Down(MouseButton::Left) = m.kind else { return false };
        let Some(t) = self.panes.get_mut(&pane) else { return false };
        let view = t.view_mut();
        let (x, row) = (m.column - r.x, view.top_row() + (m.row - r.y) as u32);
        let count = match &self.click {
            Some(c) if c.pane == pane && c.x == x && c.row == row && c.at.elapsed() < MULTI_CLICK => c.count % 3 + 1,
            _ => 1,
        };
        self.click = Some(Click { pane, x, row, at: Instant::now(), count });
        let unit = match count {
            1 => Unit::Cell,
            2 => Unit::Word,
            _ => Unit::Line,
        };
        view.select_from(x, row, unit);
        self.selecting = Some((pane, r));
        if let Some(c) = &mut self.copy {
            c.output = false;
        }
        true
    }

    /// The mouse while a selection is being made: drags grow it (scrolling
    /// when they leave the pane), letting go copies it.
    pub fn select_mouse(&mut self, m: &MouseEvent) {
        let Some((pane, r)) = self.selecting else { return };
        let Some(t) = self.panes.get_mut(&pane) else {
            self.selecting = None;
            return;
        };
        match m.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                let view = t.view_mut();
                if m.row < r.y {
                    view.scroll(-1);
                } else if m.row >= r.bottom() {
                    view.scroll(1);
                }
                let x = m.column.clamp(r.x, r.right().saturating_sub(1)) - r.x;
                let y = m.row.clamp(r.y, r.bottom().saturating_sub(1)) - r.y;
                let row = view.top_row() + y as u32;
                view.select_to(x, row);
            }
            MouseEventKind::Up(_) => {
                self.selecting = None;
                self.copy_out(pane);
            }
            _ => {}
        }
    }

    /// Put the pane's selection on the clipboard.
    pub fn copy_out(&mut self, pane: PaneId) -> bool {
        let Some(mut text) = self.panes.get(&pane).and_then(|t| t.view().selection_text()) else { return false };
        if self.copy.as_ref().is_some_and(|c| c.pane == pane && c.output) && matches!(self.mode, Mode::Copy) {
            text.push('\n');
        }
        let lines = text.lines().count();
        self.say(if lines > 1 {
            format!("Copied {lines} lines")
        } else {
            format!("Copied {} characters", text.chars().count())
        });
        self.clip = Some(text);
        true
    }

    // ---- the keyboard ----

    /// Ctrl-] `[`: copy mode in the focused pane, its cursor where the
    /// program's is.
    pub fn enter_copy(&mut self) {
        let Some(pane) = self.focus else { return };
        let Some(t) = self.panes.get_mut(&pane) else { return };
        t.close_archive();
        let (cx, cy) = t.engine.cursor();
        let rows = t.engine.size().1 as u32;
        let row = t.engine.total_rows().saturating_sub(rows) + cy as u32;
        let top = t.engine.top_row();
        // Scrolled back: start at the bottom of what's shown.
        let row = if row >= top + rows { top + rows - 1 } else { row };
        let last = self.copy.take().and_then(|c| c.last);
        self.copy = Some(Copy { pane, x: cx, row, selecting: false, output: false, last, deepening: None });
        self.mode = Mode::Copy;
    }

    /// In copy mode, or its search prompt.
    pub fn in_copy(&self) -> bool {
        match &self.mode {
            Mode::Copy => true,
            Mode::Prompt(p) => matches!(p.ask, Ask::Search { .. }),
            _ => false,
        }
    }

    pub fn leave_copy(&mut self) {
        if let Some(c) = &self.copy
            && let Some(t) = self.panes.get_mut(&c.pane)
        {
            t.close_archive();
            t.engine.select_none();
            t.engine.scroll_to_bottom();
        }
        if let Some(c) = &mut self.copy {
            c.deepening = None;
        }
        self.mode = Mode::Normal;
    }

    pub fn copy_key(&mut self, k: KeyEvent) {
        self.mode = Mode::Copy;
        let Some(c) = &self.copy else { return self.leave_copy() };
        let pane = c.pane;
        let Some(t) = self.panes.get(&pane) else { return self.leave_copy() };
        let t = t.view();
        let (cols, rows) = t.size();
        let total = t.total_rows();
        let (x, row) = (c.x, c.row);
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let half = (rows / 2).max(1) as i64;
        let page = rows.saturating_sub(1).max(1) as i64;
        match k.code {
            _ if keys::is_menu_key(&k) => self.leave_copy(),
            KeyCode::Esc | KeyCode::Char('q') => self.leave_copy(),
            KeyCode::Char('h') | KeyCode::Left => self.copy_to(x.saturating_sub(1), row),
            KeyCode::Char('l') | KeyCode::Right => self.copy_to((x + 1).min(cols.saturating_sub(1)), row),
            KeyCode::Char('k') | KeyCode::Up => self.copy_by(-1),
            KeyCode::Char('j') | KeyCode::Down => self.copy_by(1),
            KeyCode::Char('u') if ctrl => self.copy_by(-half),
            KeyCode::Char('d') if ctrl => self.copy_by(half),
            KeyCode::Char('b') if ctrl => self.copy_by(-page),
            KeyCode::Char('f') if ctrl => self.copy_by(page),
            KeyCode::PageUp => self.copy_by(-page),
            KeyCode::PageDown => self.copy_by(page),
            KeyCode::Char('0') | KeyCode::Char('^') | KeyCode::Home => self.copy_to(0, row),
            KeyCode::Char('$') | KeyCode::End => self.copy_to(cols.saturating_sub(1), row),
            KeyCode::Char('g') => self.copy_to(0, 0),
            KeyCode::Char('G') => self.copy_to(0, total.saturating_sub(1)),
            KeyCode::Char(c @ ('v' | 'V' | ' ')) => {
                let was = self.copy.as_ref().is_some_and(|c| c.selecting);
                let t = self.panes.get_mut(&pane).expect("checked above").view_mut();
                if was && c != 'V' {
                    t.select_none();
                } else {
                    t.select_from(x, row, if c == 'V' { Unit::Line } else { Unit::Cell });
                    t.select_to(x, row);
                }
                if let Some(cp) = &mut self.copy {
                    cp.selecting = !was || c == 'V';
                    cp.output = false;
                }
            }
            KeyCode::Char('y') | KeyCode::Enter => {
                self.copy_out(pane);
                self.leave_copy();
            }
            KeyCode::Char(c @ ('/' | '?')) => {
                let back = c == '?';
                let text = self.copy.as_ref().and_then(|c| c.last.clone()).map(|(s, _)| s).unwrap_or_default();
                self.mode = Mode::Prompt(Prompt {
                    title: if back { "Search up".into() } else { "Search down".into() },
                    text,
                    ask: Ask::Search { back },
                });
            }
            KeyCode::Char(c @ ('n' | 'N')) => {
                if let Some((needle, back)) = self.copy.as_ref().and_then(|c| c.last.clone()) {
                    self.search(&needle, if c == 'n' { back } else { !back });
                }
            }
            KeyCode::Char(c @ ('[' | ']')) => {
                let prompts = t.prompt_rows();
                let to = if c == '[' {
                    prompts.into_iter().rev().find(|p| *p < row)
                } else {
                    prompts.into_iter().find(|p| *p > row)
                };
                match to {
                    Some(p) => self.copy_to(0, p),
                    None => self.say("No more prompts (they need shell integration)"),
                }
            }
            KeyCode::Char('o') => {
                // On a prompt, the output below it; in output, that output.
                let t = self.panes.get_mut(&pane).expect("checked above").view_mut();
                let on_prompt = t.prompt_rows().contains(&row);
                let found = (on_prompt && t.select_output(row + 1)) || t.select_output(row);
                if !found {
                    self.say("No command output here");
                }
                if let Some(cp) = &mut self.copy {
                    cp.selecting = false;
                    cp.output = found;
                }
            }
            _ => {}
        }
    }

    /// Move the copy cursor by rows.
    fn copy_by(&mut self, rows: i64) {
        let Some(c) = &self.copy else { return };
        let row = (c.row as i64 + rows).max(0) as u32;
        self.copy_to(c.x, row);
    }

    /// Move the copy cursor, keeping it in view and growing the selection.
    fn copy_to(&mut self, x: u16, row: u32) {
        let Some(c) = &mut self.copy else { return };
        let Some(t) = self.panes.get_mut(&c.pane) else { return };
        let t = t.view_mut();
        let row = row.min(t.total_rows().saturating_sub(1));
        c.x = x;
        c.row = row;
        t.show_row(row);
        if c.selecting {
            t.select_to(x, row);
        }
    }

    /// Find `needle` from the copy cursor, up or down, and land on it.
    pub fn search(&mut self, needle: &str, back: bool) {
        let Some(c) = &mut self.copy else { return };
        c.last = Some((needle.to_owned(), back));
        c.output = false;
        let pane = c.pane;
        let Some(t) = self.panes.get_mut(&pane) else { return };
        let deeper = t.archive.is_none() && t.pending.is_none();
        let view = t.view_mut();
        match view.find(needle, c.row, c.x, back) {
            Some(f) => {
                c.x = f.x;
                c.row = f.row;
                view.show_row(f.row);
                if c.selecting {
                    view.select_to(f.x, f.row);
                } else {
                    view.select_cells(f.row, f.x, f.x + f.width - 1);
                }
            }
            None if deeper && t.offset.is_some() => {
                // Not in what we hold: read the log up to where we are, and
                // look again in that (`take_fetched`).
                let until = t.offset.unwrap_or(0);
                t.pending = Some((until, Vec::new()));
                c.deepening = Some((needle.to_owned(), back));
                let from = until.saturating_sub(LOG_BYTES);
                let tx = self.fetched.0.clone();
                self.conn.get(format!("/api/panes/{pane}/tail?from={from}&until={until}"), move |log| {
                    let _ = tx.send(Fetched { pane, until, log });
                });
                self.say("Searching further back…");
            }
            None if t.pending.is_some() => self.say("Still reading the history…"),
            None => self.say(format!("Not found: {needle}")),
        }
    }

    /// Pane logs that have come back: open them as archives and finish the
    /// searches that waited for them.
    pub fn take_fetched(&mut self) {
        while let Ok(Fetched { pane, until, log }) = self.fetched.1.try_recv() {
            self.dirty = true;
            let Some(t) = self.panes.get_mut(&pane) else { continue };
            // Copy mode ended, or a snapshot came, while it was on its way.
            if t.pending.as_ref().is_none_or(|(u, _)| *u != until) {
                continue;
            }
            let Some((needle, back)) = self.copy.as_mut().filter(|c| c.pane == pane).and_then(|c| c.deepening.take())
            else {
                t.pending = None;
                continue;
            };
            let log = match log {
                Ok(log) => log,
                Err(e) => {
                    t.pending = None;
                    self.say(format!("Couldn't read the history: {e:#}"));
                    continue;
                }
            };
            t.open_archive(until, &log, ARCHIVE_BYTES);
            // From the bottom, where the search that missed began.
            let bottom = t.view().total_rows().saturating_sub(1);
            if let Some(c) = &mut self.copy {
                (c.row, c.x, c.selecting) = (bottom, 0, false);
            }
            if self.in_copy() {
                self.search(&needle, back);
            }
        }
    }

    /// A pane's engine was replaced by a snapshot: keep the copy cursor in
    /// it. A search waiting for the log is dropped with the archive.
    pub fn snapshot_taken(&mut self, pane: PaneId) {
        let Some(c) = &mut self.copy else { return };
        if c.pane != pane {
            return;
        }
        let Some(t) = self.panes.get_mut(&pane) else { return };
        t.engine.select_none();
        c.selecting = false;
        c.output = false;
        c.row = c.row.min(t.engine.total_rows().saturating_sub(1));
        if c.deepening.take().is_some() {
            self.say("The pane caught up from a snapshot: search again");
        }
    }
}

/// OSC 52: put `text` on the outer terminal's clipboard (over ssh too).
pub fn osc52(text: &str) -> Vec<u8> {
    let mut out = b"\x1b]52;c;".to_vec();
    out.extend(base64(text.as_bytes()).bytes());
    out.push(0x07);
    out
}

fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_and_osc52() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("héllo\n".as_bytes()), "aMOpbGxvCg==");
        assert_eq!(osc52("hi"), b"\x1b]52;c;aGk=\x07");
    }
}
