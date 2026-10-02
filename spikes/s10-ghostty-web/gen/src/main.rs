//! S10 data: for each S1 fixture, the daemon engine's plain_text() (the
//! reference), its formatter VT snapshot (what the web client gets today)
//! and the raw GHOSTSNP bytes. Plus three attach cases: a small screen,
//! 10k lines of scrollback and 200k lines (64k kept, like S5).
use illogical_vt::{GhosttyEngine, VtEngine};
use libghostty_vt::{
    Terminal,
    screen::{CellContentTag, CellWide, Screen},
    style::{StyleColor, Underline},
    terminal::{Point, PointCoordinate},
};
use std::{fs, time::Instant};

fn color(c: StyleColor) -> serde_json::Value {
    match c {
        StyleColor::None => serde_json::Value::Null,
        StyleColor::Palette(i) => (i.0 as u64).into(),
        StyleColor::Rgb(c) => format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b).into(),
    }
}

/// The visible grid of the same engine (libghostty-vt at the daemon's
/// rev), replayed the same way: text, width and style per cell.
fn cells(bytes: &[u8], meta: &serde_json::Value) -> serde_json::Value {
    let mut t: Terminal<'static, 'static> =
        Terminal::new(meta["cols"].as_u64().unwrap() as u16, meta["rows"].as_u64().unwrap() as u16).unwrap();
    t.set_scrollback_max_bytes(Some(64 * 1024 * 1024)).unwrap();
    let mut pos = 0;
    for r in meta["resizes"].as_array().unwrap() {
        let off = r["offset"].as_u64().unwrap() as usize;
        t.vt_write(&bytes[pos..off]);
        t.resize(r["cols"].as_u64().unwrap() as u16, r["rows"].as_u64().unwrap() as u16, 8, 16).unwrap();
        pos = off;
    }
    t.vt_write(&bytes[pos..]);
    let (cols, rows) = (t.cols().unwrap(), t.rows().unwrap());
    let mut out = Vec::new();
    for y in 0..rows {
        let mut row = Vec::new();
        for x in 0..cols {
            let g = t.grid_ref(Point::Active(PointCoordinate { x, y: y.into() })).unwrap();
            let mut buf = ['\0'; 16];
            let n = g.graphemes(&mut buf).unwrap_or(0);
            let text: String = buf[..n].iter().collect();
            let cell = g.cell().unwrap();
            let mut st = g.style().unwrap();
            if matches!(st.bg_color, StyleColor::None) {
                match cell.content_tag().unwrap() {
                    CellContentTag::BgColorPalette => st.bg_color = StyleColor::Palette(cell.bg_color_palette().unwrap()),
                    CellContentTag::BgColorRgb => st.bg_color = StyleColor::Rgb(cell.bg_color_rgb().unwrap()),
                    _ => {}
                }
            }
            let wide = match cell.wide().unwrap() {
                CellWide::Narrow => 1,
                CellWide::Wide => 2,
                _ => 0,
            };
            row.push(serde_json::json!([text, wide, st.bold, st.italic, st.faint, !matches!(st.underline, Underline::None),
                st.inverse, st.strikethrough, color(st.fg_color), color(st.bg_color)]));
        }
        out.push(serde_json::Value::Array(row));
    }
    serde_json::json!({
        "cursor": [t.cursor_x().unwrap(), t.cursor_y().unwrap()],
        "cursor_visible": t.is_cursor_visible().unwrap(),
        "alt": t.active_screen().unwrap() == Screen::Alternate,
        "rows": out,
    })
}

fn ghostsnp(e: &GhosttyEngine) -> Vec<u8> {
    let ck = e.checkpoint();
    // ILLOGICAL-CKPT1\n<engine tag>\n<zstd>
    let mut nl = ck.iter().enumerate().filter(|(_, b)| **b == b'\n').map(|(i, _)| i);
    nl.next();
    let body = &ck[nl.next().unwrap() + 1..];
    zstd::decode_all(body).unwrap()
}

fn dump(out: &str, name: &str, e: &mut GhosttyEngine, meta: serde_json::Value) {
    let t = Instant::now();
    let snap = e.snapshot();
    let snap_ms = t.elapsed().as_secs_f64() * 1000.0;
    let g = ghostsnp(e);
    let (cols, rows) = e.size();
    fs::write(format!("{out}/{name}.vt"), &snap).unwrap();
    fs::write(format!("{out}/{name}.ghostsnp"), &g).unwrap();
    fs::write(format!("{out}/{name}.plain"), e.plain_text()).unwrap();
    let mut m = meta;
    m["final_cols"] = cols.into();
    m["final_rows"] = rows.into();
    m["alt"] = e.alt_screen().into();
    fs::write(format!("{out}/{name}.meta.json"), serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    println!("{name:14} {cols}x{rows} vt {:>9} B ({snap_ms:.1} ms)  ghostsnp {:>9} B", snap.len(), g.len());
}

fn main() {
    let fx = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../crates/vt/fixtures");
    let out = concat!(env!("CARGO_MANIFEST_DIR"), "/../work/data");
    for name in ["seq", "modes", "nvim", "nvim_resize", "less", "top", "resize"] {
        let bytes = fs::read(format!("{fx}/{name}.bin")).unwrap();
        let meta: serde_json::Value = serde_json::from_slice(&fs::read(format!("{fx}/{name}.json")).unwrap()).unwrap();
        let mut e = GhosttyEngine::new(meta["cols"].as_u64().unwrap() as u16, meta["rows"].as_u64().unwrap() as u16);
        let mut pos = 0;
        for r in meta["resizes"].as_array().unwrap() {
            let off = r["offset"].as_u64().unwrap() as usize;
            e.feed(&bytes[pos..off]);
            e.resize(r["cols"].as_u64().unwrap() as u16, r["rows"].as_u64().unwrap() as u16);
            pos = off;
        }
        e.feed(&bytes[pos..]);
        fs::write(format!("{out}/{name}.cells.json"), serde_json::to_vec(&cells(&bytes, &meta)).unwrap()).unwrap();
        fs::copy(format!("{fx}/{name}.bin"), format!("{out}/{name}.bin")).unwrap();
        dump(out, name, &mut e, meta);
    }
    // Attach cases at a desktop-ish 120x40 (the phone renders the pane's real size).
    let line = |i: usize| format!("\x1b[3{}mline {i:06}\x1b[0m some ordinary output text here\r\n", i % 8);
    for (name, n) in [("attach_small", 20usize), ("attach_10k", 10_000), ("attach_64k", 200_000)] {
        let mut e = GhosttyEngine::new(120, 40);
        for i in 0..n {
            e.feed(line(i).as_bytes());
        }
        e.feed(b"\x1b[1;32mjake@geek\x1b[0m:\x1b[1;34m~\x1b[0m$ ");
        dump(out, name, &mut e, serde_json::json!({"cols": 120, "rows": 40, "resizes": [], "lines": n}));
    }
}
