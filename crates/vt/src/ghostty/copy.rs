//! Selecting and finding text in a terminal a client draws (M32: copy mode
//! in `arugula tui`). The selection is the terminal's own, so it stays on
//! its text as output scrolls it, and drawing marks its cells. Rows here are
//! screen rows: 0 is the oldest line of scrollback, and a viewport's rows are
//! `top_row()` onward.

use libghostty_vt::{
    fmt::Format,
    screen::{CellWide, GridRef, RowSemanticPrompt, TrackedGridRef},
    selection::{FormatOptions, SelectLineOptions, SelectWordOptions, Selection},
    terminal::{Point, PointCoordinate, PointSpace, ScrollViewport},
};

use super::GhosttyEngine;
use crate::VtEngine;

/// What a selection grows by: a click and drag, a double-click, a
/// triple-click (or `v` and `V` in copy mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Cell,
    Word,
    Line,
}

/// Where a selection started, kept on its text as the terminal scrolls.
pub(super) struct Anchor {
    at: TrackedGridRef,
    unit: Unit,
}

/// A match of [`GhosttyEngine::find`]: its row, first column and width in
/// cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub row: u32,
    pub x: u16,
    pub width: u16,
}

impl GhosttyEngine {
    /// A recording terminal for reading back history (a pane's output log
    /// replayed): as [`Self::mirror`], but its scrollback is limited to
    /// `bytes` of libghostty's pages rather than to lines.
    pub fn archive(cols: u16, rows: u16, bytes: usize) -> Self {
        let mut e = Self::mirror(cols, rows, 0);
        e.term.set_scrollback_max_lines(None).expect("scrollback lines");
        e.term.set_scrollback_max_bytes(Some(bytes)).expect("scrollback limit");
        e
    }

    /// The screen row at the top of the viewport.
    pub fn top_row(&self) -> u32 {
        self.term.scrollbar().map(|s| s.offset as u32).unwrap_or(0)
    }

    /// Rows there are, scrollback and screen.
    pub fn total_rows(&self) -> u32 {
        self.term.total_rows().unwrap_or(0) as u32
    }

    /// Show `row`, with a few rows above it when it's off screen.
    pub fn show_row(&mut self, row: u32) {
        let rows = self.size().1 as u32;
        let top = self.top_row();
        if row >= top && row < top + rows {
            return;
        }
        let want = row.saturating_sub(rows / 3);
        self.term.scroll_viewport(ScrollViewport::Row(want as usize));
    }

    fn at(&self, x: u16, row: u32) -> Option<GridRef<'_>> {
        let cols = self.size().0;
        self.term.grid_ref(Point::Screen(PointCoordinate { x: x.min(cols.saturating_sub(1)), y: row })).ok()
    }

    /// The word or line at a point, or the cell itself.
    fn unit_at(&self, x: u16, row: u32, unit: Unit) -> Option<Selection<'_>> {
        let g = self.at(x, row)?;
        match unit {
            Unit::Cell => Some(Selection::new(g.clone(), g, false)),
            Unit::Word => self
                .term
                .select_word(SelectWordOptions::new(g.clone()))
                .ok()
                .flatten()
                .or_else(|| Some(Selection::new(g.clone(), g, false))),
            Unit::Line => self
                .term
                .select_line(SelectLineOptions::new(g.clone()).with_semantic_prompt_boundary(false))
                .ok()
                .flatten()
                .or_else(|| Some(Selection::new(g.clone(), g, false))),
        }
    }

    /// Start a selection at `x`, `row`. A cell selects nothing until
    /// [`Self::select_to`]; a word or line is selected at once.
    pub fn select_from(&mut self, x: u16, row: u32, unit: Unit) {
        let Ok(at) = self.term.track_grid_ref(Point::Screen(PointCoordinate { x, y: row })) else { return };
        self.anchor = Some(Box::new(Anchor { at, unit }));
        if unit == Unit::Cell {
            let _ = self.term.set_selection(None);
        } else {
            self.select_to(x, row);
        }
    }

    /// Grow the selection from where it started to `x`, `row`, by its unit.
    pub fn select_to(&mut self, x: u16, row: u32) {
        let Some(a) = &self.anchor else { return };
        let Some((ax, arow)) = a.at.point(PointSpace::Screen).ok().flatten().map(|p| (p.x, p.y)) else {
            return;
        };
        let unit = a.unit;
        let (Some(from), Some(to)) = (self.unit_at(ax, arow, unit), self.unit_at(x, row, unit)) else { return };
        let forward = (row, x) >= (arow, ax);
        let sel = if forward {
            Selection::new(from.start(), to.end(), false)
        } else {
            Selection::new(to.start(), from.end(), false)
        };
        let _ = self.term.set_selection(Some(&sel));
    }

    /// Select cells `x0..=x1` of one row (a search match).
    pub fn select_cells(&mut self, row: u32, x0: u16, x1: u16) {
        self.anchor = None;
        let (Some(a), Some(b)) = (self.at(x0, row), self.at(x1, row)) else { return };
        let sel = Selection::new(a, b, false);
        let _ = self.term.set_selection(Some(&sel));
    }

    /// Select the output of the command at `row` (between its OSC 133
    /// marks), as Ghostty does. False if there's no command output there.
    pub fn select_output(&mut self, row: u32) -> bool {
        self.anchor = None;
        let Some(g) = self.at(0, row) else { return false };
        match self.term.select_output(g).ok().flatten() {
            Some(sel) => self.term.set_selection(Some(&sel)).is_ok(),
            None => false,
        }
    }

    pub fn select_none(&mut self) {
        self.anchor = None;
        let _ = self.term.set_selection(None);
    }

    pub fn has_selection(&self) -> bool {
        self.term.selection().ok().flatten().is_some()
    }

    /// The selection as text, as `arugula capture` writes it: soft-wrapped
    /// lines joined, no trailing blanks.
    pub fn selection_text(&self) -> Option<String> {
        let o = FormatOptions::new().with_emit_format(Format::Plain).with_unwrap(true).with_trim(true);
        let bytes = self.term.format_selection_alloc(None, o).ok().flatten()?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        (!text.is_empty()).then_some(text)
    }

    /// The first row of each prompt (OSC 133), oldest first.
    pub fn prompt_rows(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for row in 0..self.total_rows() {
            let Some(g) = self.at(0, row) else { continue };
            if matches!(g.row().and_then(|r| r.semantic_prompt()), Ok(RowSemanticPrompt::Prompt)) {
                out.push(row);
            }
        }
        out
    }

    /// The nearest match of `needle` after (or with `back`, before) column
    /// `x` of `row`, wrapping around. Lower-case needles ignore case. A
    /// match doesn't run across rows.
    pub fn find(&self, needle: &str, row: u32, x: u16, back: bool) -> Option<Found> {
        if needle.is_empty() {
            return None;
        }
        let fold = !needle.chars().any(char::is_uppercase);
        let needle: Vec<char> = if fold { needle.to_lowercase().chars().collect() } else { needle.chars().collect() };
        // Plain text without unwrapping is one line per row, from row 0.
        let text = self.plain_text();
        let lines: Vec<Vec<char>> = text
            .split('\n')
            .map(|l| if fold { l.to_lowercase().chars().collect() } else { l.chars().collect() })
            .collect();
        let hits = |line: &[char]| -> Vec<usize> {
            if line.len() < needle.len() {
                return vec![];
            }
            (0..=line.len() - needle.len()).filter(|i| line[*i..*i + needle.len()] == needle[..]).collect()
        };
        let n = lines.len() as u32;
        if n == 0 {
            return None;
        }
        let row = row.min(n - 1);
        // The starting row first (past the cursor), then the others in turn,
        // then the starting row again (before the cursor).
        let col = |r: u32, i: usize| self.column(r, i);
        for step in 0..=n {
            let r = if back { (row + n * 2 - step) % n } else { (row + step) % n };
            let found = hits(&lines[r as usize]);
            let pick = if step == 0 {
                if back {
                    found.into_iter().rev().find(|i| col(r, *i) < x)
                } else {
                    found.into_iter().find(|i| col(r, *i) > x)
                }
            } else if back {
                found.into_iter().next_back()
            } else {
                found.into_iter().next()
            };
            if let Some(i) = pick {
                let x0 = col(r, i);
                let x1 = col(r, i + needle.len());
                return Some(Found { row: r, x: x0, width: x1.saturating_sub(x0).max(1) });
            }
        }
        None
    }

    /// The column of the `i`th character of a row's text (wide characters
    /// take two).
    fn column(&self, row: u32, i: usize) -> u16 {
        let cols = self.size().0;
        let mut seen = 0;
        for x in 0..cols {
            let Some(g) = self.at(x, row) else { break };
            let wide = g.cell().and_then(|c| c.wide()).unwrap_or(CellWide::Narrow);
            if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
                continue;
            }
            if seen == i {
                return x;
            }
            seen += 1;
        }
        cols
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "\x1b]133;A\x07$ \x1b]133;B\x07";

    fn shell(e: &mut GhosttyEngine, cmd: &str, out: &str) {
        e.feed(format!("{P}{cmd}\r\n\x1b]133;C\x07{out}\x1b]133;D;0\x07").as_bytes());
    }

    #[test]
    fn drag_word_and_line_selections() {
        let mut e = GhosttyEngine::mirror(20, 6, 100);
        e.feed(b"one two three\r\nfour 0123456789abcdefghij");
        assert!(!e.has_selection());
        e.select_from(4, 0, Unit::Cell);
        assert!(!e.has_selection(), "a click alone selects nothing");
        e.select_to(2, 1);
        assert_eq!(e.selection_text().as_deref(), Some("two three\nfou"));
        // Backwards from the same place takes the anchor's cell too.
        e.select_to(0, 0);
        assert_eq!(e.selection_text().as_deref(), Some("one t"));

        e.select_from(5, 0, Unit::Word);
        assert_eq!(e.selection_text().as_deref(), Some("two"));
        e.select_to(9, 0);
        assert_eq!(e.selection_text().as_deref(), Some("two three"));

        // A line is the whole soft-wrapped line, joined.
        e.select_from(3, 1, Unit::Line);
        assert_eq!(e.selection_text().as_deref(), Some("four 0123456789abcdefghij"));
        e.select_none();
        assert_eq!(e.selection_text(), None);
    }

    #[test]
    fn the_selection_stays_on_its_text_as_output_scrolls() {
        let mut e = GhosttyEngine::mirror(20, 3, 100);
        e.feed(b"keep me\r\n");
        e.select_from(0, 0, Unit::Line);
        for i in 0..5 {
            e.feed(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(e.selection_text().as_deref(), Some("keep me"));
    }

    #[test]
    fn command_output_by_its_marks() {
        let mut e = GhosttyEngine::mirror(20, 10, 100);
        shell(&mut e, "echo hi", "alpha  \r\nbeta 0123456789abcdefghij\r\n");
        shell(&mut e, "seq 2", "1\r\n2\r\n");
        e.feed(P.as_bytes());
        assert_eq!(e.prompt_rows(), vec![0, 4, 7]);
        assert!(!e.select_output(0), "a prompt isn't output");
        assert!(e.select_output(2));
        assert_eq!(e.selection_text().as_deref(), Some("alpha\nbeta 0123456789abcdefghij"));
        assert!(e.select_output(5));
        assert_eq!(e.selection_text().as_deref(), Some("1\n2"));
    }

    #[test]
    fn find_goes_both_ways_and_wraps() {
        let mut e = GhosttyEngine::mirror(20, 4, 1000);
        for i in 0..50 {
            e.feed(format!("row {i} 中x\r\n").as_bytes());
        }
        let total = e.total_rows();
        assert_eq!(total, 51);
        let hit = e.find("row 7 ", total - 1, 0, true).unwrap();
        assert_eq!(hit, Found { row: 7, x: 0, width: 6 });
        // Wide characters count two columns.
        assert_eq!(e.find("X", 7, 0, false), None, "case folds only for lower-case needles");
        let hit = e.find("x", 7, 0, false).unwrap();
        assert_eq!((hit.row, hit.x, hit.width), (7, 8, 1));
        // From the top, backwards, wraps round to the end.
        assert_eq!(e.find("row 49", 0, 0, true).map(|f| f.row), Some(49));
        assert_eq!(e.find("nowhere", 0, 0, false), None);

        e.show_row(7);
        assert!(e.top_row() <= 7 && 7 < e.top_row() + 4);
        e.select_cells(hit.row, hit.x, hit.x + hit.width - 1);
        assert_eq!(e.selection_text().as_deref(), Some("x"));
    }

    #[test]
    fn an_archive_holds_far_more_than_a_pane() {
        // The daemon's engine (16 MiB) keeps about 20k rows at this width;
        // replayed into an archive, a line 40k rows up is still there.
        let mut out = b"needle-42\r\n".to_vec();
        for i in 1..=40_000 {
            out.extend(format!("{i}\r\n").as_bytes());
        }
        let mut daemon = GhosttyEngine::new(92, 28);
        daemon.feed(&out);
        assert!(daemon.find("needle-42", daemon.total_rows() - 1, 0, true).is_none());
        let mut a = GhosttyEngine::archive(92, 28, 64 << 20);
        a.feed(&out);
        let hit = a.find("needle-42", a.total_rows() - 1, 0, true).unwrap();
        assert_eq!((hit.row, hit.x), (0, 0));
    }
}
