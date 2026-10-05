//! S26 level A and the hybrid on Linux: libadwaita tabs for the daemon's
//! tabs. A tab that is one terminal gets a native terminal view (B2); any
//! other tab (splits, blocks) gets a WebKitGTK 6 view of the client, opened
//! at its first pane with the client's own tab bar hidden.
//!
//!     s26-adw --socket DEV_SOCK [--web-only] [--shot out.png]
//!
//! The daemon's page address comes from the state directory's `listen`
//! file next to the socket path (`--url` overrides it).

use std::{cell::Cell, path::PathBuf, time::Instant};

use adw::prelude::*;
use gtk::glib;
use s26_core::{Conn, Event, proto::BlockType};
use s26_gtk::{TermView, now};
use webkit6::prelude::*;

const HIDE_BAR: &str = ".bar { display: none !important; }";

fn web_view(url: &str) -> webkit6::WebView {
    let ucm = webkit6::UserContentManager::new();
    ucm.add_style_sheet(&webkit6::UserStyleSheet::new(
        HIDE_BAR,
        webkit6::UserContentInjectedFrames::TopFrame,
        webkit6::UserStyleLevel::User,
        &[],
        &[],
    ));
    let view = webkit6::WebView::builder().user_content_manager(&ucm).build();
    view.load_uri(url);
    view.set_hexpand(true);
    view.set_vexpand(true);
    view
}

fn term_view(sock: &std::path::Path, pane: u32, t0: Instant) -> TermView {
    let view = TermView::new(120, 40);
    view.wire_input();
    let (wake_tx, wake_rx) = async_channel::unbounded::<()>();
    let (cols, rows) = view.with(|st| st.grid);
    match Conn::attach(sock, pane, cols, rows, move || {
        let _ = wake_tx.try_send(());
    }) {
        Ok(conn) => {
            view.with(|st| st.sender = Some(conn.sender.clone()));
            let v = view.clone();
            glib::MainContext::default().spawn_local_with_priority(glib::Priority::DEFAULT_IDLE, async move {
                let first = Cell::new(true);
                while wake_rx.recv().await.is_ok() {
                    while let Ok(e) = conn.events.try_recv() {
                        if matches!(e, Event::Closed(_)) {
                            return;
                        }
                        if matches!(e, Event::Snapshot(_)) && first.replace(false) {
                            println!("s26-adw: pane %{pane} drawn natively at {:.0} ms", now(t0));
                        }
                        v.feed(&e);
                    }
                }
            });
        }
        Err(e) => eprintln!("s26-adw: %{pane}: {e:#}"),
    }
    view
}

// ---- level C: the swarm's attention rail, natively (a libadwaita list)

/// One row per pane: what it is, its state, and why it wants you.
fn fill_rail(list: &gtk::ListBox, state: &s26_core::proto::State) {
    use s26_core::proto::Attention;
    while let Some(row) = list.row_at_index(0) {
        list.remove(&row);
    }
    let mut panes: Vec<_> = state.panes.iter().collect();
    // What wants you first, as the web rail orders it.
    let rank = |a: &Attention| match a {
        Attention::NeedsInput => 0,
        Attention::Done => 1,
        Attention::Working => 2,
        Attention::Idle => 3,
    };
    panes.sort_by_key(|p| (rank(&p.attention), u32::from(p.id)));
    for p in panes {
        let title = p
            .command
            .clone()
            .or_else(|| p.cwd.as_deref().map(|c| c.rsplit('/').next().unwrap_or(c).to_string()))
            .unwrap_or_else(|| format!("{:?}", p.kind));
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&format!("%{} {title}", u32::from(p.id)))).build();
        if let Some(r) = &p.reason {
            row.set_subtitle(&glib::markup_escape_text(&r.headline));
        }
        let (label, class) = match p.attention {
            Attention::NeedsInput => ("needs you", "error"),
            Attention::Done => ("done", "success"),
            Attention::Working => ("working", "accent"),
            Attention::Idle => ("idle", "dim-label"),
        };
        let badge = gtk::Label::new(Some(label));
        badge.add_css_class(class);
        badge.add_css_class("caption");
        row.add_suffix(&badge);
        row.set_activatable(true);
        unsafe { row.set_data("pane", u32::from(p.id)) };
        list.append(&row);
    }
}

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let sock = PathBuf::from(arg("--socket").expect("--socket DEV_SOCK (a dev daemon's)"));
    let web_only = args.iter().any(|a| a == "--web-only");
    let shot = arg("--shot");
    let arg_select: Option<i32> = arg("--select").and_then(|n| n.parse().ok());
    let url = arg("--url").unwrap_or_else(|| {
        let state = std::env::var("S26_STATE_DIR").map(PathBuf::from).expect("--url or S26_STATE_DIR (for its listen file)");
        format!("http://{}", std::fs::read_to_string(state.join("listen")).expect("listen file").trim())
    });
    let t0 = Instant::now();

    let app = adw::Application::builder().application_id("wtf.widgets.illogical.s26adw").build();
    app.connect_activate(move |app| {
        let state = match s26_core::hello(&sock) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("s26-adw: {e:#}");
                return;
            }
        };
        let tabs = adw::TabView::new();
        let bar = adw::TabBar::builder().view(&tabs).autohide(false).build();
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&bar));
        for tab in &state.tabs {
            let panes: Vec<u32> = tab.layout.panes.iter().map(|(p, _)| u32::from(*p)).collect();
            let Some(&first) = panes.first() else { continue };
            let kind = state.panes.iter().find(|p| u32::from(p.id) == first).map(|p| p.kind.clone()).unwrap_or_default();
            let native = !web_only && panes.len() == 1 && kind == BlockType::Terminal;
            let title = tab.name.clone().unwrap_or_else(|| format!("tab {}", u32::from(tab.id)));
            let child: gtk::Widget = if native {
                term_view(&sock, first, t0).upcast()
            } else {
                web_view(&format!("{url}/#pane={first}")).upcast()
            };
            let page = tabs.append(&child);
            page.set_title(&format!("{title}{}", if native { "" } else { " (web)" }));
            println!("s26-adw: tab {title}: {} panes, {:?}, {}", panes.len(), kind, if native { "native" } else { "web" });
        }
        if let Some(n) = arg_select {
            tabs.set_selected_page(&tabs.nth_page(n));
        }
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&header);
        body.append(&tabs);

        // The rail, beside the tabs; a click opens the pane's tab.
        let rail = gtk::ListBox::new();
        rail.add_css_class("navigation-sidebar");
        fill_rail(&rail, &state);
        let tab_of: Vec<(u32, i32)> = state
            .tabs
            .iter()
            .enumerate()
            .flat_map(|(i, t)| t.layout.panes.iter().map(move |(p, _)| (u32::from(*p), i as i32)))
            .collect();
        {
            let tabs = tabs.clone();
            rail.connect_row_activated(move |_, row| {
                let pane: u32 = unsafe { row.data::<u32>("pane").map(|p| *p.as_ptr()).unwrap_or(0) };
                if let Some((_, i)) = tab_of.iter().find(|(p, _)| *p == pane) {
                    tabs.set_selected_page(&tabs.nth_page(*i));
                }
            });
        }
        {
            let (rail, sock) = (rail.clone(), sock.clone());
            glib::timeout_add_seconds_local(2, move || {
                if let Ok(state) = s26_core::hello(&sock) {
                    fill_rail(&rail, &state);
                }
                glib::ControlFlow::Continue
            });
        }
        let side = gtk::Box::new(gtk::Orientation::Vertical, 0);
        side.append(&adw::HeaderBar::builder().show_end_title_buttons(false).title_widget(&gtk::Label::new(Some("Swarm"))).build());
        let scroller = gtk::ScrolledWindow::builder().child(&rail).vexpand(true).build();
        side.append(&scroller);
        let split = adw::OverlaySplitView::builder().sidebar(&side).content(&body).min_sidebar_width(260.0).build();
        let win = adw::ApplicationWindow::builder().application(app).title("illogical (S26, libadwaita)").content(&split).default_width(1500).default_height(850).build();
        win.present();
        println!("s26-adw: window at {:.0} ms", now(t0));
        if let Some(path) = shot.clone() {
            let w = win.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(4000), move || {
                let paintable = gtk::WidgetPaintable::new(Some(&w));
                let snap = gtk::Snapshot::new();
                paintable.snapshot(&snap, w.width() as f64, w.height() as f64);
                if let (Some(node), Some(r)) = (snap.to_node(), w.renderer()) {
                    match r.render_texture(&node, None).save_to_png(&path) {
                        Ok(()) => println!("s26-adw: saved {path}"),
                        Err(e) => println!("s26-adw: screenshot failed: {e}"),
                    }
                }
            });
        }
    });
    app.run_with_args::<&str>(&[])
}
