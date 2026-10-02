//! S1/S5 follow-up: new fixtures through both snapshot paths.
//!
//! `run` (default): for each fixture in fixtures/, feed it into terminal A,
//! then
//!   (i)  GHOSTSNP: encode A, decode into B;
//!   (ii) wire: feed the same bytes into the daemon's `GhosttyEngine`, take
//!        its `snapshot()` (formatter + S1 fix-ups), and write that into a
//!        fresh libghostty terminal B;
//! and compare A and B. State that can't be read directly (saved cursor,
//! margins, scroll region, origin mode, protection) is compared through
//! probes: the same bytes sent to both afterwards, then everything compared
//! again. Wire snapshots and Ghostty's view of them go to work/out/ for the
//! xterm.js check.
//!
//! `encode <fixture> <out>` writes A's GHOSTSNP; `decode <fixture> <snap>`
//! decodes a GHOSTSNP (e.g. from another Ghostty build) and compares it with
//! A under every probe; `info` prints build_info. Used for the cross-build test.

use std::{fs, path::Path};

mod patched;

use illogical_vt::{GhosttyEngine, VtEngine, engine_tag};
use libghostty_vt::{
    RenderState, Terminal, build_info,
    fmt::{Format, Formatter, FormatterOptions},
    kitty::graphics::PlacementIterator,
    screen::{CellContentTag, Screen},
    snapshot::Decoder,
    style::StyleColor,
    terminal::{Mode, ModeKind, Point, PointCoordinate},
};

type Term = Terminal<'static, 'static>;

const SCROLLBACK: usize = 64 * 1024 * 1024;
const KITTY_LIMIT: u64 = 64 * 1024 * 1024;
const DEC_MODES: &[u16] =
    &[1, 5, 6, 7, 12, 25, 45, 47, 66, 69, 1000, 1002, 1003, 1004, 1006, 1047, 1049, 2004, 2026, 2027, 2031, 2048];

struct Fixture {
    name: String,
    bytes: Vec<u8>,
    cols: u16,
    rows: u16,
    resizes: Vec<(usize, u16, u16)>,
}

fn fixture(dir: &Path, name: &str) -> Fixture {
    let bytes = fs::read(dir.join(format!("{name}.bin"))).unwrap();
    let meta: serde_json::Value = serde_json::from_slice(&fs::read(dir.join(format!("{name}.json"))).unwrap()).unwrap();
    let n = |v: &serde_json::Value| v.as_u64().unwrap();
    Fixture {
        name: name.into(),
        bytes,
        cols: n(&meta["cols"]) as u16,
        rows: n(&meta["rows"]) as u16,
        resizes: meta["resizes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (n(&r["offset"]) as usize, n(&r["cols"]) as u16, n(&r["rows"]) as u16))
            .collect(),
    }
}

fn new_term(cols: u16, rows: u16) -> Term {
    let mut t = Terminal::new(cols, rows).unwrap();
    t.set_scrollback_max_bytes(Some(SCROLLBACK)).unwrap();
    t.set_continuation_max_bytes(1 << 20).unwrap();
    t.set_kitty_image_storage_limit(KITTY_LIMIT).unwrap();
    t.resize(cols, rows, 8, 16).unwrap();
    t
}

fn load(f: &Fixture) -> Term {
    let mut t = new_term(f.cols, f.rows);
    let mut pos = 0;
    for &(off, c, r) in &f.resizes {
        t.vt_write(&f.bytes[pos..off]);
        t.resize(c, r, 8, 16).unwrap();
        pos = off;
    }
    t.vt_write(&f.bytes[pos..]);
    t
}

fn load_engine(f: &Fixture) -> GhosttyEngine {
    let mut e = GhosttyEngine::new(f.cols, f.rows);
    let mut pos = 0;
    for &(off, c, r) in &f.resizes {
        e.feed(&f.bytes[pos..off]);
        e.resize(c, r);
        pos = off;
    }
    e.feed(&f.bytes[pos..]);
    e
}

fn format(t: &Term, f: Format) -> Vec<u8> {
    let o = FormatterOptions::new().with_format(f);
    Formatter::new(t, o).unwrap().format_alloc(None).unwrap().to_vec()
}

fn cell_key(t: &Term, x: u16, y: u16) -> String {
    cell_key_at(t, Point::Active(PointCoordinate { x, y: y.into() }))
}

/// A cell as it looks: empty equals a space, and a background stored on the
/// cell (from an erase) equals the same background set through SGR. Also
/// its protection (DECSCA) and hyperlink.
fn cell_key_at(t: &Term, p: Point) -> String {
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
    let ln = g.hyperlink_uri(&mut link).unwrap_or(0);
    let prot = if cell.is_protected().unwrap_or(false) { " PROTECTED" } else { "" };
    format!("{text:?}{}{prot}{}", compact(&style), if ln > 0 { format!(" link={}", String::from_utf8_lossy(&link[..ln])) } else { String::new() })
}

/// Non-default style fields only.
fn compact(s: &libghostty_vt::style::Style) -> String {
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
    for (on, k) in [(s.bold, "bold"), (s.italic, "italic"), (s.faint, "faint"), (s.blink, "blink"), (s.inverse, "inverse"),
        (s.invisible, "invisible"), (s.strikethrough, "strike"), (s.overline, "overline")] {
        if on {
            out += &format!(" {k}");
        }
    }
    if s.underline != libghostty_vt::style::Underline::None {
        out += &format!(" underline={:?}", s.underline);
    }
    out
}

/// Kitty images (by the ids the fixture uses) and placements on the active screen.
fn kitty(t: &Term) -> String {
    let Ok(g) = t.kitty_graphics() else { return "unavailable".into() };
    let mut out = Vec::new();
    for id in [7, 9] {
        if let Some(i) = g.image(id) {
            out.push(format!("image {id} {}x{}", i.width().unwrap(), i.height().unwrap()));
        }
    }
    let mut it = PlacementIterator::new().unwrap();
    let mut p = it.update(&g).unwrap();
    while let Some(pl) = p.next() {
        out.push(format!(
            "placement img={} p={} virtual={} {}x{}",
            pl.image_id().unwrap(),
            pl.placement_id().unwrap(),
            pl.is_virtual().unwrap(),
            pl.columns().unwrap(),
            pl.rows().unwrap()
        ));
    }
    out.sort();
    out.join("; ")
}

/// Everything observable, as (name, value) pairs.
fn observe(t: &Term) -> Vec<(String, String)> {
    let (cols, rows) = (t.cols().unwrap(), t.rows().unwrap());
    let mut v: Vec<(String, String)> = vec![
        ("size".into(), format!("{cols}x{rows}")),
        ("screen".into(), format!("{:?}", t.active_screen().unwrap())),
        ("cursor".into(), format!("{},{}", t.cursor_x().unwrap(), t.cursor_y().unwrap())),
        ("pending_wrap".into(), t.is_cursor_pending_wrap().unwrap().to_string()),
        ("cursor_visible".into(), t.is_cursor_visible().unwrap().to_string()),
        ("cursor_sgr".into(), compact(&t.cursor_style().unwrap())),
        ("kitty_keyboard".into(), format!("{:?}", t.kitty_keyboard_flags().unwrap())),
        ("title".into(), t.title().unwrap_or("").into()),
        ("pwd".into(), t.pwd().unwrap_or("").into()),
        ("scrollback".into(), t.scrollback_rows().unwrap().to_string()),
        ("palette".into(), format!("{:?}", t.color_palette().unwrap())),
        ("kitty_images".into(), kitty(t)),
    ];
    let mut rs = RenderState::new().unwrap();
    let snap = rs.update(t).unwrap();
    v.push(("cursor_shape".into(), format!("{:?}", snap.cursor_visual_style().unwrap())));
    v.push(("cursor_blink".into(), snap.cursor_blinking().unwrap().to_string()));
    for &m in DEC_MODES {
        v.push((format!("?{m}"), t.mode(Mode::new(m, ModeKind::Dec)).unwrap().to_string()));
    }
    for m in [4, 20] {
        v.push((format!("ansi {m}"), t.mode(Mode::new(m, ModeKind::Ansi)).unwrap().to_string()));
    }
    for y in 0..rows {
        let row: Vec<String> = (0..cols).map(|x| cell_key(t, x, y)).collect();
        v.push((format!("row {y}"), row.join("][")));
    }
    // Every cell of the scrollback too (styles, links), newest first so
    // that a missing row shows up at the right place.
    let hist = t.scrollback_rows().unwrap();
    for y in (0..hist).rev() {
        let row: Vec<String> =
            (0..cols).map(|x| cell_key_at(t, Point::History(PointCoordinate { x, y: y as u32 }))).collect();
        v.push((format!("history row -{}", hist - y), row.join("][")));
    }
    let plain = String::from_utf8_lossy(&format(t, Format::Plain)).into_owned();
    let plain = plain.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
    v.push(("plain".into(), plain.trim_end().to_string()));
    v
}

fn diffs(a: &[(String, String)], b: &[(String, String)]) -> Vec<String> {
    let missing = ("".to_string(), "(missing)".to_string());
    let bm: std::collections::HashMap<&str, &(String, String)> = b.iter().map(|x| (x.0.as_str(), x)).collect();
    let am: std::collections::HashSet<&str> = a.iter().map(|x| x.0.as_str()).collect();
    let extra: Vec<String> =
        b.iter().filter(|y| !am.contains(y.0.as_str())).map(|y| format!("{}: only in B", y.0)).take(3).collect();
    a.iter()
        .map(|x| (x, *bm.get(x.0.as_str()).unwrap_or(&&missing)))
        .filter(|(x, y)| x.1 != y.1)
        .map(|(x, y)| {
            if x.0.starts_with("row ") || x.0.starts_with("history row") {
                let (ca, cb): (Vec<_>, Vec<_>) = (x.1.split("][").collect(), y.1.split("][").collect());
                if cb.len() != ca.len() {
                    return format!("{}: A has it, B {}", x.0, y.1);
                }
                let i = ca.iter().zip(&cb).position(|(p, q)| p != q).unwrap_or(0);
                let n = ca.iter().zip(&cb).filter(|(p, q)| p != q).count();
                format!("{} ({n} cells, first col {i}): A={} B={}", x.0, ca[i], cb[i])
            } else if x.0 == "plain" {
                let (la, lb): (Vec<_>, Vec<_>) = (x.1.lines().collect(), y.1.lines().collect());
                let i = (0..la.len().max(lb.len())).find(|&i| la.get(i) != lb.get(i)).unwrap_or(0);
                format!(
                    "{}: {} vs {} lines, first diff line {i}: A={:.100?} B={:.100?}",
                    x.0,
                    la.len(),
                    lb.len(),
                    la.get(i).unwrap_or(&""),
                    lb.get(i).unwrap_or(&"")
                )
            } else {
                format!("{}: A={:.160} B={:.160}", x.0, x.1, y.1)
            }
        })
        .chain(extra)
        .collect()
}

/// Bytes sent to both terminals after the restore, each from a fresh pair.
fn probes() -> Vec<(&'static str, Vec<u8>)> {
    let mut wrap = b"\x1b[H".to_vec();
    wrap.extend("0123456789".repeat(16).bytes());
    wrap.extend_from_slice(b"\x1b[99B\n\n\nEND\x1b[2L");
    vec![
        ("as restored", vec![]),
        ("DECRC, print", b"\x1b8q@".to_vec()),
        ("home, long line, LFs at bottom, IL", wrap),
        ("selective erase (DECSED)", b"\x1b[?2J".to_vec()),
        ("1049l, DECRC, print", b"\x1b[?1049lq@\x1b8q@".to_vec()),
        ("47l, DECRC, print", b"\x1b[?47l\x1b8q@".to_vec()),
    ]
}

/// Compare A and B under every probe. `make_b` builds a fresh B each time.
fn compare(f: &Fixture, make_b: &dyn Fn() -> Term) -> Vec<(&'static str, Vec<String>)> {
    probes()
        .into_iter()
        .map(|(name, bytes)| {
            let (mut a, mut b) = (load(f), make_b());
            a.vt_write(&bytes);
            b.vt_write(&bytes);
            (name, diffs(&observe(&a), &observe(&b)))
        })
        .collect()
}

fn report(path: &str, r: &[(&str, Vec<String>)]) -> bool {
    let bad: Vec<_> = r.iter().filter(|(_, d)| !d.is_empty()).collect();
    if bad.is_empty() {
        println!("  {path:9} OK under all {} probes", r.len());
        return true;
    }
    println!("  {path:9} DIFF under {}/{} probes", bad.len(), r.len());
    for (probe, d) in bad {
        println!("    [{probe}] {} differences", d.len());
        for x in d.iter().take(8) {
            println!("      {x}");
        }
        if d.len() > 8 {
            println!("      ... {} more", d.len() - 8);
        }
    }
    false
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            (p.extension()? == "bin").then(|| p.file_stem().unwrap().to_string_lossy().into())
        })
        .collect();
    v.sort();
    v
}

fn info() {
    println!("engine_tag()              {}", engine_tag());
    println!("version_string            {:?}", build_info::version_string());
    println!("version_build             {:?}", build_info::build_version());
    println!("version_pre               {:?}", build_info::pre_version());
    println!("kitty graphics compiled   {:?}", build_info::supports_kitty_graphics());
    let t = Terminal::new(10, 2).unwrap();
    println!("default kitty image limit {:?}", t.kitty_image_storage_limit());
}

fn ghostty_view(t: &Term) -> serde_json::Value {
    serde_json::json!({
        "cols": t.cols().unwrap(), "rows": t.rows().unwrap(),
        "alt": t.active_screen().unwrap() == Screen::Alternate,
        "cursor": [t.cursor_x().unwrap(), t.cursor_y().unwrap()],
        "plain": String::from_utf8_lossy(&format(t, Format::Plain)),
    })
}

fn main() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = here.join("fixtures");
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("info") => return info(),
        Some("bench") => {
            // Median of 20 snapshot() calls: the crate's vs the patched one.
            let med = |mut v: Vec<f64>| {
                v.sort_by(f64::total_cmp);
                v[v.len() / 2]
            };
            for name in ["s1_seq", "s1_nvim", "s1_top", "claude", "decsc_1049"] {
                let f = fixture(&dir, name);
                let mut e = load_engine(&f);
                let t0 = med((0..20).map(|_| { let s = std::time::Instant::now(); e.snapshot(); s.elapsed().as_secs_f64() * 1e3 }).collect());
                let mut p = patched::Patched::new(f.cols, f.rows);
                let mut pos = 0;
                for &(off, c, r) in &f.resizes {
                    p.feed(&f.bytes[pos..off]);
                    p.resize(c, r);
                    pos = off;
                }
                p.feed(&f.bytes[pos..]);
                let t1 = med((0..20).map(|_| { let s = std::time::Instant::now(); p.snapshot(); s.elapsed().as_secs_f64() * 1e3 }).collect());
                println!("{name:12} crate {t0:.3} ms  patched {t1:.3} ms");
            }
            return;
        }
        Some("kittyq") => {
            // What the daemon's engine answers to a Kitty graphics query
            // (what chafa, yazi, timg send to detect support).
            let mut e = GhosttyEngine::new(80, 24);
            e.feed(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c");
            println!("reply to a=q + DA1: {:?}", String::from_utf8_lossy(&e.take_replies()));
            e.feed(b"\x1bPq#0;2;0;0;0#0~~~~\x1b\\\x1b[?2;1;0S");
            println!("reply to sixel + XTSMGRAPHICS: {:?}", String::from_utf8_lossy(&e.take_replies()));
            return;
        }
        Some("encode") => {
            let a = load(&fixture(&dir, &args[1]));
            fs::write(&args[2], a.encode_snapshot_alloc(None).unwrap().unwrap().to_vec()).unwrap();
            return;
        }
        Some("decode") => {
            let f = fixture(&dir, &args[1]);
            let snap = fs::read(&args[2]).unwrap();
            let stage = Decoder::new_buf(&snap).map(|_| ()).map_err(|e| format!("Decoder::new_buf: {e:?}"))
                .and_then(|_| Decoder::new_buf(&snap).unwrap().ready::<'static>().map(|_| ()).map_err(|e| format!("ready(): {e:?}")));
            println!("{}: stages: {}", f.name, match &stage { Ok(()) => "new_buf ok, ready ok".into(), Err(e) => e.clone() });
            match Decoder::new_buf(&snap).and_then(|d| d.decode::<'static>()) {
                Err(e) => println!("{}: decode failed: {e:?}", f.name),
                Ok(_) => {
                    let r = compare(&f, &|| Decoder::new_buf(&snap).unwrap().decode().unwrap());
                    println!("{}: decoded", f.name);
                    report("foreign", &r);
                }
            }
            return;
        }
        _ => {}
    }
    info();
    let only: Vec<String> = args.into_iter().filter(|a| a != "run").collect();
    let out = here.join("work/out");
    fs::create_dir_all(&out).unwrap();
    let mut summary = Vec::new();
    for name in names(&dir) {
        if !only.is_empty() && !only.contains(&name) {
            continue;
        }
        let f = fixture(&dir, &name);
        let a = load(&f);
        println!("\n== {name} ({} bytes, ends {}x{} on {:?})", f.bytes.len(), a.cols().unwrap(), a.rows().unwrap(), a.active_screen().unwrap());

        // (i) GHOSTSNP
        let a2 = load(&f);
        let before = observe(&a2);
        let snap = a2.encode_snapshot_alloc(None).unwrap().unwrap().to_vec();
        let changed = diffs(&before, &observe(&a2));
        if !changed.is_empty() {
            println!("  encode changed the source: {changed:?}");
        }
        let r = compare(&f, &|| Decoder::new_buf(&snap).unwrap().decode().unwrap());
        let snp_ok = report("GHOSTSNP", &r) && changed.is_empty();

        // (ii) wire: the daemon's snapshot()
        let mut e = load_engine(&f);
        let wire = e.snapshot();
        let (c, rr) = e.size();
        let r = compare(&f, &|| {
            let mut b = new_term(c, rr);
            b.vt_write(&wire);
            b
        });
        let wire_ok = report("wire", &r);

        // (iii) wire with this spike's fixes (src/patched.rs)
        let mut p = patched::Patched::new(f.cols, f.rows);
        let mut pos = 0;
        for &(off, c, r) in &f.resizes {
            p.feed(&f.bytes[pos..off]);
            p.resize(c, r);
            pos = off;
        }
        p.feed(&f.bytes[pos..]);
        let pbefore = observe(&p.term);
        let fixed = p.snapshot();
        let pchanged = diffs(&pbefore, &observe(&p.term));
        if !pchanged.is_empty() {
            println!("  patched snapshot changed the source: {pchanged:?}");
        }
        let r = compare(&f, &|| {
            let mut b = new_term(c, rr);
            b.vt_write(&fixed);
            b
        });
        let fixed_ok = report("wire+fix", &r) && pchanged.is_empty();
        fs::write(out.join(format!("{name}.patched.snap")), &fixed).unwrap();
        println!(
            "  sizes: GHOSTSNP {} B, wire {} B, wire+fix {} B ({} cells repainted)",
            snap.len(),
            wire.len(),
            fixed.len(),
            p.patched_cells
        );

        // For the xterm.js check: the wire snapshot, and Ghostty's view of
        // A and of B under each probe.
        fs::write(out.join(format!("{name}.snap")), &wire).unwrap();
        let mut views = Vec::new();
        for (probe, bytes) in probes() {
            let mut a = load(&f);
            let mut b = new_term(c, rr);
            b.vt_write(&wire);
            a.vt_write(&bytes);
            b.vt_write(&bytes);
            views.push(serde_json::json!({"probe": probe, "bytes": String::from_utf8_lossy(&bytes),
                "ghostty_a": ghostty_view(&a), "ghostty_b": ghostty_view(&b)}));
        }
        let meta = serde_json::json!({"cols": f.cols, "rows": f.rows,
            "resizes": f.resizes.iter().map(|r| serde_json::json!({"offset": r.0, "cols": r.1, "rows": r.2})).collect::<Vec<_>>(),
            "probes": views});
        fs::write(out.join(format!("{name}.expect.json")), serde_json::to_string_pretty(&meta).unwrap()).unwrap();
        summary.push((name, snp_ok, wire_ok, fixed_ok));
    }
    println!("\n== summary (ghostty -> ghostty)");
    let ok = |b: bool| if b { "OK" } else { "DIFF" };
    for (n, s, w, x) in summary {
        println!("{n:16} GHOSTSNP {:5} wire {:5} wire+fix {}", ok(s), ok(w), ok(x));
    }
}
