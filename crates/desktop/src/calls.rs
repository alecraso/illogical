//! Huddles in the Linux app (M63). WebKitGTK has no WebRTC (S30), so the
//! page keeps signaling, signing and the UI, and hands each peer's
//! descriptions to these commands; the call itself runs here: the mic and
//! speaker (cpal), echo cancellation and noise suppression (AEC3), Opus,
//! and one webrtc-rs connection per member, through TURN when it has to.
//!
//! Audio: one capture thread processes the mic (AEC3 on 10 ms frames) and
//! encodes 20 ms Opus frames once; every peer sends the same frames on its
//! own track. Each peer's audio is decoded into its own queue, and the
//! speaker plays the sum, which is also what AEC3 removes from the mic.
//!
//! The page reads levels and connection states by polling
//! [`call_native_status`] (it draws speaking rings from them).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtc::interceptor::Registry;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::configuration::media_engine::{MIME_TYPE_OPUS, MediaEngine};
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::peer_connection::transport::RTCIceServer;
use rtc::rtp;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify, broadcast};
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_local::static_rtp::TrackLocalStaticRTP;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState, RTCPeerConnectionState,
};

const RATE: u32 = 48_000;
/// 20 ms: one Opus frame.
const FRAME: usize = 960;
/// 10 ms: what AEC3 works on.
const APM_FRAME: usize = 480;
/// A peer starts playing once this much is queued (60 ms), and never
/// keeps more than `MAX_QUEUE` (200 ms): a small jitter buffer.
const PRIME: usize = 3 * FRAME;
const MAX_QUEUE: usize = RATE as usize / 5;
const GATHER: Duration = Duration::from_secs(4);
const PAYLOAD_TYPE: u8 = 111;

/// An ICE server as `RTCPeerConnection` takes it.
#[derive(Debug, Clone, Deserialize)]
pub struct IceServer {
    urls: Urls,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    credential: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum Urls {
    One(String),
    Many(Vec<String>),
}

#[derive(Serialize)]
pub struct Status {
    /// This mic's level (RMS, 0–1), after echo cancellation.
    me: f32,
    peers: Vec<PeerStatus>,
}

#[derive(Serialize)]
pub struct PeerStatus {
    id: u64,
    /// As `RTCPeerConnection.connectionState` says it.
    state: String,
    level: f32,
}

/// A peer's decoded audio, waiting for the speaker.
#[derive(Default)]
struct Queue {
    samples: VecDeque<f32>,
    playing: bool,
}

type Mixer = Arc<StdMutex<HashMap<u64, Queue>>>;

struct Peer {
    pc: Arc<dyn PeerConnection>,
    state: Arc<StdMutex<String>>,
    level: Arc<AtomicU32>,
    gathered: Arc<Notify>,
    gathering_done: Arc<AtomicBool>,
    writer: tauri::async_runtime::JoinHandle<()>,
}

struct Engine {
    /// Dropping it stops the audio thread (the mic and the speaker).
    _audio: std::sync::mpsc::Sender<()>,
    packets: broadcast::Sender<Arc<Vec<u8>>>,
    mixer: Mixer,
    muted: Arc<AtomicBool>,
    me: Arc<AtomicU32>,
    peers: HashMap<u64, Arc<Peer>>,
}

static ENGINE: LazyLock<Mutex<Option<Engine>>> = LazyLock::new(|| Mutex::new(None));

/// The loudest frame since the page last asked (then back to zero), so a
/// short word between polls still lights the speaking ring.
fn level(x: &AtomicU32) -> f32 {
    f32::from_bits(x.swap(0, Ordering::Relaxed))
}

fn raise(x: &AtomicU32, v: f32) {
    let _ =
        x.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| (v > f32::from_bits(old)).then_some(v.to_bits()));
}

fn rms(s: &[f32]) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
}

/// Echo cancellation and noise suppression.
struct Apm(webrtc_audio_processing::Processor);

impl Apm {
    fn new() -> Result<Self, String> {
        use webrtc_audio_processing_config::{Config, EchoCanceller, NoiseSuppression};
        let p = webrtc_audio_processing::Processor::new(RATE).map_err(|e| format!("echo cancellation: {e:?}"))?;
        p.set_config(Config {
            echo_canceller: Some(EchoCanceller::Full { stream_delay_ms: None }),
            noise_suppression: Some(NoiseSuppression::default()),
            ..Default::default()
        });
        Ok(Self(p))
    }
}

// ---- audio

/// Start the mic and the speaker on a thread of their own (cpal's streams
/// stay on the thread that made them), until the returned sender drops.
fn start_audio(
    mixer: Mixer,
    packets: broadcast::Sender<Arc<Vec<u8>>>,
    muted: Arc<AtomicBool>,
    me: Arc<AtomicU32>,
) -> Result<std::sync::mpsc::Sender<()>, String> {
    let apm = Arc::new(Apm::new()?);
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let (mic_tx, mic_rx) = std::sync::mpsc::channel::<Vec<f32>>();
    let render_apm = apm.clone();
    std::thread::Builder::new()
        .name("huddle-audio".into())
        .spawn(move || {
            let streams = (|| -> Result<(cpal::Stream, cpal::Stream), String> {
                let host = cpal::default_host();
                Ok((mic(&host, mic_tx)?, speaker(&host, mixer, render_apm)?))
            })();
            match streams {
                Ok(s) => {
                    let _ = ready_tx.send(Ok(()));
                    // Until the call ends (the sender drops).
                    let _ = stop_rx.recv();
                    drop(s);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    ready_rx.recv().map_err(|_| "the audio thread ended".to_owned())??;
    std::thread::Builder::new()
        .name("huddle-encode".into())
        .spawn(move || encode(mic_rx, apm, packets, muted, me))
        .map_err(|e| e.to_string())?;
    Ok(stop_tx)
}

/// Mono f32 chunks at 48 kHz from the default input.
fn mic(host: &cpal::Host, tx: std::sync::mpsc::Sender<Vec<f32>>) -> Result<cpal::Stream, String> {
    let dev = host.default_input_device().ok_or("there's no microphone")?;
    let cfg = dev.default_input_config().map_err(|e| format!("the microphone: {e}"))?;
    let ch = cfg.channels() as usize;
    let step = cfg.sample_rate() as f64 / RATE as f64;
    let mut pos = 0.0f64;
    // Linear resampling: fine for voice.
    let mut push = move |mono: Vec<f32>| {
        let mut out = Vec::with_capacity((mono.len() as f64 / step) as usize + 1);
        while (pos as usize) + 1 < mono.len() {
            let i = pos as usize;
            let f = (pos - i as f64) as f32;
            out.push(mono[i] * (1.0 - f) + mono[i + 1] * f);
            pos += step;
        }
        pos = (pos - mono.len().saturating_sub(1) as f64).max(0.0);
        let _ = tx.send(out);
    };
    let err = |e| eprintln!("arugula: microphone: {e}");
    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => dev.build_input_stream(
            cfg.config(),
            move |d: &[f32], _| push(d.chunks(ch).map(|c| c.iter().sum::<f32>() / ch as f32).collect()),
            err,
            None,
        ),
        cpal::SampleFormat::I16 => dev.build_input_stream(
            cfg.config(),
            move |d: &[i16], _| {
                push(d.chunks(ch).map(|c| c.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / ch as f32).collect())
            },
            err,
            None,
        ),
        f => return Err(format!("the microphone's sample format ({f:?}) isn't supported")),
    }
    .map_err(|e| format!("the microphone: {e}"))?;
    stream.play().map_err(|e| format!("the microphone: {e}"))?;
    Ok(stream)
}

/// Plays the sum of every peer's queue, and tells AEC3 what it played.
fn speaker(host: &cpal::Host, mixer: Mixer, apm: Arc<Apm>) -> Result<cpal::Stream, String> {
    let dev = host.default_output_device().ok_or("there's no speaker")?;
    let mut cfg: cpal::StreamConfig = dev.default_output_config().map_err(|e| format!("the speaker: {e}"))?.into();
    cfg.sample_rate = RATE;
    let ch = cfg.channels as usize;
    let mut render: Vec<f32> = Vec::with_capacity(APM_FRAME * 2);
    let stream = dev
        .build_output_stream(
            cfg,
            move |out: &mut [f32], _| {
                let mut queues = mixer.lock().unwrap();
                for frame in out.chunks_mut(ch) {
                    let mut s = 0.0f32;
                    for q in queues.values_mut() {
                        if !q.playing && q.samples.len() >= PRIME {
                            q.playing = true;
                        }
                        if q.playing {
                            match q.samples.pop_front() {
                                Some(x) => s += x,
                                // Ran dry: wait for a cushion again.
                                None => q.playing = false,
                            }
                        }
                    }
                    let s = s.clamp(-1.0, 1.0);
                    frame.iter_mut().for_each(|x| *x = s);
                    render.push(s);
                }
                drop(queues);
                while render.len() >= APM_FRAME {
                    let mut f = vec![render.drain(..APM_FRAME).collect::<Vec<f32>>()];
                    let _ = apm.0.process_render_frame(&mut f);
                }
            },
            |e| eprintln!("arugula: speaker: {e}"),
            None,
        )
        .map_err(|e| format!("the speaker: {e}"))?;
    stream.play().map_err(|e| format!("the speaker: {e}"))?;
    Ok(stream)
}

/// The mic, echo-cancelled, as 20 ms Opus frames for every peer.
fn encode(
    rx: std::sync::mpsc::Receiver<Vec<f32>>,
    apm: Arc<Apm>,
    packets: broadcast::Sender<Arc<Vec<u8>>>,
    muted: Arc<AtomicBool>,
    me: Arc<AtomicU32>,
) {
    let Ok(mut enc) = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip) else {
        eprintln!("arugula: no Opus encoder");
        return;
    };
    let mut raw: Vec<f32> = Vec::new();
    let mut clean: Vec<f32> = Vec::new();
    let mut out = vec![0u8; 1500];
    // `ARUGULA_CALL_DEBUG=1`: the mic's level before and after AEC3, every second.
    let debug = std::env::var_os("ARUGULA_CALL_DEBUG").is_some();
    let (mut before, mut frames) = (0f32, 0u32);
    // Ends when the mic's stream (and its sender) goes.
    while let Ok(chunk) = rx.recv() {
        if debug {
            before = before.max(rms(&chunk));
        }
        raw.extend(chunk);
        while raw.len() >= APM_FRAME {
            let mut f = vec![raw.drain(..APM_FRAME).collect::<Vec<f32>>()];
            let _ = apm.0.process_capture_frame(&mut f);
            clean.extend(f.pop().unwrap_or_default());
        }
        while clean.len() >= FRAME {
            let mut frame: Vec<f32> = clean.drain(..FRAME).collect();
            raise(&me, rms(&frame));
            frames += 1;
            if debug && frames % 50 == 0 {
                eprintln!("arugula: huddle mic {before:.3} before AEC3, {:.3} after", rms(&frame));
                before = 0.0;
            }
            if muted.load(Ordering::Relaxed) {
                frame.iter_mut().for_each(|x| *x = 0.0);
            }
            if let Ok(n) = enc.encode_float(&frame, &mut out) {
                // Nobody listening yet is fine.
                let _ = packets.send(Arc::new(out[..n].to_vec()));
            }
        }
    }
}

// ---- peers

struct Handler {
    id: u64,
    state: Arc<StdMutex<String>>,
    level: Arc<AtomicU32>,
    gathered: Arc<Notify>,
    gathering_done: Arc<AtomicBool>,
    mixer: Mixer,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, s: RTCIceGatheringState) {
        if s == RTCIceGatheringState::Complete {
            self.gathering_done.store(true, Ordering::Relaxed);
            self.gathered.notify_waiters();
        }
    }

    async fn on_connection_state_change(&self, s: RTCPeerConnectionState) {
        *self.state.lock().unwrap() = s.to_string();
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let (id, mixer, level) = (self.id, self.mixer.clone(), self.level.clone());
        tauri::async_runtime::spawn(async move {
            let Ok(mut dec) = opus::Decoder::new(RATE, opus::Channels::Mono) else { return };
            let mut pcm = vec![0f32; FRAME * 6];
            while let Some(evt) = track.poll().await {
                let TrackRemoteEvent::OnRtpPacket(p) = evt else { continue };
                let Ok(n) = dec.decode_float(&p.payload, &mut pcm, false) else { continue };
                let frame = &pcm[..n];
                raise(&level, rms(frame));
                let mut queues = mixer.lock().unwrap();
                let Some(q) = queues.get_mut(&id) else { return };
                q.samples.extend(frame);
                while q.samples.len() > MAX_QUEUE {
                    q.samples.pop_front();
                }
            }
        });
    }
}

fn opus_codec() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: RATE,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: PAYLOAD_TYPE,
    }
}

/// The ICE servers webrtc-rs can use: STUN, and TURN over UDP.
fn ice_servers(servers: Vec<IceServer>) -> Vec<RTCIceServer> {
    servers
        .into_iter()
        .filter_map(|s| {
            let urls: Vec<String> = match s.urls {
                Urls::One(u) => vec![u],
                Urls::Many(u) => u,
            }
            .into_iter()
            .filter(|u| u.starts_with("stun:") || (u.starts_with("turn:") && !u.contains("transport=tcp")))
            .collect();
            (!urls.is_empty()).then(|| RTCIceServer {
                urls,
                username: s.username.unwrap_or_default(),
                credential: s.credential.unwrap_or_default(),
            })
        })
        .collect()
}

async fn new_peer(
    mixer: Mixer,
    packets: &broadcast::Sender<Arc<Vec<u8>>>,
    id: u64,
    ice: Vec<IceServer>,
) -> Result<Peer, String> {
    let e = |e: webrtc::error::Error| e.to_string();
    let codec = opus_codec();
    let mut me = MediaEngine::default();
    me.register_codec(codec.clone(), RtpCodecKind::Audio).map_err(|x| x.to_string())?;
    let registry = register_default_interceptors(Registry::new(), &mut me).map_err(|x| x.to_string())?;
    let cfg = RTCConfigurationBuilder::new().with_ice_servers(ice_servers(ice)).build();
    let state = Arc::new(StdMutex::new("new".to_owned()));
    let level = Arc::new(AtomicU32::new(0));
    let gathered = Arc::new(Notify::new());
    let gathering_done = Arc::new(AtomicBool::new(false));
    mixer.lock().unwrap().insert(id, Queue::default());
    let handler = Arc::new(Handler {
        id,
        state: state.clone(),
        level: level.clone(),
        gathered: gathered.clone(),
        gathering_done: gathering_done.clone(),
        mixer: mixer.clone(),
    });
    let pc: Arc<dyn PeerConnection> = Arc::new(
        PeerConnectionBuilder::new()
            .with_configuration(cfg)
            .with_media_engine(me)
            .with_interceptor_registry(registry)
            .with_handler(handler)
            .with_udp_addrs(vec!["0.0.0.0:0".to_owned()])
            .build()
            .await
            .map_err(e)?,
    );
    let ssrc: u32 = rand_u32();
    let track = Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
        format!("arugula-{id}"),
        "arugula-audio".into(),
        "arugula-audio".into(),
        RtpCodecKind::Audio,
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters { ssrc: Some(ssrc), ..Default::default() },
            codec: codec.rtp_codec.clone(),
            ..Default::default()
        }],
    )));
    pc.add_track(track.clone() as Arc<dyn TrackLocal>).await.map_err(e)?;
    let mut rx = packets.subscribe();
    let writer = tauri::async_runtime::spawn(async move {
        let (mut seq, mut ts) = (rand_u32() as u16, rand_u32());
        loop {
            let payload = match rx.recv().await {
                Ok(p) => p,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            };
            let p = rtp::Packet {
                header: rtp::header::Header {
                    version: 2,
                    payload_type: PAYLOAD_TYPE,
                    sequence_number: seq,
                    timestamp: ts,
                    ssrc,
                    ..Default::default()
                },
                payload: payload.as_slice().to_vec().into(),
            };
            seq = seq.wrapping_add(1);
            ts = ts.wrapping_add(FRAME as u32);
            let _ = track.write_rtp(p).await;
        }
    });
    Ok(Peer { pc, state, level, gathered, gathering_done, writer })
}

/// Set our description and give it back once ICE has gathered (the page
/// sends descriptions whole).
async fn describe(peer: &Peer, desc: RTCSessionDescription) -> Result<String, String> {
    // Registered before setting, so a quick gather isn't missed.
    let gathered = peer.gathered.notified();
    peer.pc.set_local_description(desc).await.map_err(|e| e.to_string())?;
    if !peer.gathering_done.load(Ordering::Relaxed) {
        let _ = tokio::time::timeout(GATHER, gathered).await;
    }
    Ok(peer.pc.local_description().await.ok_or("no local description")?.sdp)
}

fn rand_u32() -> u32 {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).expect("the OS's random source");
    u32::from_ne_bytes(b)
}

async fn close(peer: Arc<Peer>) {
    peer.writer.abort();
    let _ = peer.pc.close().await;
}

// ---- what the page calls

/// Open the mic and the speaker for a huddle.
#[tauri::command]
pub async fn call_native_start() -> Result<(), String> {
    let mut g = ENGINE.lock().await;
    if g.is_some() {
        return Ok(());
    }
    let mixer: Mixer = Default::default();
    let (packets, _) = broadcast::channel(16);
    let muted = Arc::new(AtomicBool::new(false));
    let me = Arc::new(AtomicU32::new(0));
    let audio = start_audio(mixer.clone(), packets.clone(), muted.clone(), me.clone())?;
    *g = Some(Engine { _audio: audio, packets, mixer, muted, me, peers: HashMap::new() });
    Ok(())
}

/// A connection to huddle member `id`. With `offer`, our offer, to send.
#[tauri::command]
pub async fn call_native_peer(id: u64, ice: Vec<IceServer>, offer: bool) -> Result<Option<String>, String> {
    let (mixer, packets, old) = {
        let mut g = ENGINE.lock().await;
        let engine = g.as_mut().ok_or("the huddle hasn't started")?;
        (engine.mixer.clone(), engine.packets.clone(), engine.peers.remove(&id))
    };
    if let Some(old) = old {
        close(old).await;
    }
    let peer = Arc::new(new_peer(mixer, &packets, id, ice).await?);
    {
        let mut g = ENGINE.lock().await;
        let Some(engine) = g.as_mut() else {
            close(peer).await;
            return Err("the huddle ended".into());
        };
        engine.peers.insert(id, peer.clone());
    }
    if !offer {
        return Ok(None);
    }
    let o = peer.pc.create_offer(None).await.map_err(|e| e.to_string())?;
    Ok(Some(describe(&peer, o).await?))
}

async fn peer(id: u64) -> Result<Arc<Peer>, String> {
    let g = ENGINE.lock().await;
    let engine = g.as_ref().ok_or("the huddle hasn't started")?;
    engine.peers.get(&id).cloned().ok_or_else(|| "no such peer".to_owned())
}

/// Their description. For an offer, our answer, to send.
#[tauri::command]
pub async fn call_native_remote(id: u64, kind: String, sdp: String) -> Result<Option<String>, String> {
    let peer = peer(id).await?;
    let e = |e: webrtc::error::Error| e.to_string();
    match kind.as_str() {
        "offer" => {
            peer.pc.set_remote_description(RTCSessionDescription::offer(sdp).map_err(e)?).await.map_err(e)?;
            let a = peer.pc.create_answer(None).await.map_err(e)?;
            Ok(Some(describe(&peer, a).await?))
        }
        "answer" => {
            peer.pc.set_remote_description(RTCSessionDescription::answer(sdp).map_err(e)?).await.map_err(e)?;
            Ok(None)
        }
        _ => Err(format!("not a description: {kind}")),
    }
}

#[tauri::command]
pub async fn call_native_drop(id: u64) {
    let p = {
        let mut g = ENGINE.lock().await;
        let Some(engine) = g.as_mut() else { return };
        engine.mixer.lock().unwrap().remove(&id);
        engine.peers.remove(&id)
    };
    if let Some(p) = p {
        close(p).await;
    }
}

#[tauri::command]
pub async fn call_native_mute(muted: bool) {
    if let Some(e) = ENGINE.lock().await.as_ref() {
        e.muted.store(muted, Ordering::Relaxed);
    }
}

/// End the huddle here: every connection, the mic and the speaker.
#[tauri::command]
pub async fn call_native_stop() {
    let Some(engine) = ENGINE.lock().await.take() else { return };
    for p in engine.peers.into_values() {
        close(p).await;
    }
}

#[tauri::command]
pub async fn call_native_status() -> Status {
    let g = ENGINE.lock().await;
    let Some(e) = g.as_ref() else { return Status { me: 0.0, peers: vec![] } };
    Status {
        me: if e.muted.load(Ordering::Relaxed) { 0.0 } else { level(&e.me) },
        peers: e
            .peers
            .iter()
            .map(|(id, p)| PeerStatus { id: *id, state: p.state.lock().unwrap().clone(), level: level(&p.level) })
            .collect(),
    }
}
