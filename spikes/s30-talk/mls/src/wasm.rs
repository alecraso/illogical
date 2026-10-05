//! wasm-bindgen entry points: the same benchmark and scenarios, in wasm.
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn bench(sizes: &str, reps: usize) -> String {
    let sizes: Vec<usize> = sizes.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    serde_json::to_string(&crate::bench::run(&sizes, reps)).unwrap()
}

#[wasm_bindgen]
pub fn demo() -> String {
    let mut log = crate::Log::default();
    crate::scenarios::demo(&mut log);
    log.text()
}

#[wasm_bindgen]
pub fn scenarios() -> String {
    let mut log = crate::Log::default();
    crate::scenarios::offline(&mut log);
    log.say("");
    crate::scenarios::hostile(&mut log);
    log.text()
}
