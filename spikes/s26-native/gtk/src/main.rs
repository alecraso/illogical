//! `s26-gtk --socket DEV_SOCK --pane 1`: one pane of a dev daemon, drawn
//! natively. `s26-gtk --bench [--out results.json]`: S25's bench, no daemon.

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    time::Instant,
};

use gtk::{gdk, glib, prelude::*};
use s26_core::{Conn, Event};
use s26_gtk::{PAD, Paints, TermView, now, run_bench, shot};

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let bench = args.iter().any(|a| a == "--bench");
    let sock = arg("--socket").map(PathBuf::from);
    let pane: u32 = arg("--pane").and_then(|p| p.trim_start_matches('%').parse().ok()).unwrap_or(1);
    let out = arg("--out").map(PathBuf::from);
    let t0 = Instant::now();

    let app = gtk::Application::builder().application_id("wtf.widgets.illogical.s26").build();
    let started = Cell::new(false);
    app.connect_activate(move |app| {
        if started.replace(true) {
            return;
        }
        let view = TermView::new(160, 48);
        let win = gtk::ApplicationWindow::builder().application(app).title("illogical (S26, GTK)").child(&view).build();
        // The same pixel size as S25's bench terminal, for comparison.
        let (cw, ch) = view.with(|st| (st.cell_w, st.cell_h));
        win.set_default_size((160.0 * cw + 2.0 * PAD) as i32, (48.0 * ch + 2.0 * PAD) as i32);
        view.wire_input();

        // Paint times from the frame clock, for the bench and the HUD.
        let paints = Rc::new(RefCell::new(Paints::default()));
        {
            let paints = paints.clone();
            view.connect_realize(move |v| {
                let paints = paints.clone();
                v.frame_clock().unwrap().connect_after_paint(move |clock| {
                    let mut p = paints.borrow_mut();
                    p.times.push(now(t0));
                    p.counters.push(clock.frame_counter());
                    for w in p.waiters.drain(..) {
                        let _ = w.try_send(());
                    }
                });
            });
        }
        win.present();
        view.grab_focus();
        if std::env::var_os("S26_DEBUG").is_some() {
            let w2 = win.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                if let Some(t) = w2.surface().and_downcast::<gdk::Toplevel>() {
                    eprintln!("s26: toplevel state {:?} active {}", t.state(), w2.is_active());
                }
                glib::ControlFlow::Continue
            });
        }
        if std::env::var_os("S26_TICK").is_some() {
            let ticks = Rc::new(Cell::new(0u64));
            view.add_tick_callback(move |_, _| {
                ticks.set(ticks.get() + 1);
                if ticks.get() % 240 == 0 {
                    eprintln!("s26: {} ticks", ticks.get());
                }
                glib::ControlFlow::Continue
            });
        }
        println!("s26: window at {:.0} ms", now(t0));

        if std::env::var_os("S26_IDLE").is_some() {
            if let Ok(what) = std::env::var("S26_FEED") {
                let v = view.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(1000), move || {
                    eprintln!("s26: feeding {what}");
                    v.feed(&if what == "reset" { Event::Snapshot(Vec::new()) } else { Event::Output(b"hello\r\n".to_vec()) });
                });
            }
            return;
        }
        if bench {
            glib::spawn_future_local(run_bench(view, paints, t0, out.clone(), app.clone()));
            return;
        }
        let Some(sock) = sock.clone() else {
            eprintln!("--socket DEV_SOCK (a dev daemon's, never the daily one) or --bench");
            app.quit();
            return;
        };
        let (wake_tx, wake_rx) = async_channel::unbounded::<()>();
        let (cols, rows) = view.with(|st| st.grid);
        let conn = match Conn::attach(&sock, pane, cols, rows, move || {
            let _ = wake_tx.try_send(());
        }) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("s26: {e:#}");
                app.quit();
                return;
            }
        };
        view.with(|st| st.sender = Some(conn.sender.clone()));
        let app = app.clone();
        let first = Cell::new(true);
        // Below redraw priority: a pane flooding output must not starve frames.
        glib::MainContext::default().spawn_local_with_priority(glib::Priority::DEFAULT_IDLE, async move {
            while wake_rx.recv().await.is_ok() {
                while let Ok(e) = conn.events.try_recv() {
                    match &e {
                        Event::Closed(why) => {
                            println!("s26: closed: {why}");
                            app.quit();
                            return;
                        }
                        Event::Snapshot(_) if first.replace(false) => {
                            view.feed(&e);
                            println!("s26: first snapshot in at {:.0} ms", now(t0));
                            if let Ok(path) = std::env::var("S26_SHOT") {
                                // What the renderer draws, as a PNG, a few seconds in.
                                let v = view.clone();
                                glib::timeout_add_local_once(std::time::Duration::from_millis(
                                    std::env::var("S26_SHOT_MS").ok().and_then(|m| m.parse().ok()).unwrap_or(1500),
                                ), move || shot(&v, &path));
                            }
                        }
                        _ => view.feed(&e),
                    }
                }
            }
        });
    });
    app.run_with_args::<&str>(&[])
}
