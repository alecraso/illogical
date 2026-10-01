//! S5: can Ghostty's own snapshot format (GHOSTSNP, `ghostty_snapshot_*`)
//! replace the formatter + fix-ups for the daemon's checkpoints?
//!
//! For each S1 fixture: feed it into terminal A, encode a snapshot, decode it
//! into B (no fix-ups of any kind), and compare everything observable,
//! including the primary screen under a full-screen app. Then measure size
//! and time against the formatter, time to READY on an incremental decode,
//! continuation across a split escape sequence, and corruption detection.

use std::{fs, path::Path, time::Instant};

use libghostty_vt::{
    RenderState, Terminal,
    fmt::{Format, Formatter, FormatterOptions},
    screen::{CellContentTag, Screen},
    snapshot::Decoder,
    style::StyleColor,
    terminal::{Mode, ModeKind, Point, PointCoordinate},
};

type Term = Terminal<'static, 'static>;

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

const SCROLLBACK: usize = 64 * 1024 * 1024;

fn new_term(cols: u16, rows: u16) -> Term {
    let mut t = Terminal::new(cols, rows).unwrap();
    t.set_scrollback_max_bytes(Some(SCROLLBACK)).unwrap();
    t
}

fn load(dir: &Path, name: &str) -> (Term, Vec<u8>) {
    let bytes = fs::read(dir.join(format!("{name}.bin"))).unwrap();
    let meta: Meta = serde_json::from_slice(&fs::read(dir.join(format!("{name}.json"))).unwrap()).unwrap();
    let mut t = new_term(meta.cols, meta.rows);
    let mut pos = 0;
    for r in &meta.resizes {
        t.vt_write(&bytes[pos..r.offset]);
        t.resize(r.cols, r.rows, 8, 16).unwrap();
        pos = r.offset;
    }
    t.vt_write(&bytes[pos..]);
    (t, bytes)
}

fn format(t: &Term, f: Format, extras: bool) -> Vec<u8> {
    let mut o = FormatterOptions::new().with_format(f).with_modes(extras);
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
    Formatter::new(t, o).unwrap().format_alloc(None).unwrap().to_vec()
}

fn cell_key(t: &Term, x: u16, y: u16) -> String {
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
fn observe(t: &Term) -> Vec<(String, String)> {
    let (cols, rows) = (t.cols().unwrap(), t.rows().unwrap());
    let mut v: Vec<(String, String)> = vec![
        ("size".into(), format!("{cols}x{rows}")),
        ("screen".into(), format!("{:?}", t.active_screen().unwrap())),
        ("cursor".into(), format!("{},{}", t.cursor_x().unwrap(), t.cursor_y().unwrap())),
        ("pending_wrap".into(), t.is_cursor_pending_wrap().unwrap().to_string()),
        ("cursor_visible".into(), t.is_cursor_visible().unwrap().to_string()),
        ("cursor_sgr".into(), format!("{:?}", t.cursor_style().unwrap())),
        ("kitty".into(), format!("{:?}", t.kitty_keyboard_flags().unwrap())),
        ("title".into(), t.title().unwrap_or("").into()),
        ("pwd".into(), t.pwd().unwrap_or("").into()),
        ("scrollback".into(), t.scrollback_rows().unwrap().to_string()),
        ("palette".into(), format!("{:?}", t.color_palette().unwrap())),
    ];
    let mut rs = RenderState::new().unwrap();
    let snap = rs.update(t).unwrap();
    v.push(("cursor_shape".into(), format!("{:?}", snap.cursor_visual_style().unwrap())));
    v.push(("cursor_blink".into(), snap.cursor_blinking().unwrap().to_string()));
    for m in [1, 6, 7, 12, 25, 47, 66, 69, 1000, 1002, 1003, 1004, 1006, 1047, 1049, 2004, 2026] {
        v.push((format!("?{m}"), t.mode(Mode::new(m, ModeKind::Dec)).unwrap().to_string()));
    }
    for y in 0..rows {
        let row: Vec<String> = (0..cols).map(|x| cell_key(t, x, y)).collect();
        v.push((format!("row {y}"), row.join("][")));
    }
    v.push(("plain".into(), String::from_utf8_lossy(&format(t, Format::Plain, false)).into()));
    v.push(("vt".into(), String::from_utf8_lossy(&format(t, Format::Vt, false)).into()));
    v
}

fn diffs(a: &[(String, String)], b: &[(String, String)], prefix: &str) -> Vec<String> {
    a.iter()
        .zip(b)
        .filter(|(x, y)| x.1 != y.1)
        .map(|(x, y)| format!("{prefix}{}: A={:.120?} B={:.120?}", x.0, x.1, y.1))
        .collect()
}

/// The "saved cursor" (DECSC) isn't exposed; check it behaviourally: restore
/// it on both and compare where the cursor lands.
fn saved_cursor(t: &mut Term) -> String {
    t.vt_write(b"\x1b8");
    format!("{},{}", t.cursor_x().unwrap(), t.cursor_y().unwrap())
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            (p.extension()? == "bin").then(|| p.file_stem().unwrap().to_string_lossy().into())
        })
        .collect();
    names.sort();

    println!("== fidelity: GHOSTSNP round trip, no fix-ups");
    let mut failures = 0;
    for name in &names {
        let (mut a, _) = load(&dir, name);
        let before = observe(&a);
        let snap = a.encode_snapshot_alloc(None).unwrap().unwrap().to_vec();
        let changed = diffs(&before, &observe(&a), "encode changed source: ");
        let mut b: Term = Decoder::new_buf(&snap).unwrap().decode().unwrap();
        let mut d = changed;
        d.extend(diffs(&observe(&a), &observe(&b), ""));
        if a.active_screen().unwrap() == Screen::Alternate {
            a.vt_write(b"\x1b[?1049l");
            b.vt_write(b"\x1b[?1049l");
            d.extend(diffs(&observe(&a), &observe(&b), "after leaving alt: "));
        }
        let (sa, sb) = (saved_cursor(&mut a), saved_cursor(&mut b));
        if sa != sb {
            d.push(format!("saved cursor: A={sa} B={sb}"));
        }
        println!("{name:12} {}", if d.is_empty() { "OK" } else { "DIFF" });
        for x in &d {
            println!("    {x}");
        }
        failures += d.len();
    }

    println!("\n== size and time (median of 20), GHOSTSNP vs formatter VT");
    println!("{:12} {:>9} {:>9} {:>9} | {:>9} {:>9} {:>9}", "", "snap B", "enc ms", "dec ms", "fmt B", "fmt ms", "replay ms");
    let mut big = new_term(120, 40);
    for i in 0..200_000 {
        big.vt_write(format!("\x1b[3{}mline {i:06}\x1b[0m some ordinary output text here\r\n", i % 8).as_bytes());
    }
    let mut cases: Vec<(String, Term)> = names.iter().map(|n| (n.clone(), load(&dir, n).0)).collect();
    cases.push(("200k lines".into(), big));
    for (name, t) in &mut cases {
        let med = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let mut snap = Vec::new();
        let enc = med((0..20)
            .map(|_| {
                let s = Instant::now();
                snap = t.encode_snapshot_alloc(None).unwrap().unwrap().to_vec();
                ms(s)
            })
            .collect());
        let dec = med((0..20)
            .map(|_| {
                let s = Instant::now();
                let _t: Term = Decoder::new_buf(&snap).unwrap().decode().unwrap();
                ms(s)
            })
            .collect());
        let mut vt = Vec::new();
        let fmt = med((0..20)
            .map(|_| {
                let s = Instant::now();
                vt = format(t, Format::Vt, true);
                ms(s)
            })
            .collect());
        let (c, r) = (t.cols().unwrap(), t.rows().unwrap());
        let replay = med((0..5)
            .map(|_| {
                let mut x = new_term(c, r);
                let s = Instant::now();
                x.vt_write(&vt);
                ms(s)
            })
            .collect());
        println!(
            "{name:12} {:>9} {enc:>9.3} {dec:>9.3} | {:>9} {fmt:>9.3} {replay:>9.3}",
            snap.len(),
            vt.len()
        );
    }

    println!("\n== incremental: time to READY, then history pages");
    let (_, big) = cases.pop().unwrap();
    let snap = big.encode_snapshot_alloc(None).unwrap().unwrap().to_vec();
    if let Ok(dir) = std::env::var("DUMP") {
        fs::write(format!("{dir}/big.snap"), &snap).unwrap();
        fs::write(format!("{dir}/big.vt"), format(&big, Format::Vt, true)).unwrap();
    }
    let s = Instant::now();
    let dec = Decoder::new_buf(&snap).unwrap();
    let total_history = dec.history_rows_primary().ok();
    let mut inc = dec.ready().unwrap();
    let ready_ms = ms(s);
    let visible_ok = observe(inc.terminal()).iter().filter(|(k, _)| k.starts_with("row ")).collect::<Vec<_>>()
        == observe(&big).iter().filter(|(k, _)| k.starts_with("row ")).collect::<Vec<_>>();
    let rows_at_ready = inc.terminal().scrollback_rows().unwrap();
    let mut pages = 0;
    while let Some(p) = inc.next().unwrap() {
        let _ = p.rows();
        pages += 1;
    }
    let all_ms = ms(s);
    let done = inc.into_terminal();
    println!(
        "READY after {ready_ms:.3} ms of a {} byte snapshot; visible screen identical to source: {visible_ok}; \
         scrollback rows at READY {rows_at_ready}, history declared {total_history:?}",
        snap.len()
    );
    println!(
        "all {pages} history pages after {all_ms:.3} ms; scrollback rows {} (source {}); full text identical: {}",
        done.scrollback_rows().unwrap(),
        big.scrollback_rows().unwrap(),
        format(&done, Format::Plain, false) == format(&big, Format::Plain, false)
    );

    println!("\n== continuation: snapshot in the middle of an escape sequence");
    let mut a = new_term(80, 24);
    a.set_continuation_max_bytes(1 << 20).unwrap();
    a.vt_write(b"hello \x1b[1;3");
    let snap = a.encode_snapshot_alloc(None).unwrap().unwrap().to_vec();
    let mut b: Term = Decoder::new_buf(&snap).unwrap().decode().unwrap();
    a.vt_write(b"1mred\x1b[0m world");
    b.vt_write(b"1mred\x1b[0m world");
    let same = diffs(&observe(&a), &observe(&b), "");
    println!("split CSI resumes identically: {}", if same.is_empty() { "yes".into() } else { format!("NO {same:?}") });
    let mut c = new_term(80, 24);
    c.vt_write(b"hello \x1b[1;3");
    println!("encoding mid-sequence without tracking: {:?}", c.encode_snapshot_alloc(None).map(|b| b.map(|b| b.to_vec().len())));

    println!("\n== corruption");
    let (a, _) = load(&dir, "modes");
    let mut snap = a.encode_snapshot_alloc(None).unwrap().unwrap().to_vec();
    let mid = snap.len() / 2;
    snap[mid] ^= 0x40;
    println!("one flipped byte: {:?}", Decoder::new_buf(&snap).and_then(|d| d.decode::<'static>().map(|_| ())));
    println!("truncated: {:?}", Decoder::new_buf(&snap[..mid]).and_then(|d| d.decode::<'static>().map(|_| ())));
    println!("magic: {:?}", String::from_utf8_lossy(&snap[..8]));

    std::process::exit(if failures > 0 { 1 } else { 0 });
}
