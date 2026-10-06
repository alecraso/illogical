//! Snapshot round trips over recorded and generated sessions
//! (`fixtures/*.bin`: S1's recordings from `fixtures/record.py`, and the
//! S1/S5 follow-up's, from `archive/spikes:spikes/s1s5-followup/gen.py` and
//! `record_claude.py`). Feed a fixture into A, snapshot A into a fresh B, and
//! require that everything observable matches: as restored, and after each
//! probe (bytes sent to both afterwards to bring out state that can't be
//! read directly, such as the saved cursor, margins and protection).

use std::path::Path;

use libghostty_vt::{
    RenderState, Terminal,
    screen::{CellContentTag, Screen},
    style::{Style, StyleColor, Underline},
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

/// Every fixture: S1's seven, then the follow-up's.
const FIXTURES: &[&str] = &[
    "seq",
    "modes",
    "nvim",
    "nvim_resize",
    "less",
    "top",
    "resize",
    "claude",
    "claude_exit",
    "decom",
    "slrm",
    "slrm_decom",
    "decsc_primary",
    "decsc_wrap",
    "decsc_1049",
    "decsc_47",
    "kitty",
    "sixel",
];

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
/// cell (from an erase) equals the same background set through SGR. Also
/// its protection (DECSCA) and hyperlink.
fn cell_key(t: &Terminal<'static, 'static>, p: Point) -> String {
    let g = t.grid_ref(p).unwrap();
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
    let mut link = [0u8; 256];
    let n = g.hyperlink_uri(&mut link).unwrap_or(0);
    let link = &link[..n];
    let prot = if cell.is_protected().unwrap_or(false) { " protected" } else { "" };
    let link = if link.is_empty() { String::new() } else { format!(" link={}", String::from_utf8_lossy(link)) };
    format!("{text:?}{}{prot}{link}", compact(&style))
}

/// Non-default style fields only.
fn compact(s: &Style) -> String {
    let c = |c: &StyleColor| match c {
        StyleColor::None => None,
        StyleColor::Palette(p) => Some(format!("p{}", p.0)),
        StyleColor::Rgb(r) => Some(format!("#{:02x}{:02x}{:02x}", r.r, r.g, r.b)),
    };
    let mut out = String::new();
    for (k, v) in [("fg", c(&s.fg_color)), ("bg", c(&s.bg_color)), ("ul", c(&s.underline_color))] {
        if let Some(v) = v {
            out += &format!(" {k}={v}");
        }
    }
    for (on, k) in [
        (s.bold, "bold"),
        (s.italic, "italic"),
        (s.faint, "faint"),
        (s.blink, "blink"),
        (s.inverse, "inverse"),
        (s.invisible, "invisible"),
        (s.strikethrough, "strike"),
        (s.overline, "overline"),
    ] {
        if on {
            out += &format!(" {k}");
        }
    }
    if s.underline != Underline::None {
        out += &format!(" underline={:?}", s.underline);
    }
    out
}

const OBSERVED_DEC_MODES: &[u16] =
    &[1, 5, 6, 7, 12, 25, 45, 47, 66, 69, 1000, 1002, 1003, 1004, 1006, 1047, 1049, 2004, 2026, 2027, 2031, 2048];

/// Everything observable, as (name, value) pairs: the active screen and
/// every cell of the scrollback (newest first, so that a missing row shows
/// up where it is missing).
fn observe(e: &GhosttyEngine) -> Vec<(String, String)> {
    let t = e.terminal();
    let (cols, rows) = e.size();
    let mut v: Vec<(String, String)> = vec![
        ("size".into(), format!("{cols}x{rows}")),
        ("screen".into(), format!("{:?}", t.active_screen().unwrap())),
        ("cursor".into(), format!("{},{}", t.cursor_x().unwrap(), t.cursor_y().unwrap())),
        ("pending_wrap".into(), t.is_cursor_pending_wrap().unwrap().to_string()),
        ("cursor_visible".into(), t.is_cursor_visible().unwrap().to_string()),
        ("cursor_sgr".into(), compact(&t.cursor_style().unwrap())),
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
    for &m in OBSERVED_DEC_MODES {
        v.push((format!("?{m}"), t.mode(Mode::new(m, ModeKind::Dec)).unwrap().to_string()));
    }
    for m in [4, 20] {
        v.push((format!("ansi {m}"), t.mode(Mode::new(m, ModeKind::Ansi)).unwrap().to_string()));
    }
    for y in 0..rows {
        let row: Vec<String> =
            (0..cols).map(|x| cell_key(t, Point::Active(PointCoordinate { x, y: y.into() }))).collect();
        v.push((format!("row {y}"), row.join("][")));
    }
    let hist = t.scrollback_rows().unwrap();
    for y in (0..hist).rev() {
        let row: Vec<String> =
            (0..cols).map(|x| cell_key(t, Point::History(PointCoordinate { x, y: y as u32 }))).collect();
        v.push((format!("history row -{}", hist - y), row.join("][")));
    }
    let text = e.plain_text();
    let text = text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
    v.push(("plain".into(), text.trim_end().to_string()));
    v
}

fn differences(a: &[(String, String)], b: &[(String, String)], skip: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out: Vec<String> = a
        .iter()
        .zip(b)
        .filter(|(x, y)| !skip(&x.0) && (x.0 != y.0 || x.1 != y.1))
        .map(|(x, y)| format!("  {}:\n    want {:.300}\n    got  {} {:.300}", x.0, x.1, y.0, y.1))
        .collect();
    if a.len() != b.len() && !skip("scrollback") {
        out.push(format!("  {} observations, got {}", a.len(), b.len()));
    }
    out
}

fn assert_same(a: &GhosttyEngine, b: &GhosttyEngine, what: &str) {
    let diffs = differences(&observe(a), &observe(b), &|_| false);
    assert!(diffs.is_empty(), "{what}: {} differences\n{}", diffs.len(), diffs.join("\n"));
}

/// Bytes sent to both terminals after the restore, each to a fresh pair.
fn probes() -> Vec<(&'static str, Vec<u8>)> {
    let mut wrap = b"\x1b[H".to_vec();
    wrap.extend("0123456789".repeat(16).bytes());
    wrap.extend_from_slice(b"\x1b[99B\n\n\nEND\x1b[2L");
    vec![
        ("as restored", vec![]),
        // The live cursor's pen, charset, protection, hyperlink and
        // pending wrap.
        ("print", b"Zq".to_vec()),
        ("DECRC, print", b"\x1b8q@".to_vec()),
        // Wrapping, margins and the scroll region.
        ("home, long line, LFs at the bottom, IL", wrap),
        // Protection.
        ("selective erase (DECSED)", b"\x1b[?2J".to_vec()),
        // Each screen's saved cursor.
        ("1049l, print, DECRC, print", b"\x1b[?1049lq@\x1b8q@".to_vec()),
        ("47l, DECRC, print", b"\x1b[?47l\x1b8q@".to_vec()),
    ]
}

/// Snapshot `make()` into a fresh engine and require the two to match,
/// under every probe. Also that taking the snapshot changes nothing.
fn round_trip_with(name: &str, make: &dyn Fn() -> GhosttyEngine) {
    for (probe, bytes) in probes() {
        let mut a = make();
        let before = observe(&a);
        let snap = a.snapshot();
        assert_eq!(before, observe(&a), "{name}: snapshot changed the source terminal");
        let (cols, rows) = a.size();
        let mut b = GhosttyEngine::new(cols, rows);
        b.feed(&snap);
        a.feed(&bytes);
        b.feed(&bytes);
        assert_same(&a, &b, &format!("{name} [{probe}]"));
    }
}

fn round_trip(name: &str) {
    round_trip_with(name, &|| load(name));
}

/// One test per fixture, so that each failure shows on its own.
macro_rules! round_trips {
    ($($test:ident: $name:literal,)*) => {$(
        #[test]
        fn $test() {
            round_trip($name);
        }
    )*};
}

round_trips! {
    seq_deep_scrollback: "seq",
    modes_colors_unicode: "modes",
    nvim_over_scrollback: "nvim",
    nvim_resized: "nvim_resize",
    less_pager: "less",
    top_redraw: "top",
    reflow_after_narrowing: "resize",
    claude_code_running: "claude",
    claude_code_exited: "claude_exit",
    origin_mode_in_a_scroll_region: "decom",
    left_right_margins: "slrm",
    left_right_margins_and_origin_mode: "slrm_decom",
    saved_cursor_on_the_primary_screen: "decsc_primary",
    saved_cursor_with_a_pending_wrap: "decsc_wrap",
    saved_cursors_through_1049: "decsc_1049",
    saved_cursors_through_47: "decsc_47",
    kitty_graphics_turned_off: "kitty",
    sixel_ignored: "sixel",
}

fn engine_fed(cols: u16, rows: u16, bytes: &[u8]) -> GhosttyEngine {
    let mut e = GhosttyEngine::new(cols, rows);
    e.feed(bytes);
    e
}

#[test]
fn live_pending_wrap() {
    // The cursor past the last column, with a pen, charset, protection and
    // hyperlink that differ from the cell under it.
    let line = format!("$ {}\x1b[1;31m\x1b]8;;https://e\x1b\\\x1b[1\"q\x1b(0", "x".repeat(38));
    round_trip_with("pending wrap", &|| engine_fed(40, 6, line.as_bytes()));
    // On a wide character, under origin mode in a region.
    let wide = format!("\x1b[2;5r\x1b[?6h\x1b[4;1H{}\u{4e2d}", "y".repeat(38));
    round_trip_with("pending wrap, wide", &|| engine_fed(40, 6, wide.as_bytes()));
    // Under insert mode.
    round_trip_with("pending wrap, insert", &|| engine_fed(10, 3, b"0123456789\x1b[4h"));
}

#[test]
fn blank_screen_over_history() {
    // `clear` in Ghostty scrolls the screen into the history.
    let mut text = String::new();
    for i in 0..30 {
        text += &format!("\x1b[3{}mline {i}\x1b[0m\r\n", i % 8);
    }
    text += "\x1b[H\x1b[2J";
    round_trip_with("cleared", &|| engine_fed(40, 8, text.as_bytes()));
    round_trip_with("cleared, then a prompt below", &|| {
        engine_fed(40, 8, format!("{text}\x1b[3;1H$ \x1b[44m\x1b[K").as_bytes())
    });
}

#[test]
fn blank_cells_keep_their_own_colors() {
    // Blank cells between differently colored text, and links and
    // protection on cells, which the formatter loses.
    let bytes = b"\x1b[41mred\x1b[0m   \x1b[42mgreen\x1b[0m\x1b[5G\x1b[44m \x1b[0m\r\n\
        \x1b]8;id=1;https://a\x1b\\link\x1b]8;;\x1b\\ \x1b[1\"qkept\x1b[0\"q plain\r\n";
    round_trip_with("blank cells", &|| engine_fed(30, 4, bytes));
    round_trip_with("blank cells under an alt screen", &|| {
        let mut e = engine_fed(30, 4, b"$ app\r\n\x1b[?1049h");
        e.feed(bytes);
        e
    });
}

#[test]
fn snapshots_are_the_same_every_time() {
    for name in ["claude", "decsc_1049", "modes", "decom"] {
        let mut a = load(name);
        assert_eq!(a.snapshot(), a.snapshot(), "{name}");
    }
}

#[test]
fn a_changed_pane_gets_a_new_snapshot() {
    // What the last snapshot learned (saved cursors, cells to repaint) is
    // only reused while nothing changed.
    round_trip_with("claude, changed", &|| {
        let mut e = load("claude");
        e.snapshot();
        e.feed(b"\x1b[3;3H\x1b[1;35m\x1b7\x1b[10;10H\x1b[44m \x1b[0m  \x1b[42mx");
        e
    });
    round_trip_with("modes, scrolled", &|| {
        let mut e = load("modes");
        e.snapshot();
        e.feed(b"\x1b[41m\r\nmore\x1b[0m   \x1b[45mtext\r\n");
        e
    });
}

#[test]
fn a_line_wrapped_into_the_screen() {
    // The screen's first row continues a line from the history, with blank
    // cells between colors all along it.
    let mut text = String::new();
    for i in 0..30 {
        text += &format!("line {i}\r\n");
    }
    text += &"\x1b[41m  \x1b[0m ab \x1b[44m \x1b[0m".repeat(12);
    let make = || engine_fed(20, 4, text.as_bytes());
    round_trip_with("wrapped", &make);
    for keep in [0, 1, 2, 100] {
        let mut a = make();
        let mut b = GhosttyEngine::new(20, 4);
        b.feed(&a.snapshot_history(Some(keep)));
        let rows =
            |e: &GhosttyEngine| observe(e).into_iter().filter(|(k, _)| k.starts_with("row ")).collect::<Vec<_>>();
        assert_eq!(rows(&a), rows(&b), "keeping {keep}");
    }
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
/// no fix-ups, under every probe. Kitty images aren't in GHOSTSNP v1, but
/// the engine keeps none for xterm.js anyway.
#[test]
fn checkpoints_round_trip_every_fixture() {
    for name in FIXTURES {
        for (probe, bytes) in probes() {
            let mut a = load(name);
            let mut b = GhosttyEngine::from_checkpoint(&a.checkpoint()).expect("checkpoint decodes");
            a.feed(&bytes);
            b.feed(&bytes);
            assert_same(&a, &b, &format!("{name} checkpoint [{probe}]"));
        }
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

/// The newest lines of `e`'s plain text, without trailing blanks.
fn plain_lines(e: &GhosttyEngine) -> Vec<String> {
    let text = e.plain_text();
    let mut lines: Vec<String> = text.lines().map(|l| l.trim_end().to_owned()).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// A snapshot capped at `keep` rows of history: the screen, modes, cursor
/// and colors are exactly as in a full one, and the history it keeps is the
/// newest `keep` rows (or all, if there are fewer).
fn capped_round_trip(name: &str, keep: usize) {
    let mut a = load(name);
    let have = a.terminal().scrollback_rows().unwrap();
    let snap = a.snapshot_history(Some(keep));
    let (cols, rows) = a.size();
    let mut b = GhosttyEngine::new(cols, rows);
    b.feed(&snap);

    let history = |k: &str| k == "scrollback" || k == "plain" || k.starts_with("history row");
    let diffs = differences(&observe(&a), &observe(&b), &history);
    assert!(diffs.is_empty(), "{name} keeping {keep}: {} differences\n{}", diffs.len(), diffs.join("\n"));

    let kept = b.terminal().scrollback_rows().unwrap();
    assert_eq!(kept, keep.min(have), "{name} keeping {keep}: history rows (of {have})");
    if a.terminal().active_screen().unwrap() == Screen::Alternate {
        a.feed(b"\x1b[?1049l");
        b.feed(b"\x1b[?1049l");
    }
    let (want, got) = (plain_lines(&a), plain_lines(&b));
    assert!(
        want.ends_with(&got),
        "{name} keeping {keep}: not the newest rows\n  want ..{:?}\n  got {:?}",
        &want[want.len().saturating_sub(5)..],
        &got[got.len().saturating_sub(5)..]
    );
}

#[test]
fn capped_snapshots_keep_the_screen_and_the_newest_history() {
    for name in FIXTURES {
        for keep in [0, 1, 7, 100, 1_000_000] {
            capped_round_trip(name, keep);
        }
    }
}

#[test]
fn kitty_graphics_are_off_for_xterm_js() {
    // What chafa, yazi and timg send to find out whether images work.
    let query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c";
    let mut e = GhosttyEngine::new(80, 24);
    e.feed(query);
    let reply = String::from_utf8(e.take_replies()).unwrap();
    assert!(!reply.contains("\x1b_G"), "graphics query answered: {reply:?}");
    assert!(reply.contains("\x1b[?62;"), "DA1 still answered: {reply:?}");
    e.feed(b"\x1b[?1049h");
    e.feed(query);
    assert!(!String::from_utf8(e.take_replies()).unwrap().contains("\x1b_G"), "answered on the alt screen");
    assert_eq!(e.terminal().kitty_image_storage_limit().unwrap(), 0);
    let restored = GhosttyEngine::from_checkpoint(&e.checkpoint()).unwrap();
    assert_eq!(restored.terminal().kitty_image_storage_limit().unwrap(), 0, "after a checkpoint");

    let mut all = GhosttyEngine::with_capabilities(80, 24, crate::Capabilities::ALL);
    all.feed(query);
    assert!(String::from_utf8(all.take_replies()).unwrap().contains("\x1b_Gi=31;OK"), "a client that draws them");
}

#[test]
fn engine_tag_fingerprints_the_snapshot_format() {
    let tag = crate::engine_tag();
    let (_, snap) = tag.split_once(" snap@").expect("a format fingerprint");
    assert_eq!(snap.len(), 16, "{tag}");
    assert_eq!(super::fnv1a(&super::canary_snapshot()), u64::from_str_radix(snap, 16).unwrap());
    assert_eq!(super::canary_snapshot(), super::canary_snapshot(), "GHOSTSNP is deterministic");
    assert!(!super::canary_snapshot().is_empty());
}

#[test]
fn an_alt_screen_snapshot_mid_sequence_leaves_the_stream_alone() {
    // Issue #53. The pane's stream stops inside a sequence under an alt
    // screen; a snapshot then, and the rest of the stream, must give what
    // the stream alone gives: in the live terminal, in a new snapshot of it,
    // and in a terminal the snapshot was fed to (which gets the rest too).
    let mut setup = String::new();
    for i in 0..20 {
        setup += &format!("\x1b[3{}mline {i}\x1b[0m\r\n", i % 8);
    }
    // And on the primary screen, where only the continuation matters.
    let setups = [format!("{setup}$ app\x1b[?1049h\x1b[2;3Halt "), format!("{setup}$ ")];
    let big = format!("\x1b]2;{}", "t".repeat(2 << 20));
    let splits: &[(&str, &[u8], &[u8])] = &[
        ("CSI", b"\x1b[38;2;1", b";2;3mcolored\x1b[0m"),
        ("ESC", b"\x1b", b"7moved\x1b8saved"),
        ("OSC", b"\x1b]2;tit", b"le\x1b\\titled"),
        ("UTF-8", b"\xe4\xb8", b"\xad wide"),
        // Too long to keep as a continuation: no copy of the terminal can
        // be made (the snapshot has only the alt screen), and a terminal
        // fed the snapshot prints the rest.
        ("long OSC", big.as_bytes(), b"\x1b\\after"),
    ];
    for (setup, &(name, head, rest)) in setups.iter().flat_map(|s| splits.iter().map(move |x| (s, x))) {
        let name = &format!("{name}{}", if setup.ends_with("alt ") { "" } else { ", primary" });
        let mut a = engine_fed(40, 6, setup.as_bytes());
        let mut want = engine_fed(40, 6, setup.as_bytes());
        a.feed(head);
        want.feed(head);
        let snap = a.snapshot();
        a.feed(rest);
        want.feed(rest);
        assert_same(&want, &a, &format!("{name}: live terminal"));
        assert_eq!(want.snapshot(), a.snapshot(), "{name}: new snapshot");
        if !name.starts_with("long OSC") {
            let mut b = engine_fed(40, 6, &snap);
            b.feed(rest);
            assert_same(&want, &b, &format!("{name}: fed the snapshot"));
            // With the primary screen and its history.
            b.feed(b"\x1b[?1049l");
            want.feed(b"\x1b[?1049l");
            assert_same(&want, &b, &format!("{name}: fed the snapshot, primary"));
        }
    }
}

/// The in-memory scrollback stops at its byte budget (M9: 16 MiB, about
/// 9.6k rows at 200 columns), well short of the lines written.
#[test]
fn scrollback_is_capped() {
    let mut e = GhosttyEngine::new(200, 50);
    let line = format!("{}\r\n", "x".repeat(199));
    let chunk = line.repeat(1000);
    for _ in 0..40 {
        e.feed(chunk.as_bytes());
    }
    let kept = e.terminal().scrollback_rows().unwrap();
    assert!((5_000..15_000).contains(&kept), "kept {kept} rows of 40,000");
}
