//! The daemon's local socket as a named pipe: axum serving it (a Listener
//! that checks each client's user), and the CLI's blocking client.

use std::{
    ffi::c_void,
    io::{self, Read, Write},
    os::windows::io::AsRawHandle,
    time::{Duration, Instant},
};

use axum::{Router, routing::get};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::{Foundation::HANDLE, Security::SECURITY_ATTRIBUTES};

use crate::win::{owner_only, pipe_name, same_user};

struct Sa(SECURITY_ATTRIBUTES);
// The descriptor is only read, by CreateNamedPipe.
unsafe impl Send for Sa {}
unsafe impl Sync for Sa {}

fn instance(name: &str, first: bool, sa: &mut Sa) -> io::Result<NamedPipeServer> {
    // SAFETY: `sa` is a valid SECURITY_ATTRIBUTES for the call.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, &mut sa.0 as *mut _ as *mut c_void)
    }
}

/// A named pipe as an axum listener: a new instance waits while the last
/// one serves, as a socket's backlog would.
struct PipeListener {
    name: String,
    next: NamedPipeServer,
    sa: Sa,
}

impl axum::serve::Listener for PipeListener {
    type Io = NamedPipeServer;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            if let Err(e) = self.next.connect().await {
                eprintln!("pipe: connect: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            let fresh = match instance(&self.name, false, &mut self.sa) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("pipe: new instance: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let conn = std::mem::replace(&mut self.next, fresh);
            match same_user(conn.as_raw_handle() as HANDLE) {
                Ok(true) => return (conn, ()),
                other => eprintln!("pipe: refused a client: {other:?}"),
            }
        }
    }

    fn local_addr(&self) -> io::Result<()> {
        Ok(())
    }
}

fn router() -> Router {
    Router::new()
        .route("/ping", get(|| async { "pong" }))
        // Bytes a little at a time, then done: a streamed response (logs, output).
        .route(
            "/stream",
            get(|| async {
                let s = async_stream_lite();
                axum::body::Body::from_stream(s)
            }),
        )
}

fn async_stream_lite() -> impl futures_lite::Stream<Item = Result<Vec<u8>, io::Error>> {
    futures_lite::stream::unfold(0u32, |i| async move {
        if i == 5 {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        Some((Ok(format!("chunk {i}\n").into_bytes()), i + 1))
    })
}

pub async fn serve(pipe: &str, tcp: &str) -> io::Result<()> {
    let name = pipe_name(pipe);
    let mut sa = Sa(owner_only()?);
    let first = instance(&name, true, &mut sa)?;
    let pipe = PipeListener { name, next: first, sa };
    let tcp = tokio::net::TcpListener::bind(tcp).await?;
    eprintln!("http: serving {} and {}", pipe.name, tcp.local_addr()?);
    tokio::spawn(axum::serve(tcp, router()).into_future());
    axum::serve(pipe, router()).await
}

/// One request on a fresh connection, as the CLI does it.
fn request<S: Read + Write>(mut s: S, path: &str) -> io::Result<String> {
    write!(s, "GET {path} HTTP/1.1\r\nHost: illogical\r\nConnection: close\r\n\r\n")?;
    let mut out = String::new();
    s.read_to_string(&mut out)?;
    Ok(out)
}

fn open_pipe(name: &str) -> io::Result<std::fs::File> {
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(name) {
            // All instances busy: the next one is a moment away.
            Err(e) if e.raw_os_error() == Some(231) => std::thread::sleep(Duration::from_millis(1)),
            r => return r,
        }
    }
}

pub fn bench(pipe: &str, tcp: &str) -> io::Result<()> {
    let name = pipe_name(pipe);
    for (what, n) in [("pipe", 1000), ("tcp", 1000)] {
        let mut v = vec![];
        for _ in 0..n {
            let t = Instant::now();
            let body = if what == "pipe" {
                request(open_pipe(&name)?, "/ping")?
            } else {
                request(std::net::TcpStream::connect(tcp)?, "/ping")?
            };
            assert!(body.ends_with("pong"), "{body}");
            v.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "http /ping over {what}: p50 {:.3} ms, p99 {:.3} ms ({n} requests, a connection each)",
            v[n / 2],
            v[n * 99 / 100]
        );
    }
    let t = Instant::now();
    let body = request(open_pipe(&name)?, "/stream")?;
    println!(
        "http /stream over pipe: {} chunks in {:.0} ms (chunked: {})",
        body.matches("chunk ").count(),
        t.elapsed().as_secs_f64() * 1000.0,
        body.contains("transfer-encoding: chunked")
    );
    // Duplex on a synchronous pipe handle: a read blocked in one thread
    // holds up a write from another (one file object, synchronous I/O).
    let f = open_pipe(&name)?;
    let g = f.try_clone()?;
    std::thread::spawn(move || {
        let mut buf = [0u8; 1];
        let _ = (&g).read(&mut buf);
    });
    std::thread::sleep(Duration::from_millis(200));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let t = Instant::now();
        let r = (&f).write_all(b"GET /ping HTTP/1.1\r\nHost: illogical\r\n\r\n");
        let _ = tx.send((r.is_ok(), t.elapsed()));
    });
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok((ok, d)) => println!("duplex on a sync handle: the write returned ({ok}) after {:.1} ms", d.as_secs_f64() * 1000.0),
        Err(_) => println!("duplex on a sync handle: the write is still blocked after 3 s, behind the read"),
    }
    Ok(())
}
