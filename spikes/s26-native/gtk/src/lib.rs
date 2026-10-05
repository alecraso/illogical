//! S26 B2 on Linux: a pane drawn natively with GTK4, from a client-side
//! libghostty-vt terminal. `s26-gtk` (main.rs) shows one pane or runs the
//! bench; `s26-adw` (bin/adw.rs) is the hybrid window.
//!
//! Drawing goes through GTK's scene graph (GSK, on the GPU): each row is a
//! cached render node, rebuilt only when libghostty's render state marks
//! the row dirty. Text runs are Pango layouts; cells outside ASCII are laid
//! out one by one at their column so wide characters stay on the grid.

use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    time::Instant,
};

use gtk::{gdk, glib, graphene, gsk, pango, prelude::*, subclass::prelude::*};
use s26_core::{
    Event, Pane, Sender,
    libghostty_vt::{
        key::Mods,
        render::{CellIterator, CursorVisualStyle, Dirty, RenderState, RowIterator},
        style::RgbColor,
    },
};

pub const PAD: f32 = 6.0;
const FONT: &str = "JetBrains Mono, DejaVu Sans Mono, monospace 11";

pub struct State {
    pane: Pane,
    render: RenderState<'static>,
    rows_it: RowIterator<'static>,
    cells_it: CellIterator<'static>,
    cache: Vec<Option<gsk::RenderNode>>,
    font: pango::FontDescription,
    bold: pango::FontDescription,
    pub cell_w: f32,
    pub cell_h: f32,
    pub grid: (u16, u16),
    pub sender: Option<Sender>,
    preedit: String,
    /// Rows rebuilt over the life of the view, for the README.
    rebuilt: u64,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TermView {
        pub st: RefCell<Option<State>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TermView {
        const NAME: &'static str = "S26TermView";
        type Type = super::TermView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for TermView {}

    impl WidgetImpl for TermView {
        fn snapshot(&self, s: &gtk::Snapshot) {
            if std::env::var_os("S26_DEBUG").is_some() {
                eprintln!("s26: snapshot");
            }
            if let Some(st) = self.st.borrow_mut().as_mut() {
                super::draw(&self.obj(), st, s);
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            if let Some(st) = self.st.borrow_mut().as_mut() {
                let cols = (((width as f32 - 2.0 * PAD) / st.cell_w).floor() as u16).max(2);
                let rows = (((height as f32 - 2.0 * PAD) / st.cell_h).floor() as u16).max(2);
                if (cols, rows) != st.grid {
                    st.grid = (cols, rows);
                    st.pane.resize(cols, rows, st.cell_w as u32, st.cell_h as u32);
                    st.cache.clear();
                    if let Some(tx) = &st.sender {
                        tx.view(cols, rows);
                    }
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct TermView(ObjectSubclass<imp::TermView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

fn rgba(c: RgbColor) -> gdk::RGBA {
    gdk::RGBA::new(c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0, 1.0)
}

fn draw(w: &TermView, st: &mut State, s: &gtk::Snapshot) {
    let State { pane, render, rows_it, cells_it, cache, font, bold, cell_w, cell_h, preedit, rebuilt, .. } = st;
    let (cw, ch) = (*cell_w, *cell_h);
    let Ok(snap) = render.update(&pane.term) else { return };
    let Ok(colors) = snap.colors() else { return };
    let full = matches!(snap.dirty(), Ok(Dirty::Full));
    let (wd, ht) = (w.width() as f32, w.height() as f32);
    s.append_color(&rgba(colors.background), &graphene::Rect::new(0.0, 0.0, wd, ht));

    let Ok(mut rows) = rows_it.update(&snap) else { return };
    let mut y = 0usize;
    let mut text = String::new();
    while let Some(row) = rows.next() {
        if cache.len() <= y {
            cache.resize(y + 1, None);
        }
        if full || cache[y].is_none() || row.dirty().unwrap_or(true) {
            // One row, in its own coordinates: backgrounds, then text runs.
            let rs = gtk::Snapshot::new();
            let bgs = gtk::Snapshot::new();
            let Ok(mut cells) = cells_it.update(row) else { break };
            let mut x = 0u16;
            let mut run = String::new();
            let mut run_start = 0u16;
            let mut run_style: Option<(RgbColor, bool, bool)> = None;
            let mut bg_run: Option<(RgbColor, u16)> = None;
            let fill = |rs: &gtk::Snapshot, c: RgbColor, from: u16, to: u16| {
                let x0 = (PAD + from as f32 * cw).round();
                let x1 = (PAD + to as f32 * cw).round();
                rs.append_color(&rgba(c), &graphene::Rect::new(x0, 0.0, x1 - x0, ch));
            };
            let flush = |rs: &gtk::Snapshot, run: &mut String, start: u16, style: Option<(RgbColor, bool, bool)>| {
                if let (false, Some((fg, b, it))) = (run.is_empty(), style) {
                    let layout = w.create_pango_layout(Some(run));
                    let mut fd = if b { bold.clone() } else { font.clone() };
                    if it {
                        fd.set_style(pango::Style::Italic);
                    }
                    layout.set_font_description(Some(&fd));
                    rs.save();
                    rs.translate(&graphene::Point::new(PAD + start as f32 * cw, 0.0));
                    rs.append_layout(&layout, &rgba(fg));
                    rs.restore();
                }
                run.clear();
            };
            while let Some(cell) = cells.next() {
                let n = cell.graphemes_len().unwrap_or(0);
                let bg = cell.bg_color().ok().flatten();
                let mut fg = cell.fg_color().ok().flatten().unwrap_or(colors.foreground);
                let (mut b, mut it, mut inv) = (false, false, false);
                if cell.has_styling().unwrap_or(false)
                    && let Ok(style) = cell.style()
                {
                    b = style.bold;
                    it = style.italic;
                    inv = style.inverse;
                }
                let mut bgc = bg;
                if inv || cell.is_selected().unwrap_or(false) {
                    bgc = Some(fg);
                    fg = bg.unwrap_or(colors.background);
                }
                // Backgrounds as merged, pixel-aligned runs: one rectangle per
                // cell at a fractional width leaves antialiased seams.
                if bgc != bg_run.map(|(c, _)| c) {
                    if let Some((c, start)) = bg_run.take() {
                        fill(&bgs, c, start, x);
                    }
                    bg_run = bgc.map(|c| (c, x));
                }
                if n == 0 {
                    // Blank, or the tail of a wide character: ends a run.
                    flush(&rs, &mut run, run_start, run_style);
                } else {
                    let _ = cell.graphemes_utf8(&mut text);
                    let ascii = text.is_ascii();
                    let style = Some((fg, b, it));
                    if !ascii || style != run_style || run.is_empty() && x != run_start {
                        flush(&rs, &mut run, run_start, run_style);
                    }
                    if run.is_empty() {
                        run_start = x;
                        run_style = style;
                    }
                    run.push_str(&text);
                    if !ascii {
                        flush(&rs, &mut run, run_start, run_style);
                    }
                }
                x += 1;
                if run.is_empty() {
                    run_start = x;
                }
            }
            flush(&rs, &mut run, run_start, run_style);
            if let Some((c, start)) = bg_run.take() {
                fill(&bgs, c, start, x);
            }
            // Backgrounds under text.
            let row_snap = gtk::Snapshot::new();
            if let Some(n) = bgs.to_node() {
                row_snap.append_node(&n);
            }
            if let Some(n) = rs.to_node() {
                row_snap.append_node(&n);
            }
            cache[y] = row_snap.to_node();
            *rebuilt += 1;
            let _ = row.set_dirty(false);
        }
        if let Some(node) = &cache[y] {
            s.save();
            s.translate(&graphene::Point::new(0.0, PAD + y as f32 * ch));
            s.append_node(node);
            s.restore();
        }
        y += 1;
    }

    if snap.cursor_visible().unwrap_or(false)
        && let Ok(Some(c)) = snap.cursor_viewport()
    {
        let (cx, cy) = (PAD + c.x as f32 * cw, PAD + c.y as f32 * ch);
        let color = rgba(colors.cursor.unwrap_or(colors.foreground));
        if !preedit.is_empty() {
            let layout = w.create_pango_layout(Some(preedit));
            layout.set_font_description(Some(font));
            let (_, logical) = layout.pixel_extents();
            s.append_color(&rgba(colors.background), &graphene::Rect::new(cx, cy, logical.width() as f32, ch));
            s.save();
            s.translate(&graphene::Point::new(cx, cy));
            s.append_layout(&layout, &color);
            s.restore();
            s.append_color(&color, &graphene::Rect::new(cx, cy + ch - 2.0, logical.width() as f32, 1.0));
        } else {
            match snap.cursor_visual_style().unwrap_or(CursorVisualStyle::Block) {
                CursorVisualStyle::Bar => s.append_color(&color, &graphene::Rect::new(cx, cy, 2.0, ch)),
                CursorVisualStyle::Underline => s.append_color(&color, &graphene::Rect::new(cx, cy + ch - 2.0, cw, 2.0)),
                _ => {
                    let mut c2 = color;
                    c2.set_alpha(0.6);
                    s.append_color(&c2, &graphene::Rect::new(cx, cy, cw, ch));
                }
            }
        }
    }
    let _ = snap.set_dirty(Dirty::Clean);
}

impl TermView {
    pub fn new(cols: u16, rows: u16) -> TermView {
        let w: TermView = glib::Object::new();
        let font = pango::FontDescription::from_string(FONT);
        let mut bold = font.clone();
        bold.set_weight(pango::Weight::Bold);
        let probe = w.create_pango_layout(Some("M"));
        probe.set_font_description(Some(&font));
        let (_, logical) = probe.extents();
        let cell_w = logical.width() as f32 / pango::SCALE as f32;
        let cell_h = (logical.height() as f32 / pango::SCALE as f32).ceil();
        *w.imp().st.borrow_mut() = Some(State {
            pane: Pane::new(cols, rows).unwrap(),
            render: RenderState::new().unwrap(),
            rows_it: RowIterator::new().unwrap(),
            cells_it: CellIterator::new().unwrap(),
            cache: Vec::new(),
            font,
            bold,
            cell_w,
            cell_h,
            grid: (cols, rows),
            sender: None,
            preedit: String::new(),
            rebuilt: 0,
        });
        w.set_focusable(true);
        w.set_hexpand(true);
        w.set_vexpand(true);
        w
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(self.imp().st.borrow_mut().as_mut().unwrap())
    }

    pub fn feed(&self, e: &Event) {
        self.with(|st| st.pane.feed(e));
        self.queue_draw();
    }

    /// Keys through libghostty's encoder; text through the input method
    /// (which also covers IME composition).
    pub fn wire_input(&self) {
        let im = gtk::IMMulticontext::new();
        im.set_client_widget(Some(self));
        let keys = gtk::EventControllerKey::new();
        keys.set_im_context(Some(&im));
        let me = self.clone();
        keys.connect_key_pressed(move |_, keyval, keycode, state| {
            let mut mods = Mods::empty();
            if state.contains(gdk::ModifierType::SHIFT_MASK) {
                mods |= Mods::SHIFT;
            }
            if state.contains(gdk::ModifierType::CONTROL_MASK) {
                mods |= Mods::CTRL;
            }
            if state.contains(gdk::ModifierType::ALT_MASK) {
                mods |= Mods::ALT;
            }
            if state.contains(gdk::ModifierType::SUPER_MASK) {
                mods |= Mods::SUPER;
            }
            let text = keyval.to_unicode().map(|c| c.to_string());
            me.with(|st| {
                let bytes = st.pane.key(keycode.saturating_sub(8), true, mods, text.as_deref()).to_vec();
                if let Some(tx) = &st.sender {
                    tx.input(&bytes);
                }
            });
            glib::Propagation::Stop
        });
        let me = self.clone();
        im.connect_commit(move |_, text| {
            me.with(|st| {
                st.preedit.clear();
                if let Some(tx) = &st.sender {
                    tx.input(text.as_bytes());
                }
            });
            me.queue_draw();
        });
        let me = self.clone();
        im.connect_preedit_changed(move |im| {
            let (text, _, _) = im.preedit_string();
            me.with(|st| st.preedit = text.to_string());
            me.queue_draw();
        });
        self.add_controller(keys);
        let focus = gtk::EventControllerFocus::new();
        let (a, b) = (im.clone(), im.clone());
        focus.connect_enter(move |_| a.focus_in());
        focus.connect_leave(move |_| b.focus_out());
        self.add_controller(focus);
    }
}

/// Renders the view the way GTK would and saves it as a PNG.
pub fn shot(v: &TermView, path: &str) {
    let paintable = gtk::WidgetPaintable::new(Some(v));
    let snap = gtk::Snapshot::new();
    paintable.snapshot(&snap, v.width() as f64, v.height() as f64);
    let Some(node) = snap.to_node() else { return };
    let Some(renderer) = v.native().and_then(|n| n.renderer()) else { return };
    let tex = renderer.render_texture(&node, None);
    match tex.save_to_png(path) {
        Ok(()) => println!("s26: saved {path}"),
        Err(e) => println!("s26: screenshot failed: {e}"),
    }
}

// ---- the bench: S25's scenarios, fed straight into the terminal

/// Paint times from the frame clock, and futures waiting for the next one.
/// (Polling with a short timer instead starves GTK's redraw: timers run at
/// default priority, above GDK_PRIORITY_REDRAW.)
#[derive(Default)]
pub struct Paints {
    pub times: Vec<f64>,
    /// The frame counter of each paint, for its presentation time.
    pub counters: Vec<i64>,
    pub waiters: Vec<async_channel::Sender<()>>,
}

impl Paints {
    fn len(&self) -> usize {
        self.times.len()
    }
}

async fn next_paint(p: &Rc<RefCell<Paints>>) {
    let (tx, rx) = async_channel::bounded(1);
    p.borrow_mut().waiters.push(tx);
    let _ = rx.recv().await;
}

/// Yield to the main loop below redraw priority, so a frame can happen.
async fn yield_low() {
    glib::timeout_future_with_priority(glib::Priority::LOW, std::time::Duration::ZERO).await;
}

struct Bench {
    view: TermView,
    paints: Rc<RefCell<Paints>>,
    t0: Instant,
}

pub fn now(t0: Instant) -> f64 {
    t0.elapsed().as_secs_f64() * 1000.0
}

fn pct(xs: &[f64], p: f64) -> f64 {
    let mut s = xs.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if s.is_empty() { 0.0 } else { s[((p / 100.0) * s.len() as f64).floor().min(s.len() as f64 - 1.0) as usize] }
}

fn r1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

impl Bench {
    /// Write a marker and wait for the paint with it, then the next frame.
    async fn probe(&self) -> f64 {
        let start = now(self.t0);
        let n = self.paints.borrow().len();
        self.view.feed(&Event::Output(b"\x1b7\x1b[1;150H#\x1b8".to_vec()));
        if std::env::var_os("S26_DEBUG").is_some() {
            eprintln!("s26: probe at {start:.1}, {n} paints, mapped {} visible {} size {}x{}", self.view.is_mapped(), self.view.is_visible(), self.view.width(), self.view.height());
        }
        while self.paints.borrow().len() <= n {
            next_paint(&self.paints).await;
        }
        // As S25 measured the browsers: until the frame after the paint
        // (rAF there, a tick callback here, both aligned to vsync).
        let (tx, rx) = async_channel::bounded(1);
        self.view.add_tick_callback(move |_, _| {
            let _ = tx.try_send(());
            glib::ControlFlow::Break
        });
        let _ = rx.recv().await;
        now(self.t0) - start
    }

    fn frames(&self, a: f64, b: f64) -> Vec<f64> {
        let p = self.paints.borrow();
        let inside: Vec<f64> = p.times.iter().copied().filter(|t| *t >= a && *t <= b).collect();
        inside.windows(2).map(|w| w[1] - w[0]).collect()
    }

    fn summary(&self, name: &str, a: f64, b: f64, lat: &[f64], extra: serde_like::Obj) -> String {
        let f = self.frames(a, b);
        let secs = (b - a) / 1000.0;
        let mut o = serde_like::Obj::new();
        o.s("name", name);
        o.n("seconds", r1(secs));
        o.n("fps", r1(f.len() as f64 / secs));
        o.n("frame_p95", r1(pct(&f, 95.0)));
        o.n("frame_max", r1(f.iter().copied().fold(0.0, f64::max)));
        o.n("frames_over_50ms", f.iter().filter(|d| **d > 50.0).count() as f64);
        o.n("latency_p50", r1(pct(lat, 50.0)));
        o.n("latency_p95", r1(pct(lat, 95.0)));
        o.n("latency_max", r1(lat.iter().copied().fold(0.0, f64::max)));
        o.n("latency_n", lat.len() as f64);
        o.extend(extra);
        o.done()
    }

    async fn flood(&self, name: &str, mut chunk: impl FnMut() -> Vec<u8>) -> String {
        self.view.feed(&Event::Snapshot(Vec::new()));
        glib::timeout_future(std::time::Duration::from_millis(200)).await;
        let a = now(self.t0);
        let mut bytes = 0usize;
        let mut lat = Vec::new();
        let mut next_probe = a;
        while now(self.t0) - a < 5000.0 {
            let c = chunk();
            bytes += c.len();
            self.view.feed(&Event::Output(c));
            if now(self.t0) >= next_probe {
                lat.push(self.probe().await);
                next_probe = now(self.t0) + 100.0;
            } else {
                // Yield so GTK can paint, as a socket read would.
                yield_low().await;
            }
        }
        let b = now(self.t0);
        eprintln!("s26: {name} done");
        let mut x = serde_like::Obj::new();
        x.n("mbps", r1(bytes as f64 / 1e6 / ((b - a) / 1000.0)));
        self.summary(name, a, b, &lat, x)
    }

    async fn redraw(&self, name: &str, screen: impl Fn(usize) -> Vec<u8>) -> String {
        self.view.feed(&Event::Snapshot(Vec::new()));
        glib::timeout_future(std::time::Duration::from_millis(200)).await;
        let a = now(self.t0);
        let mut lat = Vec::new();
        let mut i = 0;
        while now(self.t0) - a < 5000.0 {
            self.view.feed(&Event::Output(screen(i)));
            i += 1;
            if i % 6 == 0 {
                lat.push(self.probe().await);
            } else {
                next_paint(&self.paints).await;
            }
        }
        let b = now(self.t0);
        let mut x = serde_like::Obj::new();
        x.n("screens_per_s", r1(i as f64 / ((b - a) / 1000.0)));
        self.summary(name, a, b, &lat, x)
    }
}

/// Enough JSON for the bench output, without serde in this crate.
mod serde_like {
    pub struct Obj(Vec<String>);
    impl Obj {
        pub fn new() -> Obj {
            Obj(Vec::new())
        }
        pub fn s(&mut self, k: &str, v: &str) {
            self.0.push(format!("\"{k}\":\"{v}\""));
        }
        pub fn n(&mut self, k: &str, v: f64) {
            self.0.push(format!("\"{k}\":{v}"));
        }
        pub fn raw(&mut self, k: &str, v: &str) {
            self.0.push(format!("\"{k}\":{v}"));
        }
        pub fn extend(&mut self, o: Obj) {
            self.0.extend(o.0);
        }
        pub fn done(self) -> String {
            format!("{{{}}}", self.0.join(","))
        }
    }
}

const WORDS: [&str; 15] =
    ["the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "request", "handled", "cache", "miss", "retry", "ok", "warn"];

fn log_chunk(seq: &mut usize) -> Vec<u8> {
    let mut s = String::new();
    for _ in 0..400 {
        let lvl = ["\x1b[32mINFO\x1b[0m", "\x1b[33mWARN\x1b[0m", "\x1b[31mERROR\x1b[0m", "\x1b[2mDEBUG\x1b[0m"][*seq % 4];
        s += &format!("\x1b[90m2026-10-04T06:{:02}:00.{}Z\x1b[0m {lvl} \x1b[36msvc::worker{}\x1b[0m ", *seq % 60, *seq % 1000, *seq % 9);
        for w in 0..12 {
            s += WORDS[(*seq * 7 + w) % WORDS.len()];
            s.push(' ');
        }
        s += &format!("id={}\r\n", *seq);
        *seq += 1;
    }
    s.into_bytes()
}

fn htop(n: usize) -> Vec<u8> {
    let mut s = String::from("\x1b[H");
    for row in 0..48 {
        let pct = (row * 13 + n * 7) % 100;
        let bar = format!("{:<50}", "|".repeat(pct / 2));
        s += &format!(
            "\x1b[{};1H\x1b[1;37m{:>3}\x1b[0m [\x1b[32m{}\x1b[31m{}\x1b[0m] \x1b[{}m {:>3}% {} jake  20 0 {:<60}\x1b[0m",
            row + 1,
            row,
            &bar[..30],
            &bar[30..],
            if row % 2 == 1 { 44 } else { 40 },
            pct,
            1000 + ((n + row) * 37) % 9000,
            WORDS[(n + row) % WORDS.len()]
        );
    }
    s.into_bytes()
}

fn vim_scroll(n: usize) -> Vec<u8> {
    format!(
        "\x1b[1;47r\x1b[47;1H\n\x1b[47;1H\x1b[33m{n:>5}\x1b[0m fn handler_{n}(req: Request) -> Result<Response> {{ let id = req.id(); {} }}\x1b[K\x1b[r\x1b[48;1H\x1b[7m src/main.rs  [+]  {n},1  {}% \x1b[K\x1b[0m",
        WORDS[n % WORDS.len()],
        n % 100
    )
    .into_bytes()
}

pub async fn run_bench(view: TermView, paints: Rc<RefCell<Paints>>, t0: Instant, out: Option<PathBuf>, app: gtk::Application) {
    let b = Bench { view, paints, t0 };
    glib::timeout_future(std::time::Duration::from_millis(800)).await;
    let base = b.frames(now(t0) - 500.0, now(t0));
    eprintln!("s26: bench start, {} paints so far", b.paints.borrow().len());
    let mut scenarios = Vec::new();
    // Idle: 100 probes.
    b.view.feed(&Event::Snapshot(Vec::new()));
    glib::timeout_future(std::time::Duration::from_millis(300)).await;
    let mut lat = Vec::new();
    for _ in 0..100 {
        lat.push(b.probe().await);
        glib::timeout_future(std::time::Duration::from_millis(30)).await;
    }
    eprintln!("s26: idle done");
    scenarios.push(format!(
        "{{\"name\":\"idle\",\"latency_p50\":{},\"latency_p95\":{},\"latency_max\":{},\"latency_n\":100}}",
        r1(pct(&lat, 50.0)),
        r1(pct(&lat, 95.0)),
        r1(lat.iter().copied().fold(0.0, f64::max))
    ));
    let yes = "y\r\n".repeat(16384).into_bytes();
    scenarios.push(b.flood("yes", || yes.clone()).await);
    let mut seq = 0;
    scenarios.push(b.flood("log", || log_chunk(&mut seq)).await);
    scenarios.push(b.redraw("htop", htop).await);
    scenarios.push(b.redraw("vim-scroll", vim_scroll).await);
    let rebuilt = b.view.with(|st| st.rebuilt);
    let mut o = serde_like::Obj::new();
    o.s("engine", &format!("s26-gtk (GTK {}.{}.{}, GSK)", gtk::major_version(), gtk::minor_version(), gtk::micro_version()));
    o.s("renderer", &std::env::var("GSK_RENDERER").unwrap_or_else(|_| "default".into()));
    o.n("refresh_hz", r1(1000.0 / pct(&base, 50.0).max(0.1)));
    o.n("rows_rebuilt", rebuilt as f64);
    o.raw("scenarios", &format!("[{}]", scenarios.join(",")));
    let json = o.done();
    println!("S26RESULT {json}");
    if let Some(p) = out {
        let _ = std::fs::write(p, &json);
    }
    app.quit();
}

