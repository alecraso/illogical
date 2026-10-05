fn main() {
    let mut log = s30_mls::Log::default();
    let which = std::env::args().nth(1).unwrap_or_default();
    if which.is_empty() || which == "offline" {
        s30_mls::scenarios::offline(&mut log);
        log.say("");
    }
    if which.is_empty() || which == "hostile" {
        s30_mls::scenarios::hostile(&mut log);
    }
}
