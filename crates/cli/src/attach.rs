//! `illogical attach`: use a pane from the terminal you're in. The pane is
//! zoomed to this terminal's size while you're attached (like a phone);
//! Ctrl-] detaches.

use std::{
    io::{ErrorKind, Read, Write},
    os::fd::AsFd,
};

use anyhow::Context;
use illogical_proto::{AttachPane, ClientMsg, Frame, FrameKind, ServerMsg};
use nix::{
    libc,
    poll::{PollFd, PollFlags, PollTimeout, poll},
    sys::termios::{self, SetArg},
};
use tungstenite::{Message, WebSocket};

use crate::http::{Stream, Target};

const DETACH: u8 = 0x1d; // Ctrl-]

fn term_size() -> (u16, u16) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: TIOCGWINSZ fills one winsize.
    if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
        (ws.ws_col, ws.ws_row)
    } else {
        (80, 24)
    }
}

fn send(ws: &mut WebSocket<Box<dyn Stream>>, msg: &ClientMsg) -> anyhow::Result<()> {
    ws.send(Message::Text(serde_json::to_string(msg)?.into()))?;
    Ok(())
}

pub fn run(target: &Target, pane: u32) -> anyhow::Result<i32> {
    let stream = target.connect()?;
    let (mut ws, _) = tungstenite::client(target.ws_request()?, stream).map_err(|e| match e {
        tungstenite::HandshakeError::Failure(e) => anyhow::Error::from(e).context("websocket handshake"),
        tungstenite::HandshakeError::Interrupted(_) => anyhow::anyhow!("websocket handshake interrupted"),
    })?;

    // Hello: find the pane's tab.
    let tab = loop {
        if let Message::Text(t) = ws.read()?
            && let Ok(ServerMsg::Hello { state, .. }) = serde_json::from_str(&t)
        {
            break state
                .tabs
                .iter()
                .find(|t| {
                    t.layout.panes.iter().any(|(p, _)| *p == pane) || illogical_proto::Node::contains(&t.root, pane)
                })
                .map(|t| t.id)
                .with_context(|| format!("no pane %{pane}"))?;
        }
    };
    let mut size = term_size();
    let view = |s: (u16, u16)| ClientMsg::View { tab, cols: s.0, rows: s.1, zoom: Some(pane), claim: true };
    send(&mut ws, &view(size))?;
    send(
        &mut ws,
        &ClientMsg::Attach { panes: vec![AttachPane::new(pane, None)], zstd: false, acks: false, kitty_keys: false },
    )?;

    // Raw mode for the duration; restored however we leave.
    let stdin = std::io::stdin();
    let saved = termios::tcgetattr(stdin.as_fd()).ok();
    if let Some(t) = &saved {
        let mut raw = t.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(stdin.as_fd(), SetArg::TCSANOW, &raw)?;
    }
    struct Restore(Option<termios::Termios>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(t) = &self.0 {
                let _ = termios::tcsetattr(std::io::stdin().as_fd(), SetArg::TCSANOW, t);
            }
            // Leave whatever screen and modes the pane had us in.
            let _ = std::io::stdout()
                .write_all(b"\x1b[?1049l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[0m\x1b[?25h\r\n");
            let _ = std::io::stdout().flush();
        }
    }
    let _restore = Restore(saved);
    eprint!("\x1b[2m[attached to %{pane} · Ctrl-] detaches]\x1b[0m\r\n");

    ws.get_mut().set_nonblocking(true)?;
    let mut out = std::io::stdout();
    let mut buf = [0u8; 4096];
    loop {
        let (sock_ready, stdin_ready) = {
            let mut fds =
                [PollFd::new(ws.get_ref().fd(), PollFlags::POLLIN), PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
            poll(&mut fds, PollTimeout::from(200u16))?;
            let r = |i: usize| fds[i].revents().is_some_and(|e| !e.is_empty());
            (r(0), r(1))
        };
        if sock_ready {
            loop {
                match ws.read() {
                    Ok(Message::Binary(b)) => {
                        if let Ok(f) = Frame::decode(&b)
                            && f.pane == pane
                        {
                            if f.kind == FrameKind::Snapshot {
                                out.write_all(b"\x1bc")?;
                            }
                            out.write_all(&f.data)?;
                        }
                    }
                    Ok(Message::Text(t)) => match serde_json::from_str(&t) {
                        Ok(ServerMsg::State { state }) if !state.panes.iter().any(|p| p.id == pane) => return Ok(0),
                        // Why typing went nowhere (someone else drives it,
                        // a viewer's share): say so, on a line of its own.
                        Ok(ServerMsg::Error { message, .. }) => {
                            eprint!("\r\n\x1b[2m[illogical: {message}]\x1b[0m\r\n");
                        }
                        _ => {}
                    },
                    Ok(Message::Close(_)) => return Ok(0),
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }
            out.flush()?;
        }
        if stdin_ready {
            let n = std::io::stdin().read(&mut buf)?;
            if n == 0 {
                return Ok(0);
            }
            let data = &buf[..n];
            let (data, detach) = match data.iter().position(|b| *b == DETACH) {
                Some(i) => (&data[..i], true),
                None => (data, false),
            };
            if !data.is_empty() {
                let f = Frame { kind: FrameKind::Input, pane, offset: 0, data: data.to_vec() };
                ws.send(Message::Binary(f.encode().into()))?;
            }
            if detach {
                return Ok(0);
            }
        }
        let now = term_size();
        if now != size {
            size = now;
            send(&mut ws, &view(size))?;
        }
        match ws.flush() {
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
            r => r?,
        }
    }
}
