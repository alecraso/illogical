//! S26's core as a C library, for the Swift front end (`mac/s26.h`).
//!
//! The render call does the cell walking on the Rust side, the same way the
//! GTK view does: for each dirty row it hands back runs (cells with the same
//! colors and flags; one run per cell outside ASCII, so wide characters
//! stay on the grid), and Swift only draws them.
#![allow(clippy::missing_safety_doc)]

use std::{
    ffi::{CStr, c_char, c_void},
    path::Path,
};

use s26_core::{
    Conn, Event, Pane,
    libghostty_vt::{
        key::Mods,
        render::{CellIterator, Dirty, RenderState, RowIterator},
        style::RgbColor,
    },
    mac_key,
};

pub struct S26Term {
    pane: Pane,
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    key_buf: Vec<u8>,
}

#[repr(C)]
pub struct S26Run {
    pub col: u16,
    pub cells: u16,
    pub fg: u32,
    /// 0xRRGGBB, or u32::MAX for the default background.
    pub bg: u32,
    /// 1 bold, 2 italic, 4 underline.
    pub flags: u8,
    pub text: *const u8,
    pub text_len: usize,
}

#[repr(C)]
pub struct S26Frame {
    pub dirty: u32,
    pub cols: u16,
    pub rows: u16,
    pub fg: u32,
    pub bg: u32,
    pub cursor_visible: bool,
    pub cursor_x: u16,
    pub cursor_y: u16,
    pub cursor_color: u32,
}

fn rgb(c: RgbColor) -> u32 {
    (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn s26_term_new(cols: u16, rows: u16) -> *mut S26Term {
    let t = S26Term {
        pane: Pane::new(cols, rows).expect("terminal"),
        render: RenderState::new().expect("render state"),
        rows: RowIterator::new().expect("rows"),
        cells: CellIterator::new().expect("cells"),
        key_buf: Vec::new(),
    };
    Box::into_raw(Box::new(t))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn s26_term_free(t: *mut S26Term) {
    if !t.is_null() {
        drop(unsafe { Box::from_raw(t) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn s26_term_write(t: *mut S26Term, data: *const u8, len: usize) {
    let t = unsafe { &mut *t };
    t.pane.term.vt_write(unsafe { std::slice::from_raw_parts(data, len) });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn s26_term_reset(t: *mut S26Term) {
    unsafe { &mut *t }.pane.term.reset();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn s26_term_resize(t: *mut S26Term, cols: u16, rows: u16, cell_w: u32, cell_h: u32) {
    unsafe { &mut *t }.pane.resize(cols, rows, cell_w, cell_h);
}

/// Encodes a key press. `mods`: 1 shift, 2 ctrl, 4 alt (Option), 8 super
/// (Command). Returns the bytes' length, written to `out` (up to `cap`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn s26_term_key(
    t: *mut S26Term,
    mac_code: u16,
    mods: u32,
    text: *const c_char,
    out: *mut u8,
    cap: usize,
) -> usize {
    let t = unsafe { &mut *t };
    let mut m = Mods::empty();
    for (bit, flag) in [(1, Mods::SHIFT), (2, Mods::CTRL), (4, Mods::ALT), (8, Mods::SUPER)] {
        if mods & bit != 0 {
            m |= flag;
        }
    }
    let text = (!text.is_null()).then(|| unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned());
    let bytes = t.pane.key_as(mac_key(mac_code), true, m, text.as_deref());
    t.key_buf.clear();
    t.key_buf.extend_from_slice(bytes);
    let n = t.key_buf.len().min(cap);
    unsafe { std::ptr::copy_nonoverlapping(t.key_buf.as_ptr(), out, n) };
    n
}

pub type RowFn = extern "C" fn(ctx: *mut c_void, row: u16, runs: *const S26Run, n: usize);

/// Walks the render state: `row_fn` for every row that changed since the
/// last call (all of them when `dirty` comes back 2), then fills `frame`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn s26_term_render(t: *mut S26Term, ctx: *mut c_void, row_fn: RowFn, frame: *mut S26Frame) {
    let t = unsafe { &mut *t };
    let S26Term { pane, render, rows, cells, .. } = t;
    let Ok(snap) = render.update(&pane.term) else { return };
    let Ok(colors) = snap.colors() else { return };
    let dirty = snap.dirty().unwrap_or(Dirty::Full);
    let full = dirty == Dirty::Full;
    let mut runs: Vec<S26Run> = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    let mut text = String::new();
    if let Ok(mut it) = rows.update(&snap) {
        let mut y = 0u16;
        while let Some(row) = it.next() {
            if full || row.dirty().unwrap_or(true) {
                runs.clear();
                texts.clear();
                if let Ok(mut cit) = cells.update(row) {
                    let mut x = 0u16;
                    // (fg, bg, flags) of the open run, and where it began.
                    let mut open: Option<(u32, u32, u8, u16)> = None;
                    let mut buf = String::new();
                    let close = |open: &mut Option<(u32, u32, u8, u16)>, buf: &mut String, end: u16, texts: &mut Vec<String>, runs: &mut Vec<S26Run>| {
                        if let Some((fg, bg, flags, start)) = open.take() {
                            texts.push(std::mem::take(buf));
                            runs.push(S26Run { col: start, cells: end - start, fg, bg, flags, text: std::ptr::null(), text_len: 0 });
                        }
                    };
                    while let Some(cell) = cit.next() {
                        let n = cell.graphemes_len().unwrap_or(0);
                        let bg = cell.bg_color().ok().flatten();
                        let mut fg = cell.fg_color().ok().flatten().unwrap_or(colors.foreground);
                        let mut flags = 0u8;
                        let mut inv = false;
                        if cell.has_styling().unwrap_or(false)
                            && let Ok(st) = cell.style()
                        {
                            flags |= st.bold as u8 | (st.italic as u8) << 1;
                            inv = st.inverse;
                        }
                        let mut bgc = bg.map(rgb).unwrap_or(u32::MAX);
                        if inv || cell.is_selected().unwrap_or(false) {
                            bgc = rgb(fg);
                            fg = bg.unwrap_or(colors.background);
                        }
                        let fgc = rgb(fg);
                        if n == 0 {
                            text.clear();
                        } else {
                            let _ = cell.graphemes_utf8(&mut text);
                        }
                        let ascii = text.is_ascii();
                        let same = matches!(open, Some((f, b, fl, _)) if f == fgc && b == bgc && fl == flags);
                        if !(same && ascii) {
                            close(&mut open, &mut buf, x, &mut texts, &mut runs);
                        }
                        // The tail of a wide character belongs to the run before.
                        if n == 0 && !same && runs.last().is_some_and(|r| r.col + r.cells == x && !texts.last().unwrap().is_ascii()) {
                            runs.last_mut().unwrap().cells += 1;
                            x += 1;
                            continue;
                        }
                        if open.is_none() {
                            open = Some((fgc, bgc, flags, x));
                        }
                        buf.push_str(if n == 0 { " " } else { &text });
                        if !ascii {
                            close(&mut open, &mut buf, x + 1, &mut texts, &mut runs);
                        }
                        x += 1;
                    }
                    close(&mut open, &mut buf, x, &mut texts, &mut runs);
                }
                for (r, s) in runs.iter_mut().zip(&texts) {
                    r.text = s.as_ptr();
                    r.text_len = s.len();
                }
                row_fn(ctx, y, runs.as_ptr(), runs.len());
                let _ = row.set_dirty(false);
            }
            y += 1;
        }
    }
    let f = unsafe { &mut *frame };
    f.dirty = match dirty {
        Dirty::Clean => 0,
        Dirty::Partial => 1,
        Dirty::Full => 2,
    };
    f.cols = snap.cols().unwrap_or(0);
    f.rows = snap.rows().unwrap_or(0);
    f.fg = rgb(colors.foreground);
    f.bg = rgb(colors.background);
    f.cursor_color = rgb(colors.cursor.unwrap_or(colors.foreground));
    f.cursor_visible = false;
    if snap.cursor_visible().unwrap_or(false)
        && let Ok(Some(c)) = snap.cursor_viewport()
    {
        f.cursor_visible = true;
        f.cursor_x = c.x;
        f.cursor_y = c.y;
    }
    let _ = snap.set_dirty(Dirty::Clean);
}

/// The daemon's tabs as JSON: `[{"name", "panes": [N…], "kind"}]` (kind
/// of the first pane). Free it with `s26_string_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn s26_tabs_json(sock: *const c_char) -> *mut c_char {
    let sock = unsafe { CStr::from_ptr(sock) }.to_string_lossy().into_owned();
    let Ok(state) = s26_core::hello(Path::new(&sock)) else { return std::ptr::null_mut() };
    let tabs: Vec<_> = state
        .tabs
        .iter()
        .map(|t| {
            let panes: Vec<u32> = t.layout.panes.iter().map(|(p, _)| u32::from(*p)).collect();
            let kind = panes
                .first()
                .and_then(|f| state.panes.iter().find(|p| u32::from(p.id) == *f))
                .map(|p| format!("{:?}", p.kind))
                .unwrap_or_default();
            serde_json::json!({ "name": t.name.clone().unwrap_or_else(|| format!("tab {}", u32::from(t.id))), "panes": panes, "kind": kind })
        })
        .collect();
    std::ffi::CString::new(serde_json::Value::from(tabs).to_string()).unwrap().into_raw()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn s26_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { std::ffi::CString::from_raw(s) });
    }
}

// ---- the connection

pub struct S26Conn(Conn);

pub type NotifyFn = extern "C" fn(ctx: *mut c_void);

struct Ctx(*mut c_void);
unsafe impl Send for Ctx {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn s26_conn_attach(
    sock: *const c_char,
    pane: u32,
    cols: u16,
    rows: u16,
    notify: NotifyFn,
    ctx: *mut c_void,
) -> *mut S26Conn {
    let sock = unsafe { CStr::from_ptr(sock) }.to_string_lossy().into_owned();
    let ctx = Ctx(ctx);
    match Conn::attach(Path::new(&sock), pane, cols, rows, move || {
        let c = &ctx;
        notify(c.0)
    }) {
        Ok(c) => Box::into_raw(Box::new(S26Conn(c))),
        Err(e) => {
            eprintln!("s26: {e:#}");
            std::ptr::null_mut()
        }
    }
}

/// Feeds every queued event into `term`. Returns 1 if anything was drawn
/// into it, 0 if nothing, -1 once the connection is closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn s26_conn_drain(c: *mut S26Conn, term: *mut S26Term) -> i32 {
    let (c, t) = unsafe { (&*c, &mut *term) };
    let mut any = 0;
    while let Ok(e) = c.0.events.try_recv() {
        match e {
            Event::Closed(_) => return -1,
            Event::Snapshot(_) | Event::Output(_) => {
                t.pane.feed(&e);
                any = 1;
            }
            Event::Attached { .. } => {}
        }
    }
    any
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn s26_conn_input(c: *mut S26Conn, data: *const u8, len: usize) {
    unsafe { &*c }.0.sender.input(unsafe { std::slice::from_raw_parts(data, len) });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn s26_conn_view(c: *mut S26Conn, cols: u16, rows: u16) {
    unsafe { &*c }.0.sender.view(cols, rows);
}
