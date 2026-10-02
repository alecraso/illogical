//! Snapshot round trips over recorded PTY sessions (`fixtures/*.bin`,
//! recorded with `fixtures/record.py`): feed a fixture into A, snapshot A
//! into a fresh B, and require that everything observable matches, on the
//! active screen and, for full-screen apps, on the primary screen after the
//! app exits.

use std::path::Path;

use libghostty_vt::{
    RenderState, Terminal,
    screen::{CellContentTag, Screen},
    style::StyleColor,
    terminal::{Mode, ModeKind, Point, PointCoordinate},
};

use super::GhosttyEngine;
use crate::VtEngine;

#[derive(serde::Deserialize)]
struct Meta {
    cols: u16,
    rows: u16,
    resizes: Vec<Resize>,
}

#[derive(serde::Deserialize)]
struct Resize {
    offset: usize,
    cols: u16,
    rows: u16,
}

fn load(name: &str) -> GhosttyEngine {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let bytes = std::fs::read(dir.join(format!("{name}.bin"))).unwrap();
    let meta: Meta = serde_json::from_slice(&std::fs::read(dir.join(format!("{name}.json"))).unwrap()).unwrap();
    let mut e = GhosttyEngine::new(meta.cols, meta.rows);
    let mut pos = 0;
    for r in &meta.resizes {
        e.feed(&bytes[pos..r.offset]);
        e.resize(r.cols, r.rows);
        pos = r.offset;
    }
    e.feed(&bytes[pos..]);
    e
}

/// A cell as it looks: empty equals a space, and a background stored on the
/// cell (from an erase) equals the same background set through SGR.
fn cell_key(t: &Terminal<'static, 'static>, x: u16, y: u16) -> String {
    let g = t.grid_ref(Point::Active(PointCoordinate { x, y: y.into() })).unwrap();
    let mut buf = ['\0'; 16];
    let n = g.graphemes(&mut buf).unwrap_or(0);
    let text: String = if n == 0 { " ".into() } else { buf[..n].iter().collect() };
    let cell = g.cell().unwrap();
    let mut style = g.style().unwrap();
    if matches!(style.bg_color, StyleColor::None) {
        match cell.content_tag().unwrap() {
            CellContentTag::BgColorPalette => style.bg_color = StyleColor::Palette(cell.bg_color_palette().unwrap()),
            CellContentTag::BgColorRgb => style.bg_color = StyleColor::Rgb(cell.bg_color_rgb().unwrap()),
            _ => {}
        }
    }
    format!("{text}|{style:?}")
}

/// Everything observable, as (name, value) pairs.
fn observe(e: &GhosttyEngine) -> Vec<(String, String)> {
    let t = e.terminal();
    let (cols, rows) = e.size();
    let mut v: Vec<(String, String)> = vec![
        ("size".into(), format!("{cols}x{rows}")),
        ("screen".into(), format!("{:?}", t.active_screen().unwrap())),
        ("cursor".into(), format!("{},{}", t.cursor_x().unwrap(), t.cursor_y().unwrap())),
        ("cursor_visible".into(), t.is_cursor_visible().unwrap().to_string()),
        ("cursor_sgr".into(), format!("{:?}", t.cursor_style().unwrap())),
        ("kitty".into(), format!("{:?}", t.kitty_keyboard_flags().unwrap())),
        ("title".into(), e.title()),
        ("pwd".into(), e.pwd()),
        ("scrollback".into(), t.scrollback_rows().unwrap().to_string()),
        ("palette".into(), format!("{:?}", t.color_palette().unwrap())),
    ];
    let mut rs = RenderState::new().unwrap();
    let snap = rs.update(t).unwrap();
    v.push(("cursor_shape".into(), format!("{:?}", snap.cursor_visual_style().unwrap())));
    v.push(("cursor_blink".into(), snap.cursor_blinking().unwrap().to_string()));
    for m in [1, 6, 7, 12, 25, 47, 66, 1000, 1002, 1003, 1004, 1006, 1047, 1049, 2004, 2026] {
        v.push((format!("?{m}"), t.mode(Mode::new(m, ModeKind::Dec)).unwrap().to_string()));
    }
    for y in 0..rows {
        let row: Vec<String> = (0..cols).map(|x| cell_key(t, x, y)).collect();
        v.push((format!("row {y}"), row.join("][")));
    }
    let text = e.plain_text();
    let text = text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
    v.push(("plain".into(), text.trim_end().to_string()));
    v
}

fn assert_same(a: &GhosttyEngine, b: &GhosttyEngine, what: &str) {
    let diffs: Vec<String> = observe(a)
        .into_iter()
        .zip(observe(b))
        .filter(|(x, y)| x.1 != y.1)
        .map(|(x, y)| format!("  {}:\n    want {:.300}\n    got  {:.300}", x.0, x.1, y.1))
        .collect();
    assert!(diffs.is_empty(), "{what}: {} differences\n{}", diffs.len(), diffs.join("\n"));
}

fn round_trip(name: &str) {
    let mut a = load(name);
    let before = observe(&a);
    let snap = a.snapshot();
    assert_eq!(before, observe(&a), "{name}: snapshot changed the source terminal");

    let (cols, rows) = a.size();
    let mut b = GhosttyEngine::new(cols, rows);
    b.feed(&snap);
    assert_same(&a, &b, name);

    if a.terminal().active_screen().unwrap() == Screen::Alternate {
        a.feed(b"\x1b[?1049l");
        b.feed(b"\x1b[?1049l");
        assert_same(&a, &b, &format!("{name} after leaving the alt screen"));
    }
}

#[test]
fn seq_deep_scrollback() {
    round_trip("seq");
}

#[test]
fn modes_colors_unicode() {
    round_trip("modes");
}

#[test]
fn nvim_over_scrollback() {
    round_trip("nvim");
}

#[test]
fn nvim_resized() {
    round_trip("nvim_resize");
}

#[test]
fn less_pager() {
    round_trip("less");
}

#[test]
fn top_redraw() {
    round_trip("top");
}

#[test]
fn reflow_after_narrowing() {
    round_trip("resize");
}

#[test]
fn answers_device_attributes() {
    let mut e = GhosttyEngine::new(80, 24);
    e.feed(b"\x1b[c");
    let reply = e.take_replies();
    assert!(reply.starts_with(b"\x1b[?62;"), "DA1 reply: {:?}", String::from_utf8_lossy(&reply));
    assert!(e.take_replies().is_empty(), "replies are drained");
}

#[test]
fn answers_cursor_position_and_colors() {
    let mut e = GhosttyEngine::new(80, 24);
    e.feed(b"\x1b[5;10H\x1b[6n");
    assert_eq!(e.take_replies(), b"\x1b[5;10R");
    e.feed(b"\x1b]11;?\x1b\\");
    let reply = String::from_utf8(e.take_replies()).unwrap();
    assert!(reply.contains("rgb:1e1e/1e1e/2e2e"), "OSC 11 reply: {reply:?}");
}

#[test]
fn snapshot_of_blank_terminal_is_blank() {
    let mut a = GhosttyEngine::new(80, 24);
    let snap = a.snapshot();
    let mut b = GhosttyEngine::new(80, 24);
    b.feed(&snap);
    assert_same(&a, &b, "blank");
}

#[test]
fn does_not_promise_what_xterm_js_cannot_draw() {
    let mut e = GhosttyEngine::new(80, 24);
    // What nvim asks at startup.
    e.feed(b"\x1b[?69$p\x1b[?2026$p\x1b[?u\x1b[c");
    let reply = String::from_utf8(e.take_replies()).unwrap();
    assert!(reply.contains("\x1b[?69;0$y"), "DECLRMM must read as unsupported: {reply:?}");
    assert!(reply.contains("\x1b[?2026;2$y"), "sync output is supported: {reply:?}");
    assert!(!reply.contains('u'), "no kitty keyboard reply: {reply:?}");
    assert!(reply.contains("\x1b[?62;"), "DA1 still answered: {reply:?}");
}

/// Checkpoints (GHOSTSNP + zstd) restore everything the snapshot does, with
/// no fix-ups, including the primary screen under a full-screen app.
fn checkpoint_round_trip(name: &str) {
    let mut a = load(name);
    let ckpt = a.checkpoint();
    let mut b = GhosttyEngine::from_checkpoint(&ckpt).expect("checkpoint decodes");
    assert_same(&a, &b, &format!("{name} checkpoint"));
    if a.terminal().active_screen().unwrap() == Screen::Alternate {
        a.feed(b"\x1b[?1049l");
        b.feed(b"\x1b[?1049l");
        assert_same(&a, &b, &format!("{name} checkpoint after leaving the alt screen"));
    }
}

#[test]
fn checkpoints_round_trip_every_fixture() {
    for name in ["seq", "modes", "nvim", "nvim_resize", "less", "top", "resize"] {
        checkpoint_round_trip(name);
    }
}

#[test]
fn checkpoint_mid_escape_sequence_resumes() {
    let mut a = GhosttyEngine::new(80, 24);
    a.feed(b"hello \x1b[1;3");
    let mut b = GhosttyEngine::from_checkpoint(&a.checkpoint()).unwrap();
    a.feed(b"1mred\x1b[0m world");
    b.feed(b"1mred\x1b[0m world");
    assert_same(&a, &b, "split CSI");
}

#[test]
fn restored_engine_still_answers_queries() {
    let a = load("modes");
    let mut b = GhosttyEngine::from_checkpoint(&a.checkpoint()).unwrap();
    b.feed(b"\x1b[c\x1b[?69$p");
    let reply = String::from_utf8(b.take_replies()).unwrap();
    assert!(reply.contains("\x1b[?62;") && reply.contains("\x1b[?69;0$y"), "{reply:?}");
}

#[test]
fn bad_checkpoints_are_rejected() {
    use crate::CheckpointError;
    let good = load("modes").checkpoint();
    assert_eq!(GhosttyEngine::from_checkpoint(b"nope").err(), Some(CheckpointError::NotACheckpoint));
    let mut other = good.clone();
    let tag_at = other.iter().position(|b| *b == b'@').unwrap();
    other[tag_at + 1] = b'X';
    assert!(matches!(GhosttyEngine::from_checkpoint(&other), Err(CheckpointError::OtherEngine(_))));
    let mut corrupt = good.clone();
    let n = corrupt.len();
    corrupt[n - 10] ^= 0xff;
    assert_eq!(GhosttyEngine::from_checkpoint(&corrupt).err(), Some(CheckpointError::Corrupt));
    assert_eq!(GhosttyEngine::from_checkpoint(&good[..good.len() / 2]).err(), Some(CheckpointError::Corrupt));
}

#[test]
fn screen_snapshot_has_no_history() {
    let mut a = GhosttyEngine::new(40, 5);
    for i in 0..30 {
        a.feed(format!("line {i}\r\n").as_bytes());
    }
    a.feed(b"on screen now");
    assert!(a.plain_text().contains("line 0"));
    let mut b = GhosttyEngine::new(40, 5);
    b.feed(&a.screen_snapshot());
    let text = b.plain_text();
    assert!(!text.contains("line 0"), "history leaked: {text}");
    assert!(!text.contains("line 25\n"), "history leaked: {text}");
    assert!(text.contains("line 29") && text.contains("on screen now"), "screen lost: {text}");
    // A full snapshot still has it all.
    let mut c = GhosttyEngine::new(40, 5);
    c.feed(&a.snapshot());
    assert!(c.plain_text().contains("line 0"));
}
