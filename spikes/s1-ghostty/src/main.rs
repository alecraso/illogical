//! S1: does a libghostty-vt VT snapshot reproduce the terminal it came from?
//!
//! For each fixture: feed the recorded bytes into terminal A (applying
//! resizes at their offsets), format A as VT with every extra enabled, feed
//! that snapshot into a fresh terminal B of the same size, then compare A and
//! B. Snapshots are written to fixtures/<name>.snap for the xterm.js check.

use std::{fs, path::Path, time::Instant};

use libghostty_vt::{
    RenderState, Terminal, TerminalOptions,
    fmt::{Format, Formatter, FormatterOptions},
    terminal::{Mode, ModeKind, Point, PointCoordinate},
};

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

const SCROLLBACK: usize = 50_000_000;

const DEC_MODES: &[u16] = &[
    1, 6, 7, 12, 25, 47, 66, 1000, 1002, 1003, 1004, 1005, 1006, 1015, 1047, 1049, 2004, 2026,
];
const ANSI_MODES: &[u16] = &[4, 20];

fn new_term(cols: u16, rows: u16) -> Terminal<'static, 'static> {
    Terminal::new(TerminalOptions { cols, rows, max_scrollback: SCROLLBACK }).unwrap()
}

fn format(t: &Terminal<'static, 'static>, f: Format, extras: bool) -> Vec<u8> {
    format_opts(t, f, extras, extras)
}

fn format_opts(t: &Terminal<'static, 'static>, f: Format, extras: bool, modes: bool) -> Vec<u8> {
    let mut o = FormatterOptions::new().with_format(f).with_modes(modes);
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
    let mut fm = Formatter::new(t, o).unwrap();
    fm.format_alloc(None).unwrap().to_vec()
}

/// What libghostty's formatter leaves out or gets wrong (see S1 findings):
/// title (not emitted), cursor shape (not emitted), cursor position
/// (clobbered by the tabstops extra, which moves the cursor with CHA).
fn trailer(t: &Terminal<'static, 'static>) -> Vec<u8> {
    let mut out = Vec::new();
    if let Ok(title) = t.title() {
        if !title.is_empty() {
            out.extend_from_slice(format!("\x1b]2;{title}\x1b\\").as_bytes());
        }
    }
    let mut rs = RenderState::new().unwrap();
    let snap = rs.update(t).unwrap();
    use libghostty_vt::render::CursorVisualStyle as S;
    let blink = snap.cursor_blinking().unwrap();
    let n = match snap.cursor_visual_style().unwrap() {
        S::Block | S::BlockHollow => if blink { 1 } else { 2 },
        S::Underline => if blink { 3 } else { 4 },
        S::Bar => if blink { 5 } else { 6 },
        _ => 0,
    };
    out.extend_from_slice(format!("\x1b[{n} q").as_bytes());
    // Re-create blank rows the formatter dropped, but only when something
    // above them was written (an all-blank screen needs no padding).
    let (pad, paint) = trailing_rows(t);
    if pad < t.rows().unwrap() as u32 {
        out.extend(std::iter::repeat_n(&b"\r\n"[..], pad as usize).flatten());
    }
    if !paint.is_empty() {
        // DECSC/DECRC keeps the pen (SGR) the formatter set up for the cursor.
        out.extend_from_slice(b"\x1b7");
        out.extend(paint);
        out.extend_from_slice(b"\x1b8");
    }
    out.extend_from_slice(format!("\x1b[{};{}H", t.cursor_y().unwrap() + 1, t.cursor_x().unwrap() + 1).as_bytes());
    out
}

/// A cell as it looks: empty and space are the same, and a background
/// stored on the cell (from an erase) equals the same background as SGR.
fn cell_key(g: &libghostty_vt::screen::GridRef<'_>) -> String {
    use libghostty_vt::screen::CellContentTag as T;
    use libghostty_vt::style::StyleColor;
    let mut buf = ['\0'; 16];
    let n = g.graphemes(&mut buf).unwrap_or(0);
    let text: String = buf[..n].iter().collect();
    let text = if text.is_empty() { " ".to_string() } else { text };
    let cell = g.cell().unwrap();
    let mut style = g.style().unwrap();
    if matches!(style.bg_color, StyleColor::None) {
        match cell.content_tag().unwrap() {
            T::BgColorPalette => style.bg_color = StyleColor::Palette(cell.bg_color_palette().unwrap()),
            T::BgColorRgb => style.bg_color = StyleColor::Rgb(cell.bg_color_rgb().unwrap()),
            _ => {}
        }
    }
    format!("{text}|{style:?}")
}

/// The formatter drops textless rows at the bottom of the active area,
/// which shifts the screen up on replay and loses erased backgrounds.
/// Returns how many there are and the bytes to repaint their backgrounds.
fn trailing_rows(t: &Terminal<'static, 'static>) -> (u32, Vec<u8>) {
    use libghostty_vt::screen::CellContentTag as T;
    use libghostty_vt::style::StyleColor;
    let (cols, rows) = (t.cols().unwrap(), t.rows().unwrap() as u32);
    let mut n = 0;
    let mut paint = Vec::new();
    for y in (0..rows).rev() {
        let mut row_paint = Vec::new();
        for x in 0..cols {
            let g = t.grid_ref(Point::Active(PointCoordinate { x, y })).unwrap();
            let cell = g.cell().unwrap();
            if cell.has_text().unwrap() {
                return (n, paint);
            }
            let bg = match (g.style().unwrap().bg_color, cell.content_tag().unwrap()) {
                (StyleColor::Rgb(c), _) => Some(format!("48;2;{};{};{}", c.r, c.g, c.b)),
                (StyleColor::Palette(p), _) => Some(format!("48;5;{}", p.0)),
                (_, T::BgColorRgb) => {
                    let c = cell.bg_color_rgb().unwrap();
                    Some(format!("48;2;{};{};{}", c.r, c.g, c.b))
                }
                (_, T::BgColorPalette) => Some(format!("48;5;{}", cell.bg_color_palette().unwrap().0)),
                _ => None,
            };
            if let Some(bg) = bg {
                row_paint.extend_from_slice(format!("\x1b[{};{}H\x1b[0;{bg}m ", y + 1, x + 1).as_bytes());
            }
        }
        paint.extend(row_paint);
        n += 1;
    }
    (n, paint)
}

/// Full snapshot. While the alt screen is active the formatter only sees the
/// alt screen, so briefly flip to the primary with mode 47 (no clear, no
/// cursor save), format it, and flip back.
fn snapshot(t: &mut Terminal<'static, 'static>, fixups: bool) -> Vec<u8> {
    use libghostty_vt::screen::Screen;
    let mut out = Vec::new();
    if fixups && t.active_screen().unwrap() == Screen::Alternate {
        t.vt_write(b"\x1b[?47l");
        out.extend(format(t, Format::Vt, false));
        let (pad, _) = trailing_rows(t);
        if pad < t.rows().unwrap() as u32 {
            out.extend(std::iter::repeat_n(&b"\r\n"[..], pad as usize).flatten());
        }
        // No CUP here: mode 47 carries the alt cursor across, and the cursor
        // 1049l restores (the saved one) is not exposed. Leaving the cursor
        // on the line after the primary content matches where the shell was
        // when the app started, which is what the saved cursor normally is.
        t.vt_write(b"\x1b[?47h");
        // Entering via 47 sets the ?47 flag; the app entered via 1049.
        t.set_mode(Mode::new(47, ModeKind::Dec), false).unwrap();
        // 1049h saves the cursor just placed (restored on exit) but does not
        // home it, and the formatter assumes the alt content starts at 1;1.
        out.extend_from_slice(b"\x1b[?1049h\x1b[H");
        out.extend(format_opts(t, Format::Vt, true, false));
        out.extend(modes(t));
    } else {
        out.extend(format(t, Format::Vt, true));
    }
    if fixups {
        out.extend(trailer(t));
    }
    out
}

/// Non-default modes as CSI h/l, minus the screen switches (already done).
fn modes(t: &Terminal<'static, 'static>) -> Vec<u8> {
    const DEFAULT_ON: &[u16] = &[7, 25];
    let mut out = Vec::new();
    for &m in DEC_MODES.iter().filter(|m| ![47, 1047, 1049].contains(*m)) {
        let on = t.mode(Mode::new(m, ModeKind::Dec)).unwrap_or(false);
        if on != DEFAULT_ON.contains(&m) {
            out.extend_from_slice(format!("\x1b[?{m}{}", if on { 'h' } else { 'l' }).as_bytes());
        }
    }
    for &m in ANSI_MODES {
        if t.mode(Mode::new(m, ModeKind::Ansi)).unwrap_or(false) {
            out.extend_from_slice(format!("\x1b[{m}h").as_bytes());
        }
    }
    out
}

/// Everything we can observe about a terminal, as comparable strings.
fn observe(t: &Terminal<'static, 'static>) -> Vec<(String, String)> {
    let mut v = vec![
        ("size".into(), format!("{}x{}", t.cols().unwrap(), t.rows().unwrap())),
        ("active_screen".into(), format!("{:?}", t.active_screen().unwrap())),
        ("cursor".into(), format!("{},{}", t.cursor_x().unwrap(), t.cursor_y().unwrap())),
        ("cursor_pending_wrap".into(), format!("{}", t.is_cursor_pending_wrap().unwrap())),
        ("cursor_visible".into(), format!("{}", t.is_cursor_visible().unwrap())),
        ("cursor_sgr".into(), format!("{:?}", t.cursor_style().unwrap())),
        ("kitty_flags".into(), format!("{:?}", t.kitty_keyboard_flags().unwrap())),
        ("mouse_tracking".into(), format!("{}", t.is_mouse_tracking().unwrap())),
        ("title".into(), t.title().unwrap_or("").to_string()),
        ("pwd".into(), t.pwd().unwrap_or("").to_string()),
        ("scrollback_rows".into(), format!("{}", t.scrollback_rows().unwrap())),
        ("palette".into(), format!("{:?}", t.color_palette().unwrap())),
    ];
    let mut rs = RenderState::new().unwrap();
    let snap = rs.update(t).unwrap();
    v.push(("cursor_shape".into(), format!("{:?}", snap.cursor_visual_style().unwrap())));
    v.push(("cursor_blink".into(), format!("{}", snap.cursor_blinking().unwrap())));
    for &m in DEC_MODES {
        let on = t.mode(Mode::new(m, ModeKind::Dec)).unwrap_or(false);
        v.push((format!("?{m}"), on.to_string()));
    }
    for &m in ANSI_MODES {
        let on = t.mode(Mode::new(m, ModeKind::Ansi)).unwrap_or(false);
        v.push((format!("{m}"), on.to_string()));
    }
    // Cell-by-cell active area: graphemes + style per cell, one line per row.
    let mut grid = String::new();
    for y in 0..t.rows().unwrap() as u32 {
        for x in 0..t.cols().unwrap() {
            let g = t.grid_ref(Point::Active(PointCoordinate { x, y })).unwrap();
            grid.push_str(&format!("[{}]", cell_key(&g)));
        }
        grid.push('\n');
    }
    v.push(("active_grid".into(), grid));
    // Content: plain text and full VT rendering of the active screen
    // (including scrollback when the primary screen is active).
    v.push(("plain".into(), String::from_utf8_lossy(&format(t, Format::Plain, false)).into()));
    v.push(("vt".into(), String::from_utf8_lossy(&format(t, Format::Vt, false)).into()));
    v
}

fn grid_diff(a: &str, b: &str) -> String {
    let (mut n, mut first) = (0, None);
    for (y, (la, lb)) in a.lines().zip(b.lines()).enumerate() {
        let (ca, cb): (Vec<_>, Vec<_>) = (la.split("][").collect(), lb.split("][").collect());
        for x in 0..ca.len().max(cb.len()) {
            let (p, q) = (ca.get(x).copied().unwrap_or("-"), cb.get(x).copied().unwrap_or("-"));
            if p != q {
                n += 1;
                first.get_or_insert(format!("row {y} col {x}\n      A: {p}\n      B: {q}"));
            }
        }
    }
    format!("{n} cells differ; first at {}", first.unwrap_or_default())
}

fn first_diff(a: &str, b: &str) -> String {
    let (al, bl): (Vec<_>, Vec<_>) = (a.lines().collect(), b.lines().collect());
    for i in 0..al.len().max(bl.len()) {
        let (x, y) = (al.get(i).copied().unwrap_or("<none>"), bl.get(i).copied().unwrap_or("<none>"));
        if x != y {
            return format!("line {i} ({} vs {} lines)\n      A: {:?}\n      B: {:?}", al.len(), bl.len(), x, y);
        }
    }
    "lines equal (whitespace/newline difference)".into()
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut names: Vec<String> = std::env::args().skip(1).collect();
    if names.is_empty() {
        names = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                (p.extension()? == "bin").then(|| p.file_stem().unwrap().to_string_lossy().into())
            })
            .collect();
        names.sort();
    }

    let mut failures = 0;
    for name in names {
        let bytes = fs::read(dir.join(format!("{name}.bin"))).unwrap();
        let meta: Meta = serde_json::from_slice(&fs::read(dir.join(format!("{name}.json"))).unwrap()).unwrap();

        let mut a = new_term(meta.cols, meta.rows);
        let mut pos = 0;
        for r in &meta.resizes {
            a.vt_write(&bytes[pos..r.offset]);
            a.resize(r.cols, r.rows, 8, 16).unwrap();
            pos = r.offset;
        }
        a.vt_write(&bytes[pos..]);

                let fixups = std::env::var("NO_FIXUPS").is_err();
        let before = observe(&a);
        let t0 = Instant::now();
        let snap = snapshot(&mut a, fixups);
        let disturbed: Vec<_> = before.iter().zip(observe(&a)).filter(|(x, y)| x.1 != y.1).map(|(x, _)| x.0.clone()).collect();
        if !disturbed.is_empty() {
            println!("    !! snapshot disturbed source terminal: {disturbed:?}");
            failures += 1;
        }
        let snap_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let _ = &mut snap_ms.clone();
        fs::write(dir.join(format!("{name}.snap")), &snap).unwrap();
        let expect = serde_json::json!({
            "cols": a.cols().unwrap(),
            "rows": a.rows().unwrap(),
            "alt": a.active_screen().unwrap() == libghostty_vt::screen::Screen::Alternate,
            "cursor": [a.cursor_x().unwrap(), a.cursor_y().unwrap()],
            "cursor_visible": a.is_cursor_visible().unwrap(),
            "title": a.title().unwrap_or(""),
            "plain": String::from_utf8_lossy(&format(&a, Format::Plain, false)),
            "modes": {
                "mouse1006": a.mode(Mode::new(1006, ModeKind::Dec)).unwrap(),
                "bracketed_paste": a.mode(Mode::new(2004, ModeKind::Dec)).unwrap(),
                "app_cursor": a.mode(Mode::new(1, ModeKind::Dec)).unwrap(),
                "app_keypad": a.mode(Mode::new(66, ModeKind::Dec)).unwrap(),
                "focus": a.mode(Mode::new(1004, ModeKind::Dec)).unwrap(),
            },
        });
        fs::write(dir.join(format!("{name}.expect.json")), serde_json::to_vec_pretty(&expect).unwrap()).unwrap();

        let mut b = new_term(a.cols().unwrap(), a.rows().unwrap());
        b.vt_write(&snap);

        let (mut oa, mut ob) = (observe(&a), observe(&b));
        // The alt-screen gap: leave the alternate screen on both and compare
        // the primary screen and its scrollback underneath.
        if a.active_screen().unwrap() == libghostty_vt::screen::Screen::Alternate {
            a.vt_write(b"\x1b[?1049l");
            b.vt_write(b"\x1b[?1049l");
            for (k, v) in observe(&a) { oa.push((format!("after_exit_alt.{k}"), v)); }
            for (k, v) in observe(&b) { ob.push((format!("after_exit_alt.{k}"), v)); }
        }
        // Trailing blanks differ when the snapshot paints spaces explicitly.
        for o in [&mut oa, &mut ob] {
            for (k, v) in o.iter_mut() {
                if k.ends_with("plain") {
                    *v = v.lines().map(str::trim_end).collect::<Vec<_>>().join("\n").trim_end().to_string();
                }
            }
        }
        // VT text can differ while cells match (a background stored on the
        // cell vs the same background as SGR); cells are what you see.
        let grid_ok = |o: &Vec<(String, String)>, p: &Vec<(String, String)>, pre: &str| {
            let g = |v: &Vec<(String, String)>| v.iter().find(|(k, _)| k == &format!("{pre}active_grid")).map(|x| x.1.clone());
            g(o) == g(p)
        };
        let diffs: Vec<_> = oa
            .iter()
            .zip(&ob)
            .filter(|(x, y)| x.1 != y.1)
            .filter(|(x, _)| {
                let pre = x.0.strip_suffix("vt");
                match pre {
                    Some(pre) if grid_ok(&oa, &ob, pre) && pre.is_empty() => {
                        println!("    note: VT text differs but every cell matches");
                        false
                    }
                    _ => true,
                }
            })
            .collect();
        println!(
            "{name:12} {:>7} B in -> {:>7} B snapshot in {snap_ms:.2} ms, screen={} scrollback A={} B={}  {}",
            bytes.len(),
            snap.len(),
            oa[1].1,
            oa[10].1,
            ob[10].1,
            if diffs.is_empty() { "OK" } else { "DIFF" }
        );
        for ((k, va), (_, vb)) in diffs {
            failures += 1;
            if k.ends_with("active_grid") {
                println!("    {k}: {}", grid_diff(va, vb));
            } else if k.ends_with("plain") || k.ends_with("vt") {
                println!("    {k}: {}", first_diff(va, vb));
            } else {
                println!("    {k}: A={va:?} B={vb:?}");
            }
        }
    }
    std::process::exit(if failures > 0 { 1 } else { 0 });
}
