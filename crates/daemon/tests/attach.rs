//! End to end against the real binary: attach, input, detach, resume.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use illogical_proto::{AttachPane, ClientMsg, Edge, Frame, FrameKind, Intent, ServerMsg, State};
use illogical_testkit::{Daemon, illogicald};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A daemon on a port of its choosing, with a fresh state dir. It's up
/// when its own Unix socket answers.
async fn start() -> Daemon {
    illogicald!("attach").env("PS1", "$ ").start()
}

#[derive(Debug)]
enum In {
    Msg(ServerMsg),
    Frame(Frame),
}

/// The next message that isn't a layout update: what attaching sends (size,
/// then snapshot), even if a state change (a pane's cwd, say) lands first.
async fn recv_attach(ws: &mut Ws) -> In {
    loop {
        match recv(ws).await {
            In::Msg(ServerMsg::State { .. } | ServerMsg::Delta { .. }) => {}
            m => return m,
        }
    }
}

async fn recv(ws: &mut Ws) -> In {
    loop {
        let msg = timeout(Duration::from_secs(5), ws.next()).await.expect("timed out").unwrap().unwrap();
        match msg {
            Message::Text(t) => return In::Msg(serde_json::from_str(&t).unwrap()),
            Message::Binary(b) => return In::Frame(Frame::decode(&b).unwrap()),
            _ => {}
        }
    }
}

async fn connect(d: &Daemon) -> (Ws, u64) {
    let (ws, state) = connect_state(d).await;
    (ws, state.panes[0].epoch)
}

async fn connect_state(d: &Daemon) -> (Ws, State) {
    let (mut ws, _) = connect_async(d.ws("/ws")).await.unwrap();
    let In::Msg(ServerMsg::Hello { state, .. }) = recv(&mut ws).await else { panic!("expected hello") };
    assert!(!state.panes.is_empty(), "the daemon starts with a session");
    (ws, state)
}

async fn send(ws: &mut Ws, msg: ClientMsg) {
    ws.send(Message::Text(serde_json::to_string(&msg).unwrap().into())).await.unwrap();
}

/// Skip messages until the state satisfies `f` (other state, such as a
/// new prompt or directory, may arrive first). Starts from a fresh `State`
/// (`subscribe` answers with one) and follows the deltas after it (M23).
async fn state_where(ws: &mut Ws, f: impl Fn(&State) -> bool) -> State {
    send(ws, ClientMsg::Subscribe { summary: false }).await;
    let mut cur: Option<State> = None;
    until(ws, |m| {
        match m {
            In::Msg(ServerMsg::State { state }) => cur = Some(state.clone()),
            In::Msg(ServerMsg::Delta { delta }) => {
                if let Some(c) = &mut cur {
                    c.apply(delta);
                }
            }
            _ => return None,
        }
        cur.as_ref().filter(|s| f(s)).cloned()
    })
    .await
}

/// Skip everything until a message matches.
async fn until<T>(ws: &mut Ws, mut f: impl FnMut(&In) -> Option<T>) -> T {
    loop {
        let m = recv(ws).await;
        if let Some(t) = f(&m) {
            return t;
        }
    }
}

async fn attach(ws: &mut Ws, offset: Option<u64>) {
    let m = ClientMsg::Attach { panes: vec![AttachPane::new(1, offset)], zstd: false, acks: false, kitty_keys: false };
    ws.send(Message::Text(serde_json::to_string(&m).unwrap().into())).await.unwrap();
}

async fn type_line(ws: &mut Ws, line: &str) {
    type_in(ws, 1, line).await;
}

async fn type_in(ws: &mut Ws, pane: u32, line: &str) {
    let f = Frame { kind: FrameKind::Input, pane, offset: 0, data: format!("{line}\r").into_bytes() };
    ws.send(Message::Binary(f.encode().into())).await.unwrap();
}

/// Read output frames until `needle` shows up; returns the end offset and
/// checks that frames are contiguous starting at `from`.
async fn read_until(ws: &mut Ws, mut from: Option<u64>, needle: &str) -> u64 {
    let mut seen = String::new();
    loop {
        match recv(ws).await {
            In::Frame(f) if f.kind == FrameKind::Output => {
                if let Some(expected) = from {
                    assert_eq!(f.offset, expected, "gap or overlap in output stream");
                }
                from = Some(f.offset + f.data.len() as u64);
                seen.push_str(&String::from_utf8_lossy(&f.data));
                if seen.contains(needle) {
                    return from.unwrap();
                }
            }
            In::Frame(f) => panic!("unexpected {:?} frame", f.kind),
            In::Msg(_) => {}
        }
    }
}

/// Read output until `then` has come after `first`.
async fn read_until_after(ws: &mut Ws, first: &str, then: &str) {
    let mut seen = String::new();
    loop {
        if let In::Frame(f) = recv(ws).await
            && f.kind == FrameKind::Output
        {
            seen.push_str(&String::from_utf8_lossy(&f.data));
            if seen.split_once(first).is_some_and(|(_, after)| after.contains(then)) {
                return;
            }
        }
    }
}

#[tokio::test]
async fn fresh_attach_gets_size_then_snapshot() {
    let d = start().await;
    let (mut ws, _) = connect(&d).await;
    attach(&mut ws, None).await;
    assert!(matches!(recv_attach(&mut ws).await, In::Msg(ServerMsg::Size { pane: 1, cols: 80, rows: 24, .. })));
    let In::Frame(f) = recv_attach(&mut ws).await else { panic!("expected snapshot") };
    assert_eq!(f.kind, FrameKind::Snapshot);
}

#[tokio::test]
async fn reconnect_resumes_from_offset_without_gaps() {
    let d = start().await;
    let (mut ws, epoch) = connect(&d).await;
    attach(&mut ws, None).await;
    let _size = recv_attach(&mut ws).await;
    let In::Frame(snap) = recv_attach(&mut ws).await else { panic!() };
    type_line(&mut ws, "echo hello-$((40+2))").await;
    let end = read_until(&mut ws, Some(snap.offset), "hello-42").await;
    drop(ws);

    // While nobody is attached, the shell keeps producing output.
    let (mut ws, epoch2) = connect(&d).await;
    assert_eq!(epoch, epoch2, "same daemon, same stream");
    type_line(&mut ws, "echo while-away-$((1+1))").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(ws);

    let (mut ws, _) = connect(&d).await;
    attach(&mut ws, Some(end)).await;
    // Replay starts exactly where we left off and includes what we missed.
    read_until(&mut ws, Some(end), "while-away-2").await;

    // From zero, the whole short history replays.
    let (mut ws, _) = connect(&d).await;
    attach(&mut ws, Some(0)).await;
    read_until(&mut ws, Some(0), "hello-42").await;
}

#[tokio::test]
async fn unknown_offset_falls_back_to_snapshot() {
    let d = start().await;
    let (mut ws, _) = connect(&d).await;
    attach(&mut ws, Some(10_000_000)).await;
    let _size = recv_attach(&mut ws).await;
    let In::Frame(f) = recv_attach(&mut ws).await else { panic!() };
    assert_eq!(f.kind, FrameKind::Snapshot);
}

#[tokio::test]
async fn snapshot_shows_a_full_screen_app() {
    let d = start().await;
    let (mut ws, _) = connect(&d).await;
    attach(&mut ws, None).await;
    let _ = (recv_attach(&mut ws).await, recv_attach(&mut ws).await);
    // A tiny full-screen "app": alt screen, draw, wait.
    type_line(&mut ws, r"printf '\e[?1049h\e[H\e[2JFULLSCREEN-%s' $((6*7)); sleep 30").await;
    read_until(&mut ws, None, "FULLSCREEN-42").await;
    drop(ws);

    let (mut ws, _) = connect(&d).await;
    attach(&mut ws, None).await;
    let _size = recv_attach(&mut ws).await;
    let In::Frame(f) = recv_attach(&mut ws).await else { panic!() };
    let text = String::from_utf8_lossy(&f.data);
    assert!(text.contains("\x1b[?1049h"), "snapshot enters the alt screen");
    assert!(text.contains("FULLSCREEN-42"), "snapshot has the app's screen");
}

/// A fresh snapshot of pane 1 at 80x24, asked for with `history` and `zstd`.
async fn snapshot_of(d: &Daemon, history: Option<u32>, zstd: bool) -> Frame {
    let (mut ws, _) = connect(d).await;
    let panes = vec![AttachPane { pane: 1, offset: None, history }];
    send(&mut ws, ClientMsg::Attach { panes, zstd, acks: false, kitty_keys: false }).await;
    let _size = recv_attach(&mut ws).await;
    let In::Frame(f) = recv_attach(&mut ws).await else { panic!("expected a snapshot") };
    f
}

/// What a snapshot draws, as lines of text with the scrollback first.
fn lines_of(snapshot: &[u8]) -> Vec<String> {
    use illogical_vt::{GhosttyEngine, VtEngine};
    let mut e = GhosttyEngine::new(80, 24);
    e.feed(snapshot);
    e.plain_text().lines().map(|l| l.trim_end().to_owned()).filter(|l| !l.is_empty()).collect()
}

#[tokio::test]
async fn snapshots_carry_only_the_history_asked_for() {
    let d = start().await;
    let (mut ws, _) = connect(&d).await;
    attach(&mut ws, None).await;
    let _ = (recv_attach(&mut ws).await, recv_attach(&mut ws).await);
    type_line(&mut ws, "seq 1 3000; echo seq-$((1+1))-done").await;
    // Its prompt too: the snapshots below must all see the same screen.
    read_until_after(&mut ws, "seq-2-done", "$ ").await;
    drop(ws);

    let full = snapshot_of(&d, None, false).await;
    assert_eq!(full.kind, FrameKind::Snapshot);
    let all = lines_of(&full.data);
    assert!(all.iter().any(|l| l == "1") && all.iter().any(|l| l == "3000"), "the full snapshot has it all");

    // 100 rows of history and the screen: the newest of it.
    let capped = snapshot_of(&d, Some(100), false).await;
    let some = lines_of(&capped.data);
    assert!(some.len() <= 100 + 24, "{} lines", some.len());
    assert!(all.ends_with(&some), "the newest rows");
    assert!(some.iter().any(|l| l == "3000") && !some.iter().any(|l| l == "2800"));

    // Compressed, it's the same bytes.
    let packed = snapshot_of(&d, Some(100), true).await;
    assert_eq!(packed.kind, FrameKind::SnapshotZstd);
    assert!(packed.data.len() < capped.data.len() / 2, "{} of {}", packed.data.len(), capped.data.len());
    assert_eq!(zstd::decode_all(&packed.data[..]).unwrap(), capped.data);

    // The screen only, after a resync.
    let screen = lines_of(&snapshot_of(&d, Some(0), false).await.data);
    assert!(screen.len() <= 24 && all.ends_with(&screen), "{screen:?}");
}

/// Attach pane 1 as a client that acks, and take its size and snapshot;
/// the offset it has drawn.
async fn attach_acking(ws: &mut Ws) -> u64 {
    send(ws, ClientMsg::Attach { panes: vec![AttachPane::new(1, None)], zstd: false, acks: true, kitty_keys: false })
        .await;
    let _size = recv_attach(ws).await;
    let In::Frame(f) = recv_attach(ws).await else { panic!("expected a snapshot") };
    f.offset
}

/// Read output without acking until it stops coming: the end offset, and
/// whether a resync came instead.
async fn read_until_held(ws: &mut Ws, mut end: u64) -> (u64, bool) {
    loop {
        match timeout(Duration::from_millis(1500), ws.next()).await {
            Err(_) => return (end, false),
            Ok(Some(Ok(Message::Binary(b)))) => {
                let f = Frame::decode(&b).unwrap();
                assert_eq!(f.offset, end, "gap or overlap in output stream");
                end += f.data.len() as u64;
            }
            Ok(Some(Ok(Message::Text(t)))) => {
                if matches!(serde_json::from_str(&t), Ok(ServerMsg::Resync { .. })) {
                    return (end, true);
                }
            }
            Ok(_) => panic!("connection ended"),
        }
    }
}

#[tokio::test]
async fn a_client_that_acks_is_held_back_then_gets_what_it_missed() {
    let d = start().await;
    let (mut ws, _) = connect(&d).await;
    let start = attach_acking(&mut ws).await;
    // About 1.2 MB: past the 512 KB window, and what's held back still fits
    // in the 1 MB the log replays.
    type_line(&mut ws, "head -c 1200000 /dev/zero | tr '\\0' x; echo; echo burst-$((1+1))").await;
    let (held, resynced) = read_until_held(&mut ws, start).await;
    assert!(!resynced, "held back, not resynced");
    let got = held - start;
    assert!(got > 256 * 1024 && got < 700 * 1024, "sent {got} bytes before holding back");

    // Drawn it all: the rest comes, from exactly where it stopped.
    send(&mut ws, ClientMsg::Ack { pane: 1, offset: held }).await;
    read_until(&mut ws, Some(held), "burst-2").await;
}

#[tokio::test]
async fn a_client_held_back_past_what_the_log_replays_resyncs() {
    let d = start().await;
    let (mut ws, _) = connect(&d).await;
    let start = attach_acking(&mut ws).await;
    type_line(&mut ws, "head -c 3000000 /dev/zero | tr '\\0' x; echo; echo burst-$((2+2))").await;
    let (held, resynced) = read_until_held(&mut ws, start).await;
    assert!(!resynced);
    // Let the rest pile up in the log while it's held back.
    tokio::time::sleep(Duration::from_millis(500)).await;
    send(&mut ws, ClientMsg::Ack { pane: 1, offset: held }).await;
    until(&mut ws, |m| matches!(m, In::Msg(ServerMsg::Resync { pane: 1 })).then_some(())).await;
}

#[tokio::test]
async fn a_held_back_client_that_resizes_starts_over() {
    let d = start().await;
    let (mut ws, state) = connect_state(&d).await;
    let tab = state.tabs[0].id;
    let start = attach_acking(&mut ws).await;
    type_line(&mut ws, "head -c 1200000 /dev/zero | tr '\\0' x; sleep 30").await;
    let (_, resynced) = read_until_held(&mut ws, start).await;
    assert!(!resynced);
    // What it missed was printed for the old size: don't replay it.
    send(&mut ws, ClientMsg::View { tab, cols: 90, rows: 20, zoom: None, claim: true }).await;
    until(&mut ws, |m| matches!(m, In::Msg(ServerMsg::Resync { pane: 1 })).then_some(())).await;
}

#[tokio::test]
async fn ctrl_c_stops_a_flood_at_once() {
    let d = start().await;
    let (mut ws, _) = connect(&d).await;
    let mut end = attach_acking(&mut ws).await;
    type_line(&mut ws, "yes flood-line").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    // Keys go ahead of the output still waiting to be taken in.
    let f = Frame { kind: FrameKind::Input, pane: 1, offset: 0, data: b"\x03".to_vec() };
    ws.send(Message::Binary(f.encode().into())).await.unwrap();
    let pressed = tokio::time::Instant::now();
    type_line(&mut ws, "echo stopped-$((3+3))").await;
    // Draw everything as it comes (acking) until the shell answers.
    let mut seen = String::new();
    loop {
        assert!(pressed.elapsed() < Duration::from_secs(5), "still flooding 5 s after Ctrl-C");
        match recv(&mut ws).await {
            In::Frame(f) if f.kind == FrameKind::Output => {
                end = end.max(f.offset + f.data.len() as u64);
                seen = format!("{}{}", &seen[seen.len().saturating_sub(64)..], String::from_utf8_lossy(&f.data));
                send(&mut ws, ClientMsg::Ack { pane: 1, offset: end }).await;
                if seen.contains("stopped-6") {
                    break;
                }
            }
            // After a resync: the screen, where the answer may be already.
            In::Frame(f) => {
                end = f.offset;
                if String::from_utf8_lossy(&f.data).contains("stopped-6") {
                    break;
                }
            }
            In::Msg(ServerMsg::Resync { pane: 1 }) => {
                let panes = vec![AttachPane { pane: 1, offset: Some(end), history: Some(0) }];
                send(&mut ws, ClientMsg::Attach { panes, zstd: false, acks: true, kitty_keys: false }).await;
            }
            In::Msg(_) => {}
        }
    }
}

#[tokio::test]
async fn programs_hear_of_kitty_keys_while_a_client_that_speaks_them_is_attached() {
    let d = start().await;
    // Ask the terminal for its kitty keyboard flags, as Neovim does, and
    // print what came back (nothing: the read times out).
    let ask = r#"printf '\033[?u'; IFS= read -rs -d u -t 1 r; echo "kitty[${r#*\?}]""#;

    let (mut ws, _) = connect(&d).await;
    attach(&mut ws, None).await;
    let _ = (recv_attach(&mut ws).await, recv_attach(&mut ws).await);
    type_line(&mut ws, ask).await;
    read_until(&mut ws, None, "kitty[]").await;
    drop(ws);

    let (mut ws, _) = connect(&d).await;
    send(
        &mut ws,
        ClientMsg::Attach { panes: vec![AttachPane::new(1, None)], zstd: false, acks: false, kitty_keys: true },
    )
    .await;
    let _ = (recv_attach(&mut ws).await, recv_attach(&mut ws).await);
    type_line(&mut ws, ask).await;
    read_until(&mut ws, None, "kitty[0]").await;
}

#[tokio::test]
async fn rejects_foreign_host_and_origin() {
    let d = start().await;
    let mut req = d.ws("/ws");
    req.headers_mut().insert("origin", "https://evil.example".parse().unwrap());
    assert!(connect_async(req).await.is_err(), "foreign origin must be refused");

    let mut req = d.ws("/ws");
    req.headers_mut().insert("host", "evil.example".parse().unwrap());
    assert!(connect_async(req).await.is_err(), "foreign host must be refused");

    let mut req = d.ws("/ws");
    req.headers_mut().insert("tailscale-user-login", "someone@else".parse().unwrap());
    assert!(connect_async(req).await.is_err(), "tailnet user without --owner must be refused");
}

#[tokio::test]
async fn pages_refuse_to_be_framed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let d = start().await;
    let mut s = TcpStream::connect(("127.0.0.1", d.port)).await.unwrap();
    let req = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
        d.port,
        d.token()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut res = Vec::new();
    timeout(Duration::from_secs(5), s.read_to_end(&mut res)).await.unwrap().unwrap();
    let head = String::from_utf8_lossy(&res).to_ascii_lowercase();
    let head = head.split("\r\n\r\n").next().unwrap();
    assert!(head.contains("content-security-policy: frame-ancestors 'none'"), "{head}");
    assert!(head.contains("x-frame-options: deny"), "{head}");
}

#[tokio::test]
async fn slow_client_is_resynced() {
    let d = start().await;
    let (mut ws, _) = connect(&d).await;
    attach(&mut ws, None).await;
    let _ = (recv_attach(&mut ws).await, recv_attach(&mut ws).await);
    type_line(&mut ws, "head -c 300000000 /dev/zero | tr '\\0' x").await;
    // Don't read while the output piles up.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        assert!(tokio::time::Instant::now() < deadline, "never resynced");
        if let In::Msg(ServerMsg::Resync { pane: 1 }) = recv(&mut ws).await {
            break;
        }
    }
    // Re-attaching after a resync gets a snapshot.
    attach(&mut ws, None).await;
    loop {
        if let In::Frame(f) = recv(&mut ws).await
            && f.kind == FrameKind::Snapshot
        {
            break;
        }
    }
}

#[tokio::test]
async fn split_spawns_a_pane_and_exit_closes_it() {
    let d = start().await;
    let (mut ws, _) = connect_state(&d).await;
    send(
        &mut ws,
        ClientMsg::Intent {
            id: Some(1),
            intent: Intent::Split { pane: 1, edge: Edge::Right, local: false, cwd: None },
        },
    )
    .await;
    let state = state_where(&mut ws, |s| s.panes.len() == 2).await;
    let ids: Vec<u32> = state.panes.iter().map(|p| p.id).collect();
    assert_eq!(ids, vec![1, 2]);
    let tab = &state.tabs[0];
    assert_eq!(tab.layout.panes.len(), 2);
    assert_eq!((tab.layout.panes[0].1.cols, tab.layout.panes[1].1.cols), (40, 39));

    // The new pane runs a shell of its own.
    send(
        &mut ws,
        ClientMsg::Attach { panes: vec![AttachPane::new(2, None)], zstd: false, acks: false, kitty_keys: false },
    )
    .await;
    type_in(&mut ws, 2, "echo in-pane-$((1+1)); exit").await;
    let state = until(&mut ws, |m| match m {
        In::Msg(ServerMsg::State { state }) if state.panes.len() == 1 => Some(state.clone()),
        _ => None,
    })
    .await;
    assert_eq!(state.panes[0].id, 1);
    assert_eq!(state.tabs[0].layout.panes.len(), 1, "exiting closed the split");
}

#[tokio::test]
async fn the_tab_takes_the_claiming_clients_size() {
    let d = start().await;
    let (mut a, state) = connect_state(&d).await;
    let tab = state.tabs[0].id;
    send(
        &mut a,
        ClientMsg::Attach { panes: vec![AttachPane::new(1, None)], zstd: false, acks: false, kitty_keys: false },
    )
    .await;
    send(&mut a, ClientMsg::View { tab, cols: 101, rows: 30, zoom: None, claim: true }).await;
    until(&mut a, |m| matches!(m, In::Msg(ServerMsg::Size { pane: 1, cols: 101, rows: 30 })).then_some(())).await;
    type_line(&mut a, "stty size").await;
    read_until(&mut a, None, "30 101").await;

    // Another client's unclaimed view doesn't take over; a claimed one does.
    let (mut b, _) = connect_state(&d).await;
    send(&mut b, ClientMsg::View { tab, cols: 60, rows: 20, zoom: None, claim: false }).await;
    send(&mut b, ClientMsg::View { tab, cols: 61, rows: 20, zoom: None, claim: true }).await;
    // Other state (prompts, directories) may come first; wait for the size.
    let state = until(&mut a, |m| match m {
        In::Msg(ServerMsg::State { state }) if state.tabs[0].cols != 101 => Some(state.clone()),
        _ => None,
    })
    .await;
    assert_eq!((state.tabs[0].cols, state.tabs[0].rows), (61, 20), "B's claim, not its plain view");
    assert_ne!(state.tabs[0].owner, None);
}

#[tokio::test]
async fn bad_intents_report_errors_and_others_see_changes() {
    let d = start().await;
    let (mut a, _) = connect_state(&d).await;
    let (mut b, _) = connect_state(&d).await;
    send(&mut a, ClientMsg::Intent { id: Some(9), intent: Intent::ClosePane { pane: 999 } }).await;
    let err = until(&mut a, |m| match m {
        In::Msg(ServerMsg::Error { id, message }) => Some((*id, message.clone())),
        _ => None,
    })
    .await;
    assert_eq!(err, (Some(9), "no pane %999".to_string()));

    send(&mut a, ClientMsg::Intent { id: None, intent: Intent::NewTab { session: 1, from_pane: Some(1), cwd: None } })
        .await;
    state_where(&mut b, |s| s.sessions[0].tabs.len() == 2).await;
}

// ---------------------------------------------------------------- M2: restore

use illogical_proto::{PaneOp, Policy};

async fn attach_pane(ws: &mut Ws, pane: u32) -> String {
    send(
        ws,
        ClientMsg::Attach { panes: vec![AttachPane::new(pane, None)], zstd: false, acks: false, kitty_keys: false },
    )
    .await;
    until(ws, |m| match m {
        In::Frame(f) if f.kind == FrameKind::Snapshot && f.pane == pane => {
            Some(String::from_utf8_lossy(&f.data).into_owned())
        }
        _ => None,
    })
    .await
}

/// Output from one pane until `needle` appears.
async fn read_pane_until(ws: &mut Ws, pane: u32, needle: &str) -> String {
    let mut seen = String::new();
    until(ws, |m| {
        if let In::Frame(f) = m
            && f.pane == pane
            && f.kind == FrameKind::Output
        {
            seen.push_str(&String::from_utf8_lossy(&f.data));
        }
        seen.contains(needle).then(|| seen.clone())
    })
    .await
}

#[tokio::test]
async fn a_clean_stop_brings_back_layout_scrollback_cwd_and_rerun() {
    let mut d = start().await;
    let (mut ws, s) = connect_state(&d).await;
    let tab = s.tabs[0].id;
    send(
        &mut ws,
        ClientMsg::Intent { id: None, intent: Intent::Split { pane: 1, edge: Edge::Right, local: false, cwd: None } },
    )
    .await;
    send(&mut ws, ClientMsg::Intent { id: None, intent: Intent::RenameTab { tab, name: Some("kept".into()) } }).await;
    attach_pane(&mut ws, 1).await;
    type_in(&mut ws, 1, "cd /tmp && echo marker-$((6*7))").await;
    read_pane_until(&mut ws, 1, "marker-42").await;
    attach_pane(&mut ws, 2).await;
    let policy = Policy::Rerun { confirm: true };
    send(&mut ws, ClientMsg::Pane { pane: 2, op: PaneOp::SetPolicy { policy: policy.clone() } }).await;
    // The trailing `true` stops bash from exec'ing into `sleep`, which would
    // leave only "sleep 300" to see (the M2 command capture reads /proc).
    type_in(&mut ws, 2, "bash -c 'echo rerun-ok-$((1+1)); sleep 300; true'").await;
    read_pane_until(&mut ws, 2, "rerun-ok-2").await;
    drop(ws);

    d.signal(nix::sys::signal::Signal::SIGTERM);
    d.start();
    let (mut ws, s) = connect_state(&d).await;
    assert_eq!(s.tabs.len(), 1);
    assert_eq!(s.tabs[0].name.as_deref(), Some("kept"));
    assert_eq!(s.tabs[0].layout.panes.len(), 2);
    let p2 = s.panes.iter().find(|p| p.id == 2).unwrap();
    assert_eq!(p2.policy, policy);
    assert!(!p2.running, "a confirm-first rerun waits");
    assert!(p2.command.as_deref().unwrap_or("").contains("sleep 300"), "{:?}", p2.command);

    // Scrollback is back, marked, and the shell starts where it was.
    let snap = attach_pane(&mut ws, 1).await;
    assert!(snap.contains("marker-42"), "scrollback restored");
    assert!(snap.contains("restored"), "restore marker");
    // Physical paths: /tmp is a link to /private/tmp on macOS.
    type_in(&mut ws, 1, "echo cwd=$(pwd -P)").await;
    let tmp = std::fs::canonicalize("/tmp").unwrap();
    read_pane_until(&mut ws, 1, &format!("cwd={}", tmp.display())).await;

    // The rerun pane shows what it would run; Enter runs it.
    let snap = attach_pane(&mut ws, 2).await;
    assert!(snap.contains("press Enter to re-run"), "banner: {snap}");
    type_in(&mut ws, 2, "").await;
    read_pane_until(&mut ws, 2, "rerun-ok-2").await;
    drop(ws);

    // It still knows its command, so the next restart offers it again
    // (#26: a second reboot).
    tokio::time::sleep(Duration::from_millis(1500)).await;
    d.signal(nix::sys::signal::Signal::SIGTERM);
    d.start();
    let (mut ws, s) = connect_state(&d).await;
    let p2 = s.panes.iter().find(|p| p.id == 2).unwrap();
    assert!(p2.command.as_deref().unwrap_or("").contains("sleep 300"), "after a rerun: {:?}", p2.command);
    let snap = attach_pane(&mut ws, 2).await;
    assert_eq!(snap.matches("press Enter to re-run").count(), 2, "banner again: {snap}");
    d.signal(nix::sys::signal::Signal::SIGTERM);
}

#[tokio::test]
async fn a_crash_loses_nothing_that_was_printed() {
    let mut d = start().await;
    let (mut ws, _) = connect_state(&d).await;
    attach_pane(&mut ws, 1).await;
    type_in(&mut ws, 1, "echo crash-$((5+5))").await;
    read_pane_until(&mut ws, 1, "crash-10").await;
    // Long enough for the first layout save, not for a checkpoint: the log
    // alone carries the output.
    tokio::time::sleep(Duration::from_millis(600)).await;
    d.signal(nix::sys::signal::Signal::SIGKILL);

    d.start();
    let (mut ws, s) = connect_state(&d).await;
    assert_eq!(s.panes.iter().map(|p| p.id).collect::<Vec<_>>(), vec![1]);
    assert!(attach_pane(&mut ws, 1).await.contains("crash-10"));
    d.signal(nix::sys::signal::Signal::SIGKILL);
}

#[tokio::test]
async fn idle_panes_are_checkpointed() {
    let mut d = start().await;
    let state = d.state.clone();
    let (mut ws, _) = connect_state(&d).await;
    attach_pane(&mut ws, 1).await;
    type_in(&mut ws, 1, "echo idle-$((2+2))").await;
    read_pane_until(&mut ws, 1, "idle-4").await;
    let ckpt = state.join("blocks/1/checkpoint");
    for _ in 0..80 {
        if ckpt.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ckpt.exists(), "checkpoint after ~5s idle");
    let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&ckpt).unwrap().permissions());
    assert_eq!(mode & 0o777, 0o600);
    d.signal(nix::sys::signal::Signal::SIGTERM);
}

#[tokio::test]
async fn a_killed_shell_keeps_its_pane_and_offers_a_new_one() {
    let d = start().await;
    let (mut ws, _) = connect_state(&d).await;
    attach_pane(&mut ws, 1).await;
    type_in(&mut ws, 1, "echo pid=$((0+$$))x").await;
    // The echoed command line also says "pid="; wait for digits.
    let mut seen = String::new();
    let pid: i32 = until(&mut ws, |m| {
        if let In::Frame(f) = m {
            seen.push_str(&String::from_utf8_lossy(&f.data));
        }
        seen.match_indices("pid=").find_map(|(i, _)| {
            let digits: String = seen[i + 4..].chars().take_while(char::is_ascii_digit).collect();
            (!digits.is_empty() && seen[i + 4 + digits.len()..].starts_with('x')).then(|| digits.parse().unwrap())
        })
    })
    .await;
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL).unwrap();
    read_pane_until(&mut ws, 1, "press Enter for a shell").await;
    let state = state_where(&mut ws, |s| !s.panes[0].running).await;
    assert_eq!(state.panes.len(), 1, "the pane stays");
    type_in(&mut ws, 1, "").await;
    type_in(&mut ws, 1, "echo again-$((3+3))").await;
    read_pane_until(&mut ws, 1, "again-6").await;
}

#[tokio::test]
async fn policy_none_waits_purge_forgets_and_closing_retires_history() {
    let mut d = start().await;
    let state = d.state.clone();
    let (mut ws, _) = connect_state(&d).await;
    attach_pane(&mut ws, 1).await;
    type_in(&mut ws, 1, "echo secret-$((9*9))").await;
    read_pane_until(&mut ws, 1, "secret-81").await;
    send(&mut ws, ClientMsg::Pane { pane: 1, op: PaneOp::Purge }).await;
    send(&mut ws, ClientMsg::Pane { pane: 1, op: PaneOp::SetPolicy { policy: Policy::None } }).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let logs: Vec<u8> = std::fs::read_dir(state.join("blocks/1"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("seg-"))
        .flat_map(|e| std::fs::read(e.path()).unwrap())
        .collect();
    assert!(!String::from_utf8_lossy(&logs).contains("secret-81"), "purged from disk");
    drop(ws);
    d.signal(nix::sys::signal::Signal::SIGTERM);

    d.start();
    let (mut ws, s) = connect_state(&d).await;
    assert!(!s.panes[0].running, "policy none: nothing runs");
    let snap = attach_pane(&mut ws, 1).await;
    assert!(!snap.contains("secret-81"), "purged from scrollback");
    assert!(snap.contains("press Enter for a shell"));
    type_in(&mut ws, 1, "").await;
    type_in(&mut ws, 1, "echo fresh-$((1+1))").await;
    read_pane_until(&mut ws, 1, "fresh-2").await;

    send(
        &mut ws,
        ClientMsg::Intent { id: None, intent: Intent::Split { pane: 1, edge: Edge::Right, local: false, cwd: None } },
    )
    .await;
    until(&mut ws, |m| matches!(m, In::Msg(ServerMsg::State { state }) if state.panes.len() == 2).then_some(())).await;
    assert!(state.join("blocks/2").exists());
    send(&mut ws, ClientMsg::Intent { id: None, intent: Intent::ClosePane { pane: 2 } }).await;
    for _ in 0..50 {
        if !state.join("blocks/2").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!state.join("blocks/2").exists(), "a closed pane leaves the live panes");
    let retired = std::fs::read_dir(state.join("closed"))
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("2-"));
    assert!(retired, "its history is kept a while under closed/");
    d.signal(nix::sys::signal::Signal::SIGTERM);
}

#[tokio::test]
async fn a_restored_pane_drops_the_dead_programs_input_modes() {
    let mut d = start().await;
    let (mut ws, _) = connect_state(&d).await;
    attach_pane(&mut ws, 1).await;
    // A "program" that turns on mouse and focus reporting, then is killed
    // with the daemon.
    type_in(&mut ws, 1, r"printf '\e[?1000h\e[?1006h\e[?1004h\e[?1hmodes-%s' on; sleep 300").await;
    read_pane_until(&mut ws, 1, "modes-on").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(ws);
    d.signal(nix::sys::signal::Signal::SIGTERM);

    d.start();
    let (mut ws, _) = connect_state(&d).await;
    let snap = attach_pane(&mut ws, 1).await;
    // A snapshot is drawn from the terminal's state (not the old bytes), so
    // any mode it turns on is one the terminal still has.
    assert!(snap.contains("restored"), "restore marker");
    for m in ["\x1b[?1000h", "\x1b[?1006h", "\x1b[?1004h", "\x1b[?1h"] {
        assert!(!snap.contains(m), "mode {m:?} survived the restore");
    }
    d.signal(nix::sys::signal::Signal::SIGTERM);
}

#[tokio::test]
async fn the_restore_marker_goes_below_what_a_program_drew_in_place() {
    let mut d = start().await;
    let (mut ws, _) = connect_state(&d).await;
    attach_pane(&mut ws, 1).await;
    // Like Claude Code's TUI: draws on the main screen, then leaves the
    // cursor above the bottom of what it drew.
    type_in(&mut ws, 1, r"clear; printf '\e[6;1Hdrawn-%s\e[7;1Hlast-%s\e[2;1H' below row; sleep 300").await;
    read_pane_until(&mut ws, 1, "last-row").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(ws);
    d.signal(nix::sys::signal::Signal::SIGTERM);

    d.start();
    let (mut ws, _) = connect_state(&d).await;
    let snap = attach_pane(&mut ws, 1).await;
    let (drawn, last, marker) = (snap.find("drawn-below"), snap.find("last-row"), snap.find("restored"));
    assert!(drawn.is_some() && last.is_some(), "what it drew is still there: {snap:?}");
    assert!(marker > last, "the marker is below it: {snap:?}");
    d.signal(nix::sys::signal::Signal::SIGTERM);
}
