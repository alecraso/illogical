fn main() {
    let sizes: Vec<usize> = std::env::args().nth(1).unwrap_or("2,10,50".into()).split(',').map(|s| s.parse().unwrap()).collect();
    let reps: usize = std::env::args().nth(2).map(|s| s.parse().unwrap()).unwrap_or(9);
    let rows = s30_mls::bench::run(&sizes, reps);
    print!("{}", s30_mls::bench::table(&rows));
    if let Some(p) = std::env::args().nth(3) {
        std::fs::write(p, serde_json::to_string_pretty(&rows).unwrap()).unwrap();
    }
}
