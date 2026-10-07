//! `arugula attach`: use a pane from the terminal you're in. The pane is
//! zoomed to this terminal's size while you're attached (like a phone);
//! Ctrl-] detaches.

use std::{
    io::{ErrorKind, Read, Write},
    sync::mpsc,
    time::Duration,
};

use anyhow::Context;
use arugula_proto::{AttachPane, ClientMsg, Frame, FrameKind, ServerMsg};
use tungstenite::{Message, WebSocket};

use crate::{
    http::{Stream, Target},
    term::size as term_size,
    wake::Wake,
};

const DETACH: u8 = 0x1d; // Ctrl-]

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
                    t.layout.panes.iter().any(|(p, _)| *p == pane) || arugula_proto::Node::contains(&t.root, pane)
                })
                .map(|t| t.id)
                .with_context(|| format!("no pane %{pane}"))?;
        }
    };
    let mut size = term_size();
    let view =
        |s: (u16, u16)| ClientMsg::View { tab, cols: s.0, rows: s.1, zoom: Some(pane), claim: true, typed: false };
    send(&mut ws, &view(size))?;
    send(
        &mut ws,
        &ClientMsg::Attach { panes: vec![AttachPane::new(pane, None)], zstd: false, acks: false, kitty_keys: false },
    )?;

    // Raw mode for the duration; restored however we leave.
    struct Restore(#[allow(dead_code)] crate::term::Raw);
    impl Drop for Restore {
        fn drop(&mut self) {
            // Leave whatever screen and modes the pane had us in.
            let _ = std::io::stdout()
                .write_all(b"\x1b[?1049l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[0m\x1b[?25h\r\n");
            let _ = std::io::stdout().flush();
        }
    }
    let _restore = Restore(crate::term::Raw::enter()?);
    eprint!("\x1b[2m[attached to %{pane} · Ctrl-] detaches]\x1b[0m\r\n");

    // Keys, read on their own thread; an empty read is the end of input.
    let (mut wake, waker) = Wake::pair()?;
    let (keys_tx, keys) = mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new().name("attach-input".into()).spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            let n = std::io::stdin().read(&mut buf).unwrap_or(0);
            let end = n == 0;
            if keys_tx.send(buf[..n].to_vec()).is_err() || end {
                break;
            }
            waker.wake();
        }
        waker.wake();
    })?;

    ws.get_mut().set_nonblocking(true)?;
    let mut out = std::io::stdout();
    loop {
        let (sock_ready, woken) = wake.wait(Some(ws.get_ref().as_ref()), Duration::from_millis(200))?;
        if woken {
            wake.drain();
        }
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
                            eprint!("\r\n\x1b[2m[arugula: {message}]\x1b[0m\r\n");
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
        while let Ok(data) = keys.try_recv() {
            if data.is_empty() {
                return Ok(0);
            }
            let data = &data[..];
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
