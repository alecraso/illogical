// Synthetic echo test for AEC3 (webrtc-audio-processing, bundled): a far
// end of noise bursts plays through a simulated room (60 ms delay, 0.4 gain,
// a short tail); the mic hears that echo, and from 7 s a near-end talker
// too. Prints how much echo is removed (ERLE) and how much of the near
// talker survives double talk.
use rand::Rng;
use webrtc_audio_processing::Processor;
use webrtc_audio_processing_config::{Config, EchoCanceller, NoiseSuppression};

fn db(p: f64) -> f64 { 10.0 * p.max(1e-12).log10() }
fn main() {
    let rate = 48_000usize;
    let ap = Processor::new(rate as u32).unwrap();
    ap.set_config(Config {
        echo_canceller: Some(EchoCanceller::Full { stream_delay_ms: None }),
        noise_suppression: if std::env::args().any(|a| a == "--ns") { Some(NoiseSuppression::default()) } else { None },
        ..Default::default()
    });
    let n = ap.num_samples_per_frame();
    let secs = 10;
    let mut rng = rand::thread_rng();
    // Far end: speech-like noise, amplitude-modulated at 3 Hz.
    let far: Vec<f32> = (0..rate * secs).map(|i| {
        let env = (0.5 + 0.5 * ((i as f32 / rate as f32) * 3.0 * std::f32::consts::TAU).sin()).powi(2);
        env * rng.gen_range(-0.3..0.3)
    }).collect();
    let d = rate * 60 / 1000;
    let echo: Vec<f32> = (0..far.len()).map(|i| {
        let g = |k: usize| if i >= k { far[i - k] } else { 0.0 };
        0.4 * g(d) + 0.15 * g(d + 480) + 0.05 * g(d + 1440)
    }).collect();
    let near: Vec<f32> = (0..far.len()).map(|i| if i >= rate * 7 { 0.1 * ((i as f32) * 300.0 * std::f32::consts::TAU / rate as f32).sin() } else { 0.0 }).collect();
    let mut out = vec![0f32; far.len()];
    let t = std::time::Instant::now();
    for f in 0..far.len() / n {
        let r = f * n..(f + 1) * n;
        let mut render = vec![far[r.clone()].to_vec()];
        ap.process_render_frame(&mut render).unwrap();
        let mut cap = vec![(r.clone()).map(|i| echo[i] + near[i]).collect::<Vec<f32>>()];
        ap.process_capture_frame(&mut cap).unwrap();
        out[r].copy_from_slice(&cap[0]);
    }
    let el = t.elapsed();
    let pw = |v: &[f32], a: usize, b: usize| v[a * rate..b * rate].iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / ((b - a) * rate) as f64;
    for (a, b) in [(0, 1), (1, 3), (3, 7)] {
        println!("echo only {a}-{b}s: ERLE {:.1} dB", db(pw(&echo, a, b)) - db(pw(&out, a, b)));
    }
    // Double talk: project output onto the near talker.
    let (a, b) = (8 * rate, 10 * rate);
    let dot: f64 = (a..b).map(|i| out[i] as f64 * near[i] as f64).sum();
    let nn: f64 = (a..b).map(|i| (near[i] as f64).powi(2)).sum();
    let gain = dot / nn;
    let resid: f64 = (a..b).map(|i| (out[i] as f64 - gain * near[i] as f64).powi(2)).sum::<f64>() / (b - a) as f64;
    println!("double talk 8-10s: near talker kept at {:.1} dB, residual echo {:.1} dB below the echo",
        db(gain * gain), db(pw(&echo, 8, 10)) - db(resid));
    println!("processing: {:.2} ms per second of audio", el.as_secs_f64() * 1000.0 / secs as f64);
}
