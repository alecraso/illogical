//! S15: end-to-end encrypted channels through a relay.
//!
//! - `keygen`: a Noise static key pair (hex).
//! - `bench`: in-process costs: handshakes, a 1 MB burst, typing, and
//!   fan-out to 2/5/20 viewers (pairwise channels vs one session key).
//! - `relay --listen ADDR`: the control-plane relay. A daemon dials
//!   `/dial/<name>` and keeps one WebSocket (M4c's mux) open; a client
//!   connects to `/c/<name>` and the relay splices it onto a new stream
//!   over that socket. It forwards opaque Noise messages only.
//! - `daemon --key F (--relay URL | --listen ADDR)`: a stand-in daemon that
//!   answers Noise IK and serves echo (typing) and burst (output) requests,
//!   reached through the relay or directly.
//! - `client --url URL --peer HEX`: measures handshake, typing round trip
//!   and a 1 MB burst; `--streams N` holds N channels open at once.
//!
//! Framing: over WebSocket, one binary message is one Noise message. Over a
//! mux stream, each Noise message is `len (u32 BE) | bytes`.

#[allow(dead_code)] // copied whole from the daemon
mod mux;
mod relay;

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_tungstenite::tungstenite::Message;

const PARAMS: &str = "Noise_IK_25519_AESGCM_SHA256";
const TAG: usize = 16;
/// Largest plaintext per Noise message we send.
const CHUNK: usize = 16 * 1024;
const MAX_MSG: usize = 65535;

fn builder() -> snow::Builder<'static> {
    snow::Builder::new(PARAMS.parse().unwrap())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let rt = tokio::runtime::Runtime::new()?;
    match args.first().map(String::as_str) {
        Some("keygen") => {
            let kp = builder().generate_keypair()?;
            println!("private {}\npublic {}", hex::encode(kp.private), hex::encode(kp.public));
            Ok(())
        }
        Some("bench") => bench(),
        Some("relay") => rt.block_on(relay::run(flag(&args, "--listen").context("--listen")?)),
        Some("daemon") => rt.block_on(daemon(&args)),
        Some("client") => rt.block_on(client(&args)),
        _ => bail!("usage: s15 keygen|bench|relay|daemon|client"),
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn read_key(path: &str) -> Result<Vec<u8>> {
    let s = std::fs::read_to_string(path)?;
    let line = s.lines().find(|l| l.starts_with("private ")).context("no private line")?;
    Ok(hex::decode(line.trim_start_matches("private ").trim())?)
}

// ------------------------------------------------------------- bench

fn pair() -> (snow::TransportState, snow::TransportState) {
    let s = builder().generate_keypair().unwrap();
    let c = builder().generate_keypair().unwrap();
    let mut i = builder()
        .local_private_key(&c.private)
        .unwrap()
        .remote_public_key(&s.public)
        .unwrap()
        .build_initiator()
        .unwrap();
    let mut r = builder().local_private_key(&s.private).unwrap().build_responder().unwrap();
    let mut a = [0u8; 256];
    let mut b = [0u8; 256];
    let n = i.write_message(&[], &mut a).unwrap();
    r.read_message(&a[..n], &mut b).unwrap();
    let n = r.write_message(&[], &mut a).unwrap();
    i.read_message(&a[..n], &mut b).unwrap();
    (i.into_transport_mode().unwrap(), r.into_transport_mode().unwrap())
}

fn bench() -> Result<()> {
    // Handshakes.
    let n = 2000;
    let t = Instant::now();
    for _ in 0..n {
        pair();
    }
    let per = t.elapsed() / n;
    println!("handshake (IK, both sides, incl. 2 keygens): {per:?}");
    let s = builder().generate_keypair()?;
    let c = builder().generate_keypair()?;
    let mut m1 = [0u8; 256];
    let mut i = builder().local_private_key(&c.private)?.remote_public_key(&s.public)?.build_initiator()?;
    let len1 = i.write_message(&[], &mut m1)?;
    let mut r = builder().local_private_key(&s.private)?.build_responder()?;
    let mut tmp = [0u8; 256];
    r.read_message(&m1[..len1], &mut tmp)?;
    let len2 = r.write_message(&[], &mut m1)?;
    println!("handshake bytes: msg1 {len1}, msg2 {len2} (+ framing)");

    // A 1 MB burst in 16 KB messages.
    let (mut tx, mut rx) = pair();
    let data = vec![b'x'; 1 << 20];
    let mut wire = 0usize;
    let mut buf = vec![0u8; MAX_MSG];
    let mut out = vec![0u8; MAX_MSG];
    let t = Instant::now();
    let reps = 50;
    for _ in 0..reps {
        for chunk in data.chunks(CHUNK) {
            let n = tx.write_message(chunk, &mut buf)?;
            wire += n;
            rx.read_message(&buf[..n], &mut out)?;
        }
    }
    let el = t.elapsed() / reps;
    println!(
        "1 MB burst: encrypt+decrypt {el:?} ({:.0} MB/s), wire {} bytes ({:.3}% overhead)",
        1.0 / el.as_secs_f64(),
        wire / reps as usize,
        (wire / reps as usize - data.len()) as f64 * 100.0 / data.len() as f64
    );

    // Typing: one keystroke per message.
    let t = Instant::now();
    for _ in 0..100_000 {
        let n = tx.write_message(b"a", &mut buf)?;
        rx.read_message(&buf[..n], &mut out)?;
    }
    println!("keystroke: {:?} per message, 1 byte -> {} bytes", t.elapsed() / 100_000, 1 + TAG);

    // Fan-out of 10 MB of output (4 KB writes) to V viewers.
    let output = vec![b'y'; 10 << 20];
    for v in [2usize, 5, 20] {
        let mut pairs: Vec<_> = (0..v).map(|_| pair()).collect();
        let t = Instant::now();
        let mut up = 0usize;
        for chunk in output.chunks(4096) {
            for (tx, _) in pairs.iter_mut() {
                up += tx.write_message(chunk, &mut buf)?;
            }
        }
        let pw = t.elapsed();
        // One session key: encrypt once; the relay fans out.
        let (mut stx, _) = pair();
        let t = Instant::now();
        let mut sup = 0usize;
        for chunk in output.chunks(4096) {
            sup += stx.write_message(chunk, &mut buf)?;
        }
        let sk = t.elapsed();
        println!(
            "fan-out 10 MB to {v:>2} viewers: pairwise {pw:?} cpu, {:.1} MB up | session key {sk:?} cpu, {:.1} MB up (relay copies)",
            up as f64 / 1e6,
            sup as f64 / 1e6
        );
    }
    Ok(())
}

// ------------------------------------------------------------- framing

pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let n = u32::from_be_bytes(len) as usize;
    if n > MAX_MSG {
        bail!("frame too large");
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, b: &[u8]) -> Result<()> {
    let mut f = Vec::with_capacity(4 + b.len());
    f.extend_from_slice(&(b.len() as u32).to_be_bytes());
    f.extend_from_slice(b);
    w.write_all(&f).await?;
    Ok(())
}

/// Splice a WebSocket onto a byte stream: each binary message becomes one
/// length-prefixed frame and back. Returns bytes moved (up, down).
async fn splice<S>(ws: tokio_tungstenite::WebSocketStream<S>, stream: DuplexStream) -> (u64, u64)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut wtx, mut wrx) = ws.split();
    let (mut rd, mut wr) = tokio::io::split(stream);
    let up = async {
        let mut n = 0u64;
        while let Some(Ok(m)) = wrx.next().await {
            if let Message::Binary(b) = m {
                n += b.len() as u64;
                if write_frame(&mut wr, &b).await.is_err() {
                    break;
                }
            } else if let Message::Close(_) = m {
                break;
            }
        }
        let _ = wr.shutdown().await;
        n
    };
    let down = async {
        let mut n = 0u64;
        while let Ok(Some(b)) = read_frame(&mut rd).await {
            n += b.len() as u64;
            if wtx.send(Message::Binary(b.into())).await.is_err() {
                break;
            }
        }
        let _ = wtx.close().await;
        n
    };
    tokio::join!(up, down)
}

// ------------------------------------------------------------- daemon

async fn daemon(args: &[String]) -> Result<()> {
    let key = Arc::new(read_key(&flag(args, "--key").context("--key")?)?);
    let (accept, mut streams) = mpsc::unbounded_channel::<DuplexStream>();
    if let Some(url) = flag(args, "--relay") {
        let (m, mut out) = mux::Mux::new(Some(accept.clone()));
        // As the real daemon's dial.rs does: TCP_NODELAY on the dial-out.
        let (ws, _) = tokio_tungstenite::connect_async_with_config(&url, None, true).await?;
        eprintln!("dialled {url}");
        let (mut tx, mut rx) = ws.split();
        tokio::spawn(async move {
            while let Some(f) = out.recv().await {
                if tx.send(Message::Binary(f.into())).await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            while let Some(Ok(msg)) = rx.next().await {
                if let Message::Binary(b) = msg
                    && m.handle(&b).is_err()
                {
                    break;
                }
            }
            eprintln!("relay connection closed");
            std::process::exit(1);
        });
    }
    if let Some(listen) = flag(args, "--listen") {
        // The direct path: a client's WebSocket, spliced onto the same
        // handler through an in-process pipe.
        let l = TcpListener::bind(&listen).await?;
        eprintln!("direct on {listen}");
        let accept = accept.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = l.accept().await {
                let _ = sock.set_nodelay(true);
                let accept = accept.clone();
                tokio::spawn(async move {
                    if let Ok(ws) = tokio_tungstenite::accept_async(sock).await {
                        let (a, b) = tokio::io::duplex(256 * 1024);
                        let _ = accept.send(b);
                        splice(ws, a).await;
                    }
                });
            }
        });
    }
    while let Some(s) = streams.recv().await {
        let key = key.clone();
        tokio::spawn(async move {
            if let Err(e) = serve(&key, s).await {
                eprintln!("stream: {e:#}");
            }
        });
    }
    Ok(())
}

/// One client channel: IK responder, then requests.
/// `e<bytes>`: echo. `b<u32 n>`: n bytes of output, then `d`.
async fn serve(key: &[u8], s: DuplexStream) -> Result<()> {
    let (mut rd, mut wr) = tokio::io::split(s);
    let mut hs = builder().local_private_key(key)?.build_responder()?;
    let mut buf = vec![0u8; MAX_MSG];
    let m1 = read_frame(&mut rd).await?.context("eof in handshake")?;
    let n = hs.read_message(&m1, &mut buf)?;
    let hello = &buf[..n];
    let who = hs.get_remote_static().map(hex::encode).unwrap_or_default();
    eprintln!("channel from {}… ({} byte hello)", &who[..16], hello.len());
    let n = hs.write_message(b"hello", &mut buf)?;
    write_frame(&mut wr, &buf[..n]).await?;
    let mut t = hs.into_transport_mode()?;
    let mut out = vec![0u8; MAX_MSG];
    while let Some(m) = read_frame(&mut rd).await? {
        let n = t.read_message(&m, &mut buf)?;
        match buf.first() {
            Some(b'e') => {
                let n = t.write_message(&buf[..n], &mut out)?;
                write_frame(&mut wr, &out[..n]).await?;
            }
            Some(b'b') => {
                let total = u32::from_be_bytes(buf[1..5].try_into()?) as usize;
                let chunk = vec![b'o'; CHUNK];
                let mut sent = 0;
                while sent < total {
                    let k = CHUNK.min(total - sent);
                    let n = t.write_message(&chunk[..k], &mut out)?;
                    write_frame(&mut wr, &out[..n]).await?;
                    sent += k;
                }
                let n = t.write_message(b"d", &mut out)?;
                write_frame(&mut wr, &out[..n]).await?;
            }
            _ => bail!("unknown request"),
        }
    }
    Ok(())
}

// ------------------------------------------------------------- client

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

struct Chan {
    ws: Ws,
    t: snow::TransportState,
    buf: Vec<u8>,
}

impl Chan {
    async fn open(url: &str, peer: &[u8], key: &[u8]) -> Result<(Self, Duration, Duration)> {
        let t0 = Instant::now();
        let (ws, _) = tokio_tungstenite::connect_async_with_config(url, None, true).await?;
        let connect = t0.elapsed();
        let mut ws = ws;
        let mut hs = builder().local_private_key(key)?.remote_public_key(peer)?.build_initiator()?;
        let mut buf = vec![0u8; MAX_MSG];
        let t1 = Instant::now();
        let n = hs.write_message(b"attach", &mut buf)?;
        ws.send(Message::Binary(buf[..n].to_vec().into())).await?;
        let m2 = next_bin(&mut ws).await?;
        hs.read_message(&m2, &mut buf)?;
        let hs_time = t1.elapsed();
        Ok((Self { ws, t: hs.into_transport_mode()?, buf }, connect, hs_time))
    }

    async fn send(&mut self, p: &[u8]) -> Result<()> {
        let n = self.t.write_message(p, &mut self.buf)?;
        self.ws.send(Message::Binary(self.buf[..n].to_vec().into())).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        let m = next_bin(&mut self.ws).await?;
        let n = self.t.read_message(&m, &mut self.buf)?;
        Ok(self.buf[..n].to_vec())
    }
}

async fn next_bin(ws: &mut Ws) -> Result<Vec<u8>> {
    loop {
        match ws.next().await.context("closed")?? {
            Message::Binary(b) => return Ok(b.to_vec()),
            Message::Close(_) => bail!("closed"),
            _ => {}
        }
    }
}

fn pct(v: &mut [Duration], p: f64) -> Duration {
    v.sort();
    v[((v.len() as f64 - 1.0) * p) as usize]
}

async fn client(args: &[String]) -> Result<()> {
    let url = flag(args, "--url").context("--url")?;
    let peer = hex::decode(flag(args, "--peer").context("--peer")?)?;
    let key = builder().generate_keypair()?.private;
    let streams: usize = flag(args, "--streams").map(|s| s.parse()).transpose()?.unwrap_or(0);

    if streams > 0 {
        // N channels held open, each typing once a second.
        let rtts = Arc::new(Mutex::new(Vec::new()));
        let t = Instant::now();
        let mut tasks = Vec::new();
        for _ in 0..streams {
            let (url, peer, key, rtts) = (url.clone(), peer.clone(), key.clone(), rtts.clone());
            tasks.push(tokio::spawn(async move {
                let (mut c, _, _) = Chan::open(&url, &peer, &key).await?;
                for _ in 0..10 {
                    let t = Instant::now();
                    c.send(b"ea").await?;
                    c.recv().await?;
                    rtts.lock().unwrap().push(t.elapsed());
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                anyhow::Ok(())
            }));
        }
        let mut failed = 0;
        for t in tasks {
            if !matches!(t.await, Ok(Ok(()))) {
                failed += 1;
            }
        }
        let mut r = rtts.lock().unwrap().clone();
        println!(
            "{streams} channels: {failed} failed, {} round trips, p50 {:?} p99 {:?}, wall {:?}",
            r.len(),
            pct(&mut r, 0.5),
            pct(&mut r, 0.99),
            t.elapsed()
        );
        return Ok(());
    }

    let (mut c, connect, hs) = Chan::open(&url, &peer, &key).await?;
    println!("connect {connect:?}, Noise handshake {hs:?}");
    let mut r = Vec::new();
    for _ in 0..200 {
        let t = Instant::now();
        c.send(b"ea").await?;
        c.recv().await?;
        r.push(t.elapsed());
    }
    println!("keystroke round trip: p50 {:?} p90 {:?} p99 {:?}", pct(&mut r, 0.5), pct(&mut r, 0.9), pct(&mut r, 0.99));
    let mut req = b"b".to_vec();
    req.extend_from_slice(&(1u32 << 20).to_be_bytes());
    let t = Instant::now();
    c.send(&req).await?;
    let mut got = 0;
    loop {
        let m = c.recv().await?;
        if m == b"d" {
            break;
        }
        got += m.len();
    }
    let el = t.elapsed();
    println!("1 MB burst: {got} bytes in {el:?} ({:.1} MB/s)", got as f64 / el.as_secs_f64() / 1e6);
    Ok(())
}
