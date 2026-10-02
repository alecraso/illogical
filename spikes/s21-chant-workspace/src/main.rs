//! `s21-chant-workspace DIR [--env ENV] [--json] [--raw] [--times N]`: read a
//! chant workspace the way the block does (the same script, the same
//! composer) and print what the block would show.

mod model;

use std::{process::Command, time::Instant};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let opt = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let root = args.iter().find(|a| !a.starts_with("--") && Some(*a) != opt("--env").as_ref() && Some(*a) != opt("--times").as_ref()).cloned().unwrap_or_else(|| ".".into());
    let env = opt("--env").unwrap_or_else(|| "local".into());
    let times: usize = opt("--times").and_then(|n| n.parse().ok()).unwrap_or(1);

    let mut walls = vec![];
    let mut last = None;
    for _ in 0..times {
        let t = Instant::now();
        let out = Command::new("sh")
            .args(["-c", model::SCRIPT, "sh", &root, model::READER, &env])
            .output()
            .expect("sh");
        walls.push(t.elapsed().as_millis());
        last = Some(out.stdout);
    }
    let raw: serde_json::Value = serde_json::from_slice(&last.unwrap()).expect("the reader prints JSON");
    if flag("--raw") {
        println!("{}", serde_json::to_string_pretty(&raw).unwrap());
        return;
    }
    let st = model::compose(&raw, &env);
    if flag("--json") {
        println!("{}", serde_json::to_string_pretty(&st).unwrap());
    } else {
        print!("{}", st.text());
        if let Some(why) = st.attention() {
            println!("attention: {why}");
        }
    }
    if times > 1 {
        walls.sort();
        eprintln!("wall ms over {times} runs: min {} median {} max {}", walls[0], walls[times / 2], walls[times - 1]);
    }
}
