//! S30's native WebRTC peer: what the Linux desktop app would run on its
//! Rust side, because distro WebKitGTK has no RTCPeerConnection.
//!
//! It joins a room on the test server (server.mjs, standing in for the
//! session's daemon), answers the browser's offer, sends Opus from the
//! microphone or a test tone, plays and/or records what it receives, and
//! signs its DTLS fingerprint with an Ed25519 key the way M63 would sign
//! with the device key (and checks the browser's).
//!
//!   s30-native --ws wss://geek.<tailnet>:8443/ws?room=r1 [--mic] [--play]
//!              [--turn turn:host:3478 --user u --pass p] [--relay]
//!              [--peer-key BASE64] [--seconds N] [--wav out.wav]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use rtc::interceptor::Registry;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::{MediaEngine, MIME_TYPE_OPUS};
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::peer_connection::transport::RTCIceServer;
use rtc::rtp;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use webrtc::media_stream::track_local::static_rtp::TrackLocalStaticRTP;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState,
    RTCIceTransportPolicy, RTCPeerConnectionState,
};

const RATE: u32 = 48_000;
const FRAME: usize = 960; // 20 ms at 48 kHz
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

struct Args {
    ws: String,
    mic: bool,
    play: bool,
    turn: Option<(String, String, String)>,
    relay: bool,
    peer_key: Option<String>,
    seconds: u64,
    wav: Option<String>,
    aec: bool,
}

fn args() -> Result<Args> {
    let mut a = Args { ws: String::new(), mic: false, play: false, turn: None, relay: false, peer_key: None, seconds: 0, wav: None, aec: true };
    let mut it = std::env::args().skip(1);
    let (mut turn, mut user, mut pass) = (None, String::new(), String::new());
    while let Some(k) = it.next() {
        let mut v = || it.next().ok_or_else(|| anyhow!("{k} needs a value"));
        match k.as_str() {
            "--ws" => a.ws = v()?,
            "--mic" => a.mic = true,
            "--play" => a.play = true,
            "--turn" => turn = Some(v()?),
            "--user" => user = v()?,
            "--pass" => pass = v()?,
            "--relay" => a.relay = true,
            "--peer-key" => a.peer_key = Some(v()?),
            "--seconds" => a.seconds = v()?.parse()?,
            "--wav" => a.wav = Some(v()?),
            "--no-aec" => a.aec = false,
            _ => return Err(anyhow!("unknown argument {k}")),
        }
    }
    a.turn = turn.map(|t| (t, user, pass));
    if a.ws.is_empty() {
        return Err(anyhow!("--ws is required"));
    }
    Ok(a)
}

/// The `a=fingerprint:` lines of an SDP, in order: what the signature covers.
fn fingerprints(sdp: &str) -> String {
    sdp.lines().filter(|l| l.starts_with("a=fingerprint:")).map(|l| l.trim()).collect::<Vec<_>>().join("\n")
}

const APM_FRAME: usize = 480; // AEC3 works on 10 ms frames

/// Echo cancellation and noise suppression (AEC3, WebRTC's audio processing).
struct Apm {
    p: webrtc_audio_processing::Processor,
}

impl Apm {
    fn new() -> Result<Self> {
        use webrtc_audio_processing_config::{Config, EchoCanceller, NoiseSuppression};
        let p = webrtc_audio_processing::Processor::new(RATE).map_err(|e| anyhow!("apm: {e:?}"))?;
        p.set_config(Config {
            echo_canceller: Some(EchoCanceller::Full { stream_delay_ms: None }),
            noise_suppression: Some(NoiseSuppression::default()),
            ..Default::default()
        });
        Ok(Self { p })
    }
}

struct Counters {
    sent: AtomicU64,
    recv: AtomicU64,
    level_milli: AtomicU64,
}

struct Handler {
    gathered: mpsc::Sender<()>,
    state: mpsc::Sender<RTCPeerConnectionState>,
    counters: Arc<Counters>,
    received: Arc<StdMutex<Vec<i16>>>,
    playback: Option<Arc<StdMutex<std::collections::VecDeque<f32>>>>,
    apm: Option<Arc<Apm>>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, s: RTCIceGatheringState) {
        if s == RTCIceGatheringState::Complete {
            let _ = self.gathered.try_send(());
        }
    }
    async fn on_connection_state_change(&self, s: RTCPeerConnectionState) {
        eprintln!("connection: {s}");
        let _ = self.state.try_send(s);
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let counters = self.counters.clone();
        let received = self.received.clone();
        let playback = self.playback.clone();
        let apm = self.apm.clone();
        tokio::spawn(async move {
            let mut render: Vec<f32> = Vec::new();
            let mut dec = opus::Decoder::new(RATE, opus::Channels::Mono).expect("opus decoder");
            let mut pcm = vec![0i16; FRAME * 6];
            while let Some(evt) = track.poll().await {
                if let TrackRemoteEvent::OnRtpPacket(p) = evt {
                    counters.recv.fetch_add(1, Ordering::Relaxed);
                    let Ok(n) = dec.decode(&p.payload, &mut pcm, false) else { continue };
                    let frame = &pcm[..n];
                    let rms = (frame.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / n.max(1) as f64).sqrt() / 32768.0;
                    counters.level_milli.store((rms * 1000.0) as u64, Ordering::Relaxed);
                    received.lock().unwrap().extend_from_slice(frame);
                    // What goes to the speaker is the echo AEC3 must remove.
                    if let Some(apm) = &apm {
                        render.extend(frame.iter().map(|&s| s as f32 / 32768.0));
                        while render.len() >= APM_FRAME {
                            let mut f = vec![render.drain(..APM_FRAME).collect::<Vec<f32>>()];
                            let _ = apm.p.process_render_frame(&mut f);
                        }
                    }
                    if let Some(pb) = &playback {
                        let mut q = pb.lock().unwrap();
                        q.extend(frame.iter().map(|&s| s as f32 / 32768.0));
                        let max = RATE as usize / 5; // keep at most 200 ms queued
                        while q.len() > max {
                            q.pop_front();
                        }
                    }
                }
            }
        });
    }
}

/// Mono f32 frames at 48 kHz from the default input, resampled if needed.
fn start_mic(tx: std::sync::mpsc::Sender<Vec<f32>>) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let dev = host.default_input_device().context("no input device")?;
    let cfg = dev.default_input_config()?;
    let ch = cfg.channels() as usize;
    let rate = cfg.sample_rate() as f64;
    eprintln!("mic: {:?} {} Hz {} ch {:?}", dev.description().map(|d| d.to_string()).unwrap_or_default(), rate, ch, cfg.sample_format());
    let step = rate / RATE as f64;
    let mut pos = 0.0f64;
    let calls = Arc::new(AtomicU64::new(0));
    let c2 = calls.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        if std::env::var("S30_DEBUG").is_ok() { eprintln!("mic callbacks: {}", c2.load(Ordering::Relaxed)); }
    });
    let mut push = move |mono: Vec<f32>| {
        calls.fetch_add(1, Ordering::Relaxed);
        // Linear resampling to 48 kHz: good enough for a spike.
        let mut out = Vec::with_capacity((mono.len() as f64 / step) as usize + 1);
        while (pos as usize) + 1 < mono.len() {
            let i = pos as usize;
            let f = (pos - i as f64) as f32;
            out.push(mono[i] * (1.0 - f) + mono[i + 1] * f);
            pos += step;
        }
        pos -= (mono.len().saturating_sub(1)) as f64;
        if pos < 0.0 {
            pos = 0.0;
        }
        let _ = tx.send(out);
    };
    let err = |e| eprintln!("mic error: {e}");
    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => dev.build_input_stream(
            cfg.config(),
            move |d: &[f32], _| push(d.chunks(ch).map(|c| c.iter().sum::<f32>() / ch as f32).collect()),
            err,
            None,
        )?,
        cpal::SampleFormat::I16 => dev.build_input_stream(
            cfg.config(),
            move |d: &[i16], _| push(d.chunks(ch).map(|c| c.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / ch as f32).collect()),
            err,
            None,
        )?,
        f => return Err(anyhow!("unsupported mic format {f:?}")),
    };
    stream.play()?;
    Ok(stream)
}

fn start_speaker(q: Arc<StdMutex<std::collections::VecDeque<f32>>>) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let dev = host.default_output_device().context("no output device")?;
    let mut cfg: cpal::StreamConfig = dev.default_output_config()?.into();
    cfg.sample_rate = RATE;
    let ch = cfg.channels as usize;
    eprintln!("speaker: {:?} {} ch", dev.description().map(|d| d.to_string()).unwrap_or_default(), ch);
    let stream = dev.build_output_stream(
        cfg,
        move |out: &mut [f32], _| {
            let mut q = q.lock().unwrap();
            for c in out.chunks_mut(ch) {
                let s = q.pop_front().unwrap_or(0.0);
                c.iter_mut().for_each(|x| *x = s);
            }
        },
        |e| eprintln!("speaker error: {e}"),
        None,
    )?;
    stream.play()?;
    Ok(stream)
}

fn cpu_seconds() -> f64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let f: Vec<&str> = s.rsplit(')').next().unwrap_or("").split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (t(11) + t(12)) / 100.0
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = args()?;
    let key = SigningKey::generate(&mut rand::rngs::OsRng);
    eprintln!("my key: {}", B64.encode(key.verifying_key().as_bytes()));

    let mut me = MediaEngine::default();
    let opus_codec = RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: RATE,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: 111,
        ..Default::default()
    };
    me.register_codec(opus_codec.clone(), RtpCodecKind::Audio)?;
    let registry = register_default_interceptors(Registry::new(), &mut me)?;
    let mut cfg = RTCConfigurationBuilder::new();
    if let Some((url, user, pass)) = &a.turn {
        cfg = cfg.with_ice_servers(vec![RTCIceServer { urls: vec![url.clone()], username: user.clone(), credential: pass.clone() }]);
    }
    if a.relay {
        cfg = cfg.with_ice_transport_policy(RTCIceTransportPolicy::Relay);
    }

    let (gtx, mut grx) = mpsc::channel(1);
    let (stx, mut srx) = mpsc::channel(8);
    let counters = Arc::new(Counters { sent: 0.into(), recv: 0.into(), level_milli: 0.into() });
    let received = Arc::new(StdMutex::new(Vec::<i16>::new()));
    let playback = a.play.then(|| Arc::new(StdMutex::new(std::collections::VecDeque::new())));
    let _speaker = match &playback { Some(q) => Some(start_speaker(q.clone())?), None => None };
    let apm = if a.aec && a.mic { Some(Arc::new(Apm::new()?)) } else { None };
    eprintln!("echo cancellation: {}", if apm.is_some() { "AEC3 + noise suppression" } else { "off" });
    let handler = Arc::new(Handler { gathered: gtx, state: stx, counters: counters.clone(), received: received.clone(), playback, apm: apm.clone() });

    let pc = PeerConnectionBuilder::new()
        .with_configuration(cfg.build())
        .with_media_engine(me)
        .with_interceptor_registry(registry)
        .with_handler(handler)
        .with_udp_addrs(vec!["0.0.0.0:0".to_string()])
        .build()
        .await?;

    let ssrc: u32 = rand::random();
    let track = Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
        "s30-native".into(),
        "s30-native-audio".into(),
        "s30-native-audio".into(),
        RtpCodecKind::Audio,
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters { ssrc: Some(ssrc), ..Default::default() },
            codec: opus_codec.rtp_codec.clone(),
            ..Default::default()
        }],
    )));
    pc.add_track(track.clone() as Arc<dyn TrackLocal>).await?;

    // Signaling: wait for the browser's offer, answer once gathering is done.
    let (ws, _) = tokio_tungstenite::connect_async(&a.ws).await.context("signaling")?;
    let (mut wtx, mut wrx) = ws.split();
    eprintln!("waiting for an offer on {}", a.ws);
    let offer = loop {
        let m = wrx.next().await.context("signaling closed")??;
        let Message::Text(t) = m else { continue };
        let v: serde_json::Value = serde_json::from_str(&t)?;
        if v["type"] == "offer" {
            break v;
        }
    };
    let sdp = offer["sdp"].as_str().context("offer without sdp")?.to_string();
    // Check the browser's signature over its fingerprints.
    let their_key = offer["key"].as_str().unwrap_or("");
    let verdict = (|| -> Result<&'static str> {
        let pk = B64.decode(their_key)?;
        let vk = VerifyingKey::from_bytes(pk.as_slice().try_into()?)?;
        let sig = Signature::from_slice(&B64.decode(offer["sig"].as_str().unwrap_or(""))?)?;
        vk.verify(fingerprints(&sdp).as_bytes(), &sig).map_err(|_| anyhow!("signature does not match the offer's fingerprints"))?;
        match &a.peer_key {
            Some(k) if k != their_key => Err(anyhow!("signed by {their_key}, expected {k}")),
            Some(_) => Ok("verified, key pinned"),
            None => Ok("verified (key not pinned)"),
        }
    })();
    match &verdict {
        Ok(v) => eprintln!("peer fingerprint: {v}"),
        Err(e) => {
            eprintln!("peer fingerprint: REJECTED: {e}");
            return Err(anyhow!("refusing the call"));
        }
    }
    let t_offer = Instant::now();
    pc.set_remote_description(RTCSessionDescription::offer(sdp)?).await?;
    let answer = pc.create_answer(None).await?;
    pc.set_local_description(answer).await?;
    let _ = tokio::time::timeout(Duration::from_secs(10), grx.recv()).await;
    let local = pc.local_description().await.context("no local description")?;
    let sig = key.sign(fingerprints(&local.sdp).as_bytes());
    let msg = serde_json::json!({
        "type": "answer", "sdp": local.sdp,
        "key": B64.encode(key.verifying_key().as_bytes()),
        "sig": B64.encode(sig.to_bytes()),
    });
    wtx.send(Message::Text(msg.to_string().into())).await?;
    let cands: HashMap<&str, usize> = local.sdp.lines().filter(|l| l.starts_with("a=candidate")).fold(HashMap::new(), |mut m, l| {
        let t = l.split(" typ ").nth(1).and_then(|r| r.split_whitespace().next()).unwrap_or("?");
        *m.entry(t).or_default() += 1;
        m
    });
    eprintln!("answer sent, local candidates {cands:?}");

    loop {
        match tokio::time::timeout(Duration::from_secs(20), srx.recv()).await {
            Ok(Some(RTCPeerConnectionState::Connected)) => break,
            Ok(Some(RTCPeerConnectionState::Failed)) | Ok(None) | Err(_) => return Err(anyhow!("did not connect")),
            _ => {}
        }
    }
    eprintln!("connected {} ms after the offer", t_offer.elapsed().as_millis());

    // Audio source: the mic, or a 440 Hz tone.
    let (atx, arx) = std::sync::mpsc::channel::<Vec<f32>>();
    let _mic = if a.mic { Some(start_mic(atx)?) } else {
        std::thread::spawn(move || {
            let mut n = 0u64;
            loop {
                let f: Vec<f32> = (0..FRAME).map(|i| 0.2 * ((n + i as u64) as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin()).collect();
                n += FRAME as u64;
                if atx.send(f).is_err() { break; }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        None
    };
    let (ftx, mut frx) = mpsc::channel::<Vec<u8>>(50);
    let capture_apm = apm.clone();
    std::thread::spawn(move || {
        let mut enc = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip).expect("opus encoder");
        let mut raw: Vec<f32> = Vec::new();
        let mut buf: Vec<f32> = Vec::new();
        let mut out = vec![0u8; 1500];
        while let Ok(chunk) = arx.recv() {
            if let Some(apm) = &capture_apm {
                raw.extend(chunk);
                while raw.len() >= APM_FRAME {
                    let mut f = vec![raw.drain(..APM_FRAME).collect::<Vec<f32>>()];
                    let _ = apm.p.process_capture_frame(&mut f);
                    buf.extend(f.pop().unwrap());
                }
            } else {
                buf.extend(chunk);
            }
            while buf.len() >= FRAME {
                let frame: Vec<f32> = buf.drain(..FRAME).collect();
                if let Ok(n) = enc.encode_float(&frame, &mut out) {
                    if ftx.blocking_send(out[..n].to_vec()).is_err() { return; }
                }
            }
        }
    });
    let sender = {
        let counters = counters.clone();
        let track = track.clone();
        tokio::spawn(async move {
            let (mut seq, mut ts) = (rand::random::<u16>(), rand::random::<u32>());
            while let Some(payload) = frx.recv().await {
                let p = rtp::Packet {
                    header: rtp::header::Header { version: 2, payload_type: 111, sequence_number: seq, timestamp: ts, ssrc, ..Default::default() },
                    payload: payload.into(),
                };
                seq = seq.wrapping_add(1);
                ts = ts.wrapping_add(FRAME as u32);
                if track.write_rtp(p).await.is_ok() {
                    counters.sent.fetch_add(1, Ordering::Relaxed);
                }
            }
        })
    };

    let start = Instant::now();
    let cpu0 = cpu_seconds();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let s = start.elapsed().as_secs_f64();
                eprintln!("{:>4.0}s sent {:>5} recv {:>5} level {:.3} cpu {:.1}%", s,
                    counters.sent.load(Ordering::Relaxed), counters.recv.load(Ordering::Relaxed),
                    counters.level_milli.load(Ordering::Relaxed) as f64 / 1000.0,
                    100.0 * (cpu_seconds() - cpu0) / s.max(0.001));
                if a.seconds > 0 && s >= a.seconds as f64 { break; }
            }
            Some(st) = srx.recv() => if matches!(st, RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed | RTCPeerConnectionState::Disconnected) { break; },
            m = wrx.next() => match m { Some(Ok(Message::Text(t))) if t.contains("\"bye\"") => break, None => break, _ => {} },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    sender.abort();
    let _ = pc.close().await;
    if let Some(path) = &a.wav {
        let spec = hound::WavSpec { channels: 1, sample_rate: RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(path, spec)?;
        for s in received.lock().unwrap().iter() { w.write_sample(*s)?; }
        w.finalize()?;
        eprintln!("wrote {path}");
    }
    Ok(())
}
