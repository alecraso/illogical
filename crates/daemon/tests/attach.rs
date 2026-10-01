//! End to end against the real binary: attach, input, detach, resume.

use std::{
    net::TcpListener,
    process::{Child, Command, Stdio},
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use illogical_proto::{AttachPane, ClientMsg, Edge, Frame, FrameKind, Intent, ServerMsg, State};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Daemon {
    child: Child,
    port: u16,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[expect(clippy::zombie_processes, reason = "Daemon's Drop kills and waits")]
async fn start() -> Daemon {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let child = Command::new(env!("CARGO_BIN_EXE_illogicald"))
        .args([
            "--listen",
            &format!("127.0.0.1:{port}"),
            "--shell",
            "bash --norc --noprofile",
        ])
        .env("PS1", "$ ")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return Daemon { child, port };
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("daemon did not start");
}

#[derive(Debug)]
enum In {
    Msg(ServerMsg),
    Frame(Frame),
}

async fn recv(ws: &mut Ws) -> In {
    loop {
        let msg = timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
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
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/ws", d.port))
        .await
        .unwrap();
    let In::Msg(ServerMsg::Hello { state, .. }) = recv(&mut ws).await else {
        panic!("expected hello")
    };
    assert!(!state.panes.is_empty(), "the daemon starts with a session");
    (ws, state)
}

async fn send(ws: &mut Ws, msg: ClientMsg) {
    ws.send(Message::Text(serde_json::to_string(&msg).unwrap().into()))
        .await
        .unwrap();
}

/// Skip messages until the next layout state.
async fn next_state(ws: &mut Ws) -> State {
    loop {
        if let In::Msg(ServerMsg::State { state }) = recv(ws).await {
            return state;
        }
    }
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
    let m = ClientMsg::Attach {
        panes: vec![AttachPane { pane: 1, offset }],
    };
    ws.send(Message::Text(serde_json::to_string(&m).unwrap().into()))
        .await
        .unwrap();
}

async fn type_line(ws: &mut Ws, line: &str) {
    type_in(ws, 1, line).await;
}

async fn type_in(ws: &mut Ws, pane: u32, line: &str) {
    let f = Frame {
        kind: FrameKind::Input,
        pane,
        offset: 0,
        data: format!("{line}\r").into_bytes(),
    };
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

#[tokio::test]
async fn fresh_attach_gets_size_then_snapshot() {
    let d = start().await;
    let (mut ws, _) = connect(&d).await;
    attach(&mut ws, None).await;
    assert!(matches!(
        recv(&mut ws).await,
        In::Msg(ServerMsg::Size {
            pane: 1,
            cols: 80,
            rows: 24,
            ..
        })
    ));
    let In::Frame(f) = recv(&mut ws).await else {
        panic!("expected snapshot")
    };
    assert_eq!(f.kind, FrameKind::Snapshot);
}

#[tokio::test]
async fn reconnect_resumes_from_offset_without_gaps() {
    let d = start().await;
    let (mut ws, epoch) = connect(&d).await;
    attach(&mut ws, None).await;
    let _size = recv(&mut ws).await;
    let In::Frame(snap) = recv(&mut ws).await else {
        panic!()
    };
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
    let _size = recv(&mut ws).await;
    let In::Frame(f) = recv(&mut ws).await else {
        panic!()
    };
    assert_eq!(f.kind, FrameKind::Snapshot);
}

#[tokio::test]
async fn snapshot_shows_a_full_screen_app() {
    let d = start().await;
    let (mut ws, _) = connect(&d).await;
    attach(&mut ws, None).await;
    let _ = (recv(&mut ws).await, recv(&mut ws).await);
    // A tiny full-screen "app": alt screen, draw, wait.
    type_line(
        &mut ws,
        r"printf '\e[?1049h\e[H\e[2JFULLSCREEN-%s' $((6*7)); sleep 30",
    )
    .await;
    read_until(&mut ws, None, "FULLSCREEN-42").await;
    drop(ws);

    let (mut ws, _) = connect(&d).await;
    attach(&mut ws, None).await;
    let _size = recv(&mut ws).await;
    let In::Frame(f) = recv(&mut ws).await else {
        panic!()
    };
    let text = String::from_utf8_lossy(&f.data);
    assert!(
        text.contains("\x1b[?1049h"),
        "snapshot enters the alt screen"
    );
    assert!(
        text.contains("FULLSCREEN-42"),
        "snapshot has the app's screen"
    );
}

#[tokio::test]
async fn rejects_foreign_host_and_origin() {
    let d = start().await;
    let mut req = format!("ws://127.0.0.1:{}/ws", d.port)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    assert!(
        connect_async(req).await.is_err(),
        "foreign origin must be refused"
    );

    let mut req = format!("ws://127.0.0.1:{}/ws", d.port)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("host", "evil.example".parse().unwrap());
    assert!(
        connect_async(req).await.is_err(),
        "foreign host must be refused"
    );

    let mut req = format!("ws://127.0.0.1:{}/ws", d.port)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("tailscale-user-login", "someone@else".parse().unwrap());
    assert!(
        connect_async(req).await.is_err(),
        "tailnet user without --owner must be refused"
    );
}

#[tokio::test]
async fn slow_client_is_resynced() {
    let d = start().await;
    let (mut ws, _) = connect(&d).await;
    attach(&mut ws, None).await;
    let _ = (recv(&mut ws).await, recv(&mut ws).await);
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
            intent: Intent::Split {
                pane: 1,
                edge: Edge::Right,
            },
        },
    )
    .await;
    let state = next_state(&mut ws).await;
    let ids: Vec<u32> = state.panes.iter().map(|p| p.id).collect();
    assert_eq!(ids, vec![1, 2]);
    let tab = &state.tabs[0];
    assert_eq!(tab.layout.panes.len(), 2);
    assert_eq!(
        (tab.layout.panes[0].1.cols, tab.layout.panes[1].1.cols),
        (40, 39)
    );

    // The new pane runs a shell of its own.
    send(
        &mut ws,
        ClientMsg::Attach {
            panes: vec![AttachPane {
                pane: 2,
                offset: None,
            }],
        },
    )
    .await;
    type_in(&mut ws, 2, "echo in-pane-$((1+1)); exit").await;
    let state = until(&mut ws, |m| match m {
        In::Msg(ServerMsg::State { state }) if state.panes.len() == 1 => Some(state.clone()),
        _ => None,
    })
    .await;
    assert_eq!(state.panes[0].id, 1);
    assert_eq!(
        state.tabs[0].layout.panes.len(),
        1,
        "exiting closed the split"
    );
}

#[tokio::test]
async fn the_tab_takes_the_claiming_clients_size() {
    let d = start().await;
    let (mut a, state) = connect_state(&d).await;
    let tab = state.tabs[0].id;
    send(
        &mut a,
        ClientMsg::Attach {
            panes: vec![AttachPane {
                pane: 1,
                offset: None,
            }],
        },
    )
    .await;
    send(
        &mut a,
        ClientMsg::View {
            tab,
            cols: 101,
            rows: 30,
            zoom: None,
            claim: true,
        },
    )
    .await;
    until(&mut a, |m| {
        matches!(
            m,
            In::Msg(ServerMsg::Size {
                pane: 1,
                cols: 101,
                rows: 30
            })
        )
        .then_some(())
    })
    .await;
    type_line(&mut a, "stty size").await;
    read_until(&mut a, None, "30 101").await;

    // Another client's unclaimed view doesn't take over; a claimed one does.
    let (mut b, _) = connect_state(&d).await;
    send(
        &mut b,
        ClientMsg::View {
            tab,
            cols: 60,
            rows: 20,
            zoom: None,
            claim: false,
        },
    )
    .await;
    send(
        &mut b,
        ClientMsg::View {
            tab,
            cols: 61,
            rows: 20,
            zoom: None,
            claim: true,
        },
    )
    .await;
    let state = next_state(&mut a).await;
    assert_eq!(
        (state.tabs[0].cols, state.tabs[0].rows),
        (61, 20),
        "B's claim, not its plain view"
    );
    assert_ne!(state.tabs[0].owner, None);
}

#[tokio::test]
async fn bad_intents_report_errors_and_others_see_changes() {
    let d = start().await;
    let (mut a, _) = connect_state(&d).await;
    let (mut b, _) = connect_state(&d).await;
    send(
        &mut a,
        ClientMsg::Intent {
            id: Some(9),
            intent: Intent::ClosePane { pane: 999 },
        },
    )
    .await;
    let err = until(&mut a, |m| match m {
        In::Msg(ServerMsg::Error { id, message }) => Some((*id, message.clone())),
        _ => None,
    })
    .await;
    assert_eq!(err, (Some(9), "no pane %999".to_string()));

    send(
        &mut a,
        ClientMsg::Intent {
            id: None,
            intent: Intent::NewTab {
                session: 1,
                from_pane: Some(1),
            },
        },
    )
    .await;
    let state = next_state(&mut b).await;
    assert_eq!(state.sessions[0].tabs.len(), 2);
}
