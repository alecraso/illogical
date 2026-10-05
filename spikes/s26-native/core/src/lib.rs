//! S26's shared core: what every native front end needs from the daemon.
//!
//! - [`Conn`]: the daemon's WebSocket over its Unix socket, on its own
//!   thread. Output arrives as [`Event`]s; input goes out through a channel
//!   whose sender wakes the thread at once (no polling interval).
//! - [`Pane`]: a client-side libghostty-vt terminal fed the pane's snapshot
//!   and output, read by a renderer through libghostty's render state. It
//!   never answers terminal queries: the daemon's terminal already does.
//! - [`Pane::key`]: keys encoded by libghostty from physical key codes, so
//!   modes (application cursor keys, the Kitty protocol) are honoured.

use std::{
    io::{ErrorKind, Read, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    path::Path,
    sync::mpsc,
    thread,
};

use anyhow::{Context, Result};
use illogical_proto::{AttachPane, ClientMsg, Frame, FrameKind, ServerMsg};
use libghostty_vt::{
    Terminal,
    key::{self, Key, Mods},
};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use tungstenite::{Message, WebSocket};

pub use illogical_proto as proto;
pub use libghostty_vt;

#[derive(Debug)]
pub enum Event {
    /// The tab the pane is in, once the daemon said hello.
    Attached { tab: u32 },
    Snapshot(Vec<u8>),
    Output(Vec<u8>),
    /// The pane or the connection is gone.
    Closed(String),
}

enum Out {
    Msg(ClientMsg),
    Input(Vec<u8>),
}

/// The connection to one pane. Cheap to clone the sending half.
#[derive(Clone)]
pub struct Sender {
    tx: mpsc::Sender<Out>,
    wake: std::sync::Arc<UnixStream>,
    pane: u32,
    tab: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl Sender {
    fn push(&self, o: Out) {
        if self.tx.send(o).is_ok() {
            let _ = (&*self.wake).write(&[1]);
        }
    }

    pub fn input(&self, data: &[u8]) {
        if !data.is_empty() {
            self.push(Out::Input(data.to_vec()));
        }
    }

    /// The window's size in cells: the pane is zoomed to it, as `attach`
    /// does (the daemon has one size per pane).
    pub fn view(&self, cols: u16, rows: u16) {
        let tab = self.tab.load(std::sync::atomic::Ordering::SeqCst);
        if tab != 0 {
            self.push(Out::Msg(ClientMsg::View { tab: tab.into(), cols, rows, zoom: Some(self.pane.into()), claim: true }));
        }
    }
}

pub struct Conn {
    pub sender: Sender,
    pub events: mpsc::Receiver<Event>,
}

impl Conn {
    /// Attaches to `pane` on the daemon at `sock`, sized `cols`x`rows`.
    /// `notify` runs on the connection thread after each event is queued,
    /// for a UI to wake its own loop.
    pub fn attach(sock: &Path, pane: u32, cols: u16, rows: u16, notify: impl Fn() + Send + 'static) -> Result<Conn> {
        let stream = UnixStream::connect(sock).with_context(|| format!("no daemon at {}", sock.display()))?;
        let (mut ws, _) = tungstenite::client("ws://localhost/ws", stream).map_err(|e| anyhow::anyhow!("handshake: {e}"))?;
        let tab = loop {
            if let Message::Text(t) = ws.read()?
                && let Ok(ServerMsg::Hello { state, .. }) = serde_json::from_str(&t)
            {
                break state
                    .tabs
                    .iter()
                    .find(|t| t.layout.panes.iter().any(|(p, _)| *p == pane) || illogical_proto::Node::contains(&t.root, pane))
                    .map(|t| u32::from(t.id))
                    .with_context(|| format!("no pane %{pane}"))?;
            }
        };
        let send = |ws: &mut WebSocket<UnixStream>, m: &ClientMsg| -> Result<()> {
            ws.send(Message::Text(serde_json::to_string(m)?.into()))?;
            Ok(())
        };
        send(&mut ws, &ClientMsg::View { tab: tab.into(), cols, rows, zoom: Some(pane.into()), claim: true })?;
        send(&mut ws, &ClientMsg::Attach { panes: vec![AttachPane::new(pane, None)], zstd: false, acks: false, kitty_keys: false })?;
        ws.get_mut().set_nonblocking(true)?;

        let (wake_tx, mut wake_rx) = UnixStream::pair()?;
        wake_rx.set_nonblocking(true)?;
        let (tx, out_rx) = mpsc::channel::<Out>();
        let (ev_tx, events) = mpsc::channel();
        let _ = ev_tx.send(Event::Attached { tab });
        notify();
        thread::Builder::new().name("s26-conn".into()).spawn(move || {
            let why = run(ws, pane, &mut wake_rx, out_rx, &ev_tx, &notify).err().map_or("closed".into(), |e| e.to_string());
            let _ = ev_tx.send(Event::Closed(why));
            notify();
        })?;
        let sender = Sender {
            tx,
            wake: std::sync::Arc::new(wake_tx),
            pane,
            tab: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(tab)),
        };
        Ok(Conn { sender, events })
    }
}

fn run(
    mut ws: WebSocket<UnixStream>,
    pane: u32,
    wake: &mut UnixStream,
    out: mpsc::Receiver<Out>,
    ev: &mpsc::Sender<Event>,
    notify: &dyn Fn(),
) -> Result<()> {
    let mut sink = [0u8; 64];
    loop {
        {
            let mut fds = [PollFd::new(ws.get_ref().as_fd(), PollFlags::POLLIN), PollFd::new(wake.as_fd(), PollFlags::POLLIN)];
            poll(&mut fds, PollTimeout::NONE)?;
        }
        while wake.read(&mut sink).is_ok_and(|n| n > 0) {}
        while let Ok(o) = out.try_recv() {
            match o {
                Out::Msg(m) => ws.send(Message::Text(serde_json::to_string(&m)?.into()))?,
                Out::Input(data) => {
                    let f = Frame { kind: FrameKind::Input, pane: pane.into(), offset: 0, data };
                    ws.send(Message::Binary(f.encode().into()))?
                }
            }
        }
        let mut any = false;
        loop {
            match ws.read() {
                Ok(Message::Binary(b)) => {
                    if let Ok(f) = Frame::decode(&b)
                        && f.pane == pane
                    {
                        any = true;
                        let _ = ev.send(match f.kind {
                            FrameKind::Snapshot => Event::Snapshot(f.data),
                            _ => Event::Output(f.data),
                        });
                    }
                }
                Ok(Message::Text(t)) => {
                    if let Ok(ServerMsg::State { state }) = serde_json::from_str(&t)
                        && !state.panes.iter().any(|p| p.id == pane)
                    {
                        return Ok(());
                    }
                }
                Ok(Message::Close(_)) => return Ok(()),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        if any {
            notify();
        }
        match ws.flush() {
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
            r => r?,
        }
    }
}

/// The daemon's layout (sessions, tabs, panes), from its hello.
pub fn hello(sock: &Path) -> Result<proto::State> {
    let stream = UnixStream::connect(sock).with_context(|| format!("no daemon at {}", sock.display()))?;
    let (mut ws, _) = tungstenite::client("ws://localhost/ws", stream).map_err(|e| anyhow::anyhow!("handshake: {e}"))?;
    loop {
        if let Message::Text(t) = ws.read()?
            && let Ok(ServerMsg::Hello { state, .. }) = serde_json::from_str(&t)
        {
            return Ok(state);
        }
    }
}

/// A pane's terminal on this side, for drawing.
pub struct Pane {
    pub term: Terminal<'static, 'static>,
    encoder: key::Encoder<'static>,
    event: key::Event<'static>,
    buf: Vec<u8>,
}

impl Pane {
    pub fn new(cols: u16, rows: u16) -> Result<Pane> {
        let mut term = Terminal::new(cols, rows)?;
        term.set_scrollback_max_lines(Some(10_000))?;
        Ok(Pane { term, encoder: key::Encoder::new()?, event: key::Event::new()?, buf: Vec::with_capacity(64) })
    }

    pub fn feed(&mut self, e: &Event) {
        match e {
            Event::Snapshot(d) => {
                self.term.reset();
                self.term.vt_write(d);
            }
            Event::Output(d) => self.term.vt_write(d),
            _ => {}
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16, cell_w: u32, cell_h: u32) {
        let _ = self.term.resize(cols, rows, cell_w, cell_h);
    }

    /// The bytes for a key press (or release), or none. `code` is the
    /// Linux evdev code (the X11/GDK keycode minus 8); `text` is what the
    /// keyboard layout produced, without control characters.
    pub fn key(&mut self, code: u32, press: bool, mods: Mods, text: Option<&str>) -> &[u8] {
        self.key_as(evdev_key(code), press, mods, text)
    }

    /// The same, for a key already identified (macOS key codes go through
    /// [`mac_key`]).
    pub fn key_as(&mut self, k: Key, press: bool, mods: Mods, text: Option<&str>) -> &[u8] {
        self.buf.clear();
        let ucp = text.and_then(|t| t.chars().next()).filter(|c| !c.is_control()).map(|c| c.to_ascii_lowercase()).unwrap_or('\0');
        let mut consumed = Mods::empty();
        if ucp != '\0' && mods.contains(Mods::SHIFT) {
            consumed |= Mods::SHIFT;
        }
        self.event
            .set_action(if press { key::Action::Press } else { key::Action::Release })
            .set_key(k)
            .set_mods(mods)
            .set_consumed_mods(consumed)
            .set_unshifted_codepoint(ucp)
            .set_utf8(text.filter(|t| !t.chars().any(char::is_control)));
        let _ = self.encoder.set_options_from_terminal(&self.term).encode_to_vec(&self.event, &mut self.buf);
        &self.buf
    }
}

/// Linux evdev key codes (input-event-codes.h) to W3C key codes.
pub fn evdev_key(code: u32) -> Key {
    use Key::*;
    const LETTERS: [(u32, Key); 26] = [
        (30, A), (48, B), (46, C), (32, D), (18, E), (33, F), (34, G), (35, H), (23, I), (36, J), (37, K), (38, L), (50, M),
        (49, N), (24, O), (25, P), (16, Q), (19, R), (31, S), (20, T), (22, U), (47, V), (17, W), (45, X), (21, Y), (44, Z),
    ];
    if let Some((_, k)) = LETTERS.iter().find(|(c, _)| *c == code) {
        return *k;
    }
    match code {
        1 => Escape,
        2 => Digit1, 3 => Digit2, 4 => Digit3, 5 => Digit4, 6 => Digit5, 7 => Digit6, 8 => Digit7, 9 => Digit8, 10 => Digit9, 11 => Digit0,
        12 => Minus, 13 => Equal, 14 => Backspace, 15 => Tab, 26 => BracketLeft, 27 => BracketRight, 28 => Enter,
        29 => ControlLeft, 39 => Semicolon, 40 => Quote, 41 => Backquote, 42 => ShiftLeft, 43 => Backslash,
        51 => Comma, 52 => Period, 53 => Slash, 54 => ShiftRight, 56 => AltLeft, 57 => Space, 58 => CapsLock,
        59 => F1, 60 => F2, 61 => F3, 62 => F4, 63 => F5, 64 => F6, 65 => F7, 66 => F8, 67 => F9, 68 => F10, 87 => F11, 88 => F12,
        97 => ControlRight, 100 => AltRight, 102 => Home, 103 => ArrowUp, 104 => PageUp, 105 => ArrowLeft, 106 => ArrowRight,
        107 => End, 108 => ArrowDown, 109 => PageDown, 110 => Insert, 111 => Delete, 125 => MetaLeft, 126 => MetaRight,
        _ => Unidentified,
    }
}

/// macOS virtual key codes (Carbon's kVK_*) to W3C key codes.
pub fn mac_key(code: u16) -> Key {
    use Key::*;
    match code {
        0x00 => A, 0x01 => S, 0x02 => D, 0x03 => F, 0x04 => H, 0x05 => G, 0x06 => Z, 0x07 => X, 0x08 => C, 0x09 => V,
        0x0B => B, 0x0C => Q, 0x0D => W, 0x0E => E, 0x0F => R, 0x10 => Y, 0x11 => T, 0x12 => Digit1, 0x13 => Digit2,
        0x14 => Digit3, 0x15 => Digit4, 0x16 => Digit6, 0x17 => Digit5, 0x18 => Equal, 0x19 => Digit9, 0x1A => Digit7,
        0x1B => Minus, 0x1C => Digit8, 0x1D => Digit0, 0x1E => BracketRight, 0x1F => O, 0x20 => U, 0x21 => BracketLeft,
        0x22 => I, 0x23 => P, 0x24 => Enter, 0x25 => L, 0x26 => J, 0x27 => Quote, 0x28 => K, 0x29 => Semicolon,
        0x2A => Backslash, 0x2B => Comma, 0x2C => Slash, 0x2D => N, 0x2E => M, 0x2F => Period, 0x30 => Tab, 0x31 => Space,
        0x32 => Backquote, 0x33 => Backspace, 0x35 => Escape, 0x60 => F5, 0x61 => F6, 0x62 => F7, 0x63 => F3, 0x64 => F8,
        0x65 => F9, 0x67 => F11, 0x6D => F10, 0x6F => F12, 0x73 => Home, 0x74 => PageUp, 0x75 => Delete, 0x76 => F4,
        0x77 => End, 0x78 => F2, 0x79 => PageDown, 0x7A => F1, 0x7B => ArrowLeft, 0x7C => ArrowRight, 0x7D => ArrowDown,
        0x7E => ArrowUp,
        _ => Unidentified,
    }
}
