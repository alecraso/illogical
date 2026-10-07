//! Drawing a frame: the sidebar (sessions, tabs, what needs you), the shown
//! tab's panes and dividers, the status line, and menus and prompts on top.

use std::time::Duration;

use arugula_proto::{Attention, BlockType, Dir, Edge, PaneInfo};
use arugula_vt::{Cursor, VtEngine};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

use super::{
    agent,
    app::{Act, App, Hit, Menu, Mode},
};

const TOAST: Duration = Duration::from_secs(5);

/// How wide the sidebar is for a screen this wide (0: hidden).
pub fn sidebar_width(cols: u16, shown: bool) -> u16 {
    match cols {
        _ if !shown => 0,
        0..60 => 0,
        60..100 => 20,
        _ => 26,
    }
}

/// Draw everything; the focused pane's cursor, if it shows and nothing
/// covers it.
pub fn draw(app: &mut App, f: &mut Frame) -> Option<Cursor> {
    let full = f.area();
    let sw = sidebar_width(full.width, app.sidebar);
    let area = Rect {
        x: if sw > 0 { sw + 1 } else { 0 },
        y: 0,
        width: full.width.saturating_sub(if sw > 0 { sw + 1 } else { 0 }),
        height: full.height.saturating_sub(1),
    };
    app.full = full;
    if area != app.area {
        app.area = area;
        app.view(true);
    }
    let buf = f.buffer_mut();
    if sw > 0 {
        sidebar(app, buf, sw, area.height);
        for y in 0..area.height {
            if let Some(c) = buf.cell_mut((sw, y)) {
                c.set_symbol("│").set_fg(Color::DarkGray);
            }
        }
    } else {
        app.hits.clear();
    }

    let mut cursor = None;
    let in_copy = app.in_copy();
    for (pid, r) in app.rects() {
        let focused = app.focus == Some(pid);
        if let Some(t) = app.panes.get_mut(&pid) {
            let mut c = t.draw(buf, r);
            let back = t.view().scrolled_back();
            if back > 0 {
                // How far up, dimly, out of the way of the text.
                let tag = format!(" ↑{back} ");
                let x = r.right().saturating_sub(tag.chars().count() as u16);
                buf.set_string(x, r.y, tag, Style::default().fg(Color::Gray).bg(Color::DarkGray));
            }
            if in_copy && let Some(cp) = app.copy.as_ref().filter(|cp| cp.pane == pid) {
                // Copy mode's cursor, where it is in the history.
                let top = t.view().top_row();
                c = None;
                if cp.row >= top && cp.row - top < r.height as u32 && cp.x < r.width {
                    let (x, y) = (r.x + cp.x, r.y + (cp.row - top) as u16);
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(Style::default().fg(Color::Black).bg(Color::Yellow));
                    }
                    c = Some(Cursor { x, y, shape: arugula_vt::CursorShape::Block, blink: false, color: None });
                }
            }
            if focused {
                cursor = c;
            }
            continue;
        }
        match app.info(pid).map(|i| i.kind) {
            Some(BlockType::Agent) => match app.blocks.get(&pid) {
                Some(state) => agent::draw(buf, r, state, app.scroll.get(&pid).copied().unwrap_or(0), focused),
                None => note(buf, r, &format!("%{pid} agent: starting")),
            },
            Some(BlockType::Browser) => {
                let url = app.blocks.get(&pid).and_then(|s| s["url"].as_str().map(str::to_owned)).unwrap_or_default();
                note(buf, r, &format!("%{pid} web page {url}  (open it in the web client)"));
            }
            Some(BlockType::Editor) => {
                let s = app.blocks.get(&pid);
                let at = |k: &str| s.and_then(|s| s[k].as_str().map(str::to_owned));
                let what = match (at("file"), s.and_then(|s| s["line"].as_u64())) {
                    (Some(f), Some(l)) => format!("{f}:{l}"),
                    (Some(f), None) => f,
                    _ => at("folder").unwrap_or_default(),
                };
                note(buf, r, &format!("%{pid} VS Code {what}  (open it in the web client)"));
            }
            Some(BlockType::Remote) => {
                let s = app.blocks.get(&pid);
                let host = s.and_then(|s| s["host"].as_str().map(str::to_owned)).unwrap_or_default();
                let pane = s.and_then(|s| s["pane"].as_u64()).map(|p| format!(" %{p}")).unwrap_or_default();
                note(buf, r, &format!("%{pid} on {host}{pane}  (open it in the web client, or `--host {host} tui`)"));
            }
            Some(BlockType::Diff) => {
                let s = app.blocks.get(&pid);
                let n = s.and_then(|s| s["files"].as_array().map(Vec::len)).unwrap_or(0);
                let name = s.and_then(|s| s["name"].as_str().map(str::to_owned)).unwrap_or_default();
                note(buf, r, &format!("%{pid} changes in {name}: {n} files  (`arugula capture %{pid}`)"));
            }
            Some(BlockType::File) => {
                let s = app.blocks.get(&pid);
                let path = s.and_then(|s| s["path"].as_str().map(str::to_owned)).unwrap_or_default();
                note(buf, r, &format!("%{pid} file {path}  (`arugula capture %{pid}`)"));
            }
            Some(BlockType::Workspace) => {
                // M34: what it is on one line, then each gate waiting and
                // how to approve it.
                let s = app.blocks.get(&pid);
                let at = |k: &str| s.and_then(|s| s[k].as_str().map(str::to_owned));
                let len = |k: &str| s.and_then(|s| s[k].as_array().map(Vec::len)).unwrap_or(0);
                let name = at("name").unwrap_or_else(|| "workspace".into());
                let line = match at("error") {
                    Some(e) => format!("%{pid} chant workspace {}: {e}", at("root").unwrap_or_default()),
                    None => format!(
                        "%{pid} chant workspace {name}: {} members, {} gates waiting  (`arugula capture %{pid}`)",
                        len("members"),
                        len("gates")
                    ),
                };
                note(buf, r, &line);
                let gates = s.and_then(|s| s["gates"].as_array()).into_iter().flatten();
                for (i, g) in gates.enumerate().take(r.height.saturating_sub(1) as usize) {
                    let g = |k: &str| g[k].as_str().unwrap_or("").to_owned();
                    let row = Rect { y: r.y + 1 + i as u16, height: 1, ..r };
                    let text = format!(
                        "  {}: {} waits at gate {}  (`arugula call %{pid} approve '{{\"key\":\"{}/{}/{}\"}}'`)",
                        g("member"),
                        g("op"),
                        g("gate"),
                        g("member"),
                        g("op"),
                        g("gate")
                    );
                    buf.set_stringn(
                        row.x + 1,
                        row.y,
                        text,
                        row.width.saturating_sub(1) as usize,
                        Style::default().fg(Color::Yellow),
                    );
                }
            }
            Some(BlockType::Fountain) => {
                // M43: the catalog on one line: how many, of how many, and
                // the filter.
                let s = app.blocks.get(&pid);
                let at = |k: &str| s.and_then(|s| s[k].as_str().map(str::to_owned));
                // M45b: the runner view, on one line: this host's runner,
                // its sandboxes, and what wants you.
                if let Some(rv) = s.filter(|s| s["view"] == "runner").map(|s| &s["runner"]) {
                    let this = &rv["this"];
                    let line = match (at("error"), rv.is_object(), this.is_object()) {
                        (Some(e), _, _) => format!("%{pid} Fountain runner: {e}"),
                        (None, false, _) => format!("%{pid} Fountain runner: reading…"),
                        (None, true, false) => match rv["unit"]["name"].as_str() {
                            Some(n) => format!("%{pid} Fountain runner {n}: not on Fountain"),
                            None => format!("%{pid} Fountain runners: this host runs none"),
                        },
                        (None, true, true) => format!(
                            "%{pid} Fountain runner {}: {} · {} sandboxes{}  (`arugula capture %{pid}`)",
                            this["name"].as_str().unwrap_or("?"),
                            if this["online"] == true { "online" } else { "offline" },
                            rv["sandboxes"].as_array().map_or(0, Vec::len),
                            rv["attention"].as_str().map(|a| format!(" · {a}")).unwrap_or_default()
                        ),
                    };
                    note(buf, r, &line);
                } else {
                    let shown = s.and_then(|s| s["agents"].as_array().map(Vec::len)).unwrap_or(0);
                    let total = s.and_then(|s| s["total"].as_u64()).unwrap_or(0);
                    let line = match (at("error"), s.is_some_and(|s| s["loading"] == true)) {
                        (Some(e), _) => format!("%{pid} Fountain agents: {e}"),
                        (None, true) => format!("%{pid} Fountain agents: reading…"),
                        (None, false) if shown as u64 == total => {
                            format!("%{pid} Fountain agents: {total}  (`arugula capture %{pid}`)")
                        }
                        (None, false) => {
                            format!("%{pid} Fountain agents: {shown} of {total}  (`arugula capture %{pid}`)")
                        }
                    };
                    note(buf, r, &line);
                }
            }
            Some(BlockType::Forge) => {
                // M36: the PR on one line, then what waits on you, each a
                // line (drafts first: an agent's write waits to be sent).
                let s = app.blocks.get(&pid);
                // M37: an issue's line is the same, without checks.
                let issue = s.is_some_and(|s| s["kind"] == "issue");
                let pr = s.map(|s| if issue { &s["issue"]["item"] } else { &s["pr"]["item"] });
                let at = |v: Option<&serde_json::Value>, k: &str| v.and_then(|v| v[k].as_str().map(str::to_owned));
                let repo = at(s, "repo").unwrap_or_default();
                let n = s.and_then(|s| s["number"].as_u64()).unwrap_or(0);
                let checks = match s.and_then(|s| s["pr"]["rollup"].as_str()) {
                    _ if issue => {
                        let a = s.and_then(|s| s["link"]["block"].as_u64());
                        a.map(|a| format!("agent %{a}")).unwrap_or_else(|| "issue".into())
                    }
                    Some(r) => format!("checks {r}"),
                    None => "checks: none".into(),
                };
                let line = match (at(s, "error"), pr.filter(|p| p.is_object())) {
                    (Some(e), _) => format!("%{pid} {repo}#{n}: {e}"),
                    (None, Some(p)) => format!(
                        "%{pid} {repo}#{n} {} [{}] {checks}  (`arugula capture %{pid}`)",
                        at(Some(p), "title").unwrap_or_default(),
                        at(Some(p), "state").unwrap_or_default()
                    ),
                    (None, None) => format!("%{pid} {repo}#{n}: reading…"),
                };
                note(buf, r, &line);
                let drafts =
                    s.and_then(|s| s["drafts"].as_array()).into_iter().flatten().filter(|d| d["status"] == "waiting");
                let mut rows: Vec<String> = drafts
                    .map(|d| {
                        format!(
                            "  draft {} by {}: answer it in the web client or `arugula attention`",
                            d["method"].as_str().unwrap_or("?"),
                            d["by"].as_str().unwrap_or("?")
                        )
                    })
                    .collect();
                rows.extend(
                    s.and_then(|s| s["wants"].as_array()).into_iter().flatten().map(|w| {
                        format!("  {}: {}", w["kind"].as_str().unwrap_or(""), w["why"].as_str().unwrap_or(""))
                    }),
                );
                for (i, text) in rows.into_iter().enumerate().take(r.height.saturating_sub(1) as usize) {
                    let row = Rect { y: r.y + 1 + i as u16, height: 1, ..r };
                    buf.set_stringn(
                        row.x + 1,
                        row.y,
                        text,
                        row.width.saturating_sub(1) as usize,
                        Style::default().fg(Color::Yellow),
                    );
                }
            }
            _ => note(buf, r, &format!("%{pid}")),
        }
    }
    dividers(app, buf);
    if let Some(mv) = &app.moving
        && let Some((target, edge)) = mv.target
        && let Some((_, r)) = app.rects().into_iter().find(|(p, _)| *p == target)
    {
        landing(buf, r, edge);
    }
    status(app, buf, full);
    match &app.mode {
        Mode::Menu(m) => {
            menu(buf, full, m);
            cursor = None;
        }
        Mode::Prompt(p) => {
            cursor = prompt(buf, full, &p.title, &p.text);
        }
        _ => {}
    }
    cursor
}

fn note(buf: &mut Buffer, r: Rect, text: &str) {
    buf.set_stringn(r.x + 1, r.y, text, r.width.saturating_sub(1) as usize, Style::default().fg(Color::DarkGray));
}

fn rank(a: Attention) -> u8 {
    match a {
        Attention::Idle => 0,
        Attention::Working => 1,
        Attention::Done => 2,
        Attention::NeedsInput => 3,
    }
}

fn glyph(rank: u8) -> (&'static str, Color) {
    match rank {
        3 => ("●", Color::Yellow),
        2 => ("✓", Color::Green),
        1 => ("◌", Color::Cyan),
        _ => (" ", Color::Reset),
    }
}

/// A tab's name: its own, or what its first pane runs, or where.
fn tab_label(name: Option<&str>, first: Option<&PaneInfo>) -> String {
    if let Some(n) = name {
        return n.to_owned();
    }
    first
        .and_then(|p| {
            p.command.clone().or_else(|| p.cwd.as_deref().map(|c| c.rsplit('/').next().unwrap_or(c).to_owned()))
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "shell".into())
}

fn sidebar(app: &mut App, buf: &mut Buffer, width: u16, height: u16) {
    let mut hits = Vec::new();
    let w = width as usize;
    let selected = match app.mode {
        Mode::Sidebar { sel } => Some(sel),
        _ => None,
    };
    let mut nav = 0usize;
    let mut y = 0u16;
    let line = |buf: &mut Buffer, y: &mut u16, text: &str, style: Style| {
        if *y < height {
            buf.set_stringn(0, *y, format!("{text:<w$}"), w, style);
        }
        *y += 1;
    };
    line(buf, &mut y, " Arugula", Style::default().add_modifier(Modifier::BOLD));
    let Some(state) = &app.state else { return };
    for s in &state.sessions {
        y += 1;
        if y < height {
            hits.push((y, Hit::Session(s.id)));
        }
        line(buf, &mut y, &format!(" {}", s.name), Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD));
        for t in &s.tabs {
            let Some(tv) = state.tabs.iter().find(|v| v.id == *t) else { continue };
            let panes: Vec<&PaneInfo> =
                tv.root.panes().iter().filter_map(|p| state.panes.iter().find(|i| i.id == *p)).collect();
            let label = tab_label(tv.name.as_deref(), panes.first().copied());
            let (g, gc) = glyph(panes.iter().map(|p| rank(p.attention)).max().unwrap_or(0));
            let shown = Some(*t) == app.tab;
            let chosen = selected == Some(nav);
            nav += 1;
            let mut text: String = format!("  {} {label}", if shown { "▸" } else { " " }).chars().take(w - 3).collect();
            while text.chars().count() < w - 2 {
                text.push(' ');
            }
            let style = if chosen {
                Style::default().bg(Color::Cyan).fg(Color::Black)
            } else if shown {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            if y < height {
                buf.set_stringn(0, y, format!("{text}{g} "), w, style);
                if let Some(c) = buf.cell_mut((width - 2, y)) {
                    c.set_fg(gc);
                }
                hits.push((y, Hit::Tab(*t)));
            }
            y += 1;
        }
    }
    let wanting: Vec<(u32, u8, String, Option<u32>)> = app
        .wanting()
        .into_iter()
        .map(|p| {
            let what = p
                .reason
                .as_ref()
                .map(|r| r.headline.clone())
                .or_else(|| p.command.clone())
                .unwrap_or_else(|| "wants you".into());
            (p.id, rank(p.attention), what, app.tab_of(p.id))
        })
        .collect();
    if !wanting.is_empty() {
        y += 1;
        line(buf, &mut y, " needs you", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
        for (id, r, what, tab) in wanting {
            let Some(tab) = tab else { continue };
            let chosen = selected == Some(nav);
            nav += 1;
            let (g, gc) = glyph(r);
            let style = if chosen { Style::default().bg(Color::Cyan).fg(Color::Black) } else { Style::default() };
            if y < height {
                buf.set_stringn(0, y, format!("{:<w$}", format!("  {g} %{id} {what}")), w, style);
                if !chosen && let Some(c) = buf.cell_mut((2, y)) {
                    c.set_fg(gc);
                }
                hits.push((y, Hit::Wants(id, tab)));
            }
            y += 1;
        }
    }
    app.hits = hits;
}

fn dividers(app: &App, buf: &mut Buffer) {
    let Some(tab) = app.tab_view() else { return };
    let area = app.area;
    let focus = tab.layout.panes.iter().find(|(p, _)| Some(*p) == app.focus).map(|(_, r)| *r);
    for s in &tab.layout.splits {
        let r = s.rect;
        let mut at = 0;
        for i in 0..s.extents.len().saturating_sub(1) {
            at += s.extents[i];
            let len = if s.dir == Dir::Row { r.rows } else { r.cols };
            for k in 0..len {
                let (x, y, sym) = match s.dir {
                    Dir::Row => (r.x + at, r.y + k, "│"),
                    Dir::Column => (r.x + k, r.y + at, "─"),
                };
                // Lit where it borders the focused pane.
                let lit = focus.is_some_and(|f| x + 1 >= f.x && x <= f.x + f.cols && y + 1 >= f.y && y <= f.y + f.rows);
                if let Some(c) = buf.cell_mut((area.x + x, area.y + y)) {
                    c.set_symbol(sym).set_style(
                        Style::default().fg(if lit { Color::Cyan } else { Color::DarkGray }).bg(Color::Reset),
                    );
                }
            }
            at += 1;
        }
    }
}

/// Where a moved pane would land.
fn landing(buf: &mut Buffer, r: Rect, edge: Edge) {
    let half = |n: u16| (n / 2).max(1);
    let zone = match edge {
        Edge::Left => Rect { width: half(r.width), ..r },
        Edge::Right => Rect { x: r.x + r.width - half(r.width), width: half(r.width), ..r },
        Edge::Top => Rect { height: half(r.height), ..r },
        Edge::Bottom => Rect { y: r.y + r.height - half(r.height), height: half(r.height), ..r },
        Edge::Center => r,
    };
    buf.set_style(zone, Style::default().bg(Color::Blue));
}

fn status(app: &App, buf: &mut Buffer, full: Rect) {
    let y = full.height.saturating_sub(1);
    let w = full.width as usize;
    let dim = Style::default().fg(Color::DarkGray);
    let (left, style) = match &app.mode {
        Mode::Prefix => (
            " v split │ s down │ c tab │ x close │ o next │ z zoom │ [ copy │ w sidebar │ m menu │ ? all │ q detach"
                .to_owned(),
            Style::default().bg(Color::Cyan).fg(Color::Black),
        ),
        Mode::Copy => (
            match &app.toast {
                Some((t, at)) if at.elapsed() < TOAST => format!(" COPY  {t}"),
                _ => {
                    " COPY  hjkl move │ v V select │ y copy │ / ? search │ n N again │ [ ] prompts │ o output │ q leave"
                        .to_owned()
                }
            },
            Style::default().bg(Color::Yellow).fg(Color::Black),
        ),
        Mode::Sidebar { .. } => (
            " ↑↓ choose · Enter go there · a allow · A always · d deny · x dismiss · Esc back".to_owned(),
            Style::default().bg(Color::Cyan).fg(Color::Black),
        ),
        _ if app.moving.is_some() => (
            " Move the pane: let go (or click) on another pane's edge, or its middle to swap · Esc cancels".to_owned(),
            Style::default().bg(Color::Blue),
        ),
        _ => match &app.toast {
            Some((t, at)) if at.elapsed() < TOAST => (format!(" {t}"), Style::default().fg(Color::Yellow)),
            _ => {
                let what = app
                    .focus
                    .map(|p| {
                        let title = app.panes.get(&p).map(|t| t.engine.title()).filter(|t| !t.is_empty());
                        let info = app.info(p);
                        let block = app.blocks.get(&p).and_then(|b| {
                            b["label"].as_str().or(b["agent"].as_str()).or(b["url"].as_str()).map(str::to_owned)
                        });
                        let label = title
                            .or(block)
                            .or_else(|| info.and_then(|i| i.command.clone()))
                            .or_else(|| info.and_then(|i| i.cwd.clone()))
                            .unwrap_or_default();
                        format!(" %{p} {label}")
                    })
                    .unwrap_or_default();
                (what, dim)
            }
        },
    };
    buf.set_stringn(0, y, format!("{left:<w$}"), w, style);
    if matches!(app.mode, Mode::Normal) && app.moving.is_none() {
        let hint = " ^] menu ";
        let x = full.width.saturating_sub(hint.len() as u16);
        buf.set_string(x, y, hint, dim);
    }
}

/// Where a menu sits on the screen.
fn menu_rect(m: &Menu, full: Rect) -> Rect {
    let width = m.items.iter().map(|i| i.label.chars().count()).max().unwrap_or(0) as u16 + 6;
    let height = m.items.len() as u16 + 2;
    let width = width.min(full.width);
    let height = height.min(full.height);
    let x = m.x.min(full.width.saturating_sub(width));
    let y = m.y.min(full.height.saturating_sub(height));
    Rect::new(x, y, width, height)
}

pub fn menu_item_at(m: &Menu, full: Rect, x: u16, y: u16) -> Option<usize> {
    let r = menu_rect(m, full);
    (x > r.x && x + 1 < r.right() && y > r.y && y + 1 < r.bottom()).then(|| (y - r.y - 1) as usize)
}

fn menu(buf: &mut Buffer, full: Rect, m: &Menu) {
    let r = menu_rect(m, full);
    frame(buf, r);
    for (i, item) in m.items.iter().enumerate() {
        let y = r.y + 1 + i as u16;
        if y + 1 >= r.bottom() {
            break;
        }
        let heading = matches!(item.act, Act::None);
        let text = if heading {
            format!(" {}", item.label)
        } else {
            format!(" {} {}", if item.checked { "✓" } else { " " }, item.label)
        };
        let style = if heading {
            Style::default().fg(Color::DarkGray)
        } else if i == m.sel {
            Style::default().bg(Color::Cyan).fg(Color::Black)
        } else {
            Style::default()
        };
        let inner = (r.width - 2) as usize;
        buf.set_stringn(r.x + 1, y, format!("{text:<inner$}"), inner, style);
    }
}

fn frame(buf: &mut Buffer, r: Rect) {
    buf.set_style(r, Style::default().bg(Color::Black).fg(Color::Gray));
    for x in r.x..r.right() {
        for y in r.y..r.bottom() {
            let sym = match (x == r.x, x + 1 == r.right(), y == r.y, y + 1 == r.bottom()) {
                (true, _, true, _) => "┌",
                (_, true, true, _) => "┐",
                (true, _, _, true) => "└",
                (_, true, _, true) => "┘",
                (_, _, true, _) | (_, _, _, true) => "─",
                (true, _, _, _) | (_, true, _, _) => "│",
                _ => " ",
            };
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_symbol(sym);
            }
        }
    }
}

fn prompt(buf: &mut Buffer, full: Rect, title: &str, text: &str) -> Option<Cursor> {
    let width = full.width.saturating_sub(4).min(64);
    let r = Rect::new((full.width - width) / 2, full.height / 3, width, 4.min(full.height));
    frame(buf, r);
    let inner = (width - 2) as usize;
    buf.set_stringn(
        r.x + 1,
        r.y + 1,
        format!(" {title}"),
        inner,
        Style::default().bg(Color::Black).add_modifier(Modifier::BOLD),
    );
    // The end of what's typed, if it's long.
    let shown: String = {
        let n = text.chars().count();
        text.chars().skip(n.saturating_sub(inner - 3)).collect()
    };
    buf.set_stringn(r.x + 1, r.y + 2, format!(" {shown:<w$}", w = inner - 1), inner, Style::default().bg(Color::Black));
    Some(Cursor {
        x: r.x + 2 + shown.chars().count() as u16,
        y: r.y + 2,
        shape: arugula_vt::CursorShape::Bar,
        blink: true,
        color: None,
    })
}
