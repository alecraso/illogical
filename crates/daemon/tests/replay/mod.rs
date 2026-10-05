//! The replay agent (`crates/vt/fixtures/agents/replay.py`) installed for a
//! test: a program named `claude` (or `codex`) that plays a recorded
//! session with its timing, waiting where someone typed.

#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde_json::Value;

pub fn agents_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vt/fixtures/agents")
}

pub struct Replay {
    /// The program: `<dir>/claude`.
    pub bin: PathBuf,
}

impl Replay {
    /// `<dir>/<program>` playing `<cast>.cast`.
    pub fn install(dir: &Path, program: &str, cast: &str) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        let bin = dir.join(program);
        std::fs::copy(agents_dir().join("replay.py"), &bin).unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::fs::copy(agents_dir().join(format!("{cast}.cast")), bin.with_extension("cast")).unwrap();
        Self { bin }
    }

    pub fn path(&self) -> String {
        self.bin.display().to_string()
    }

    fn side(&self, ext: &str) -> PathBuf {
        PathBuf::from(format!("{}.{ext}", self.bin.display()))
    }

    /// What it logged: `start`, `m working`, …, `end`.
    pub fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.side("log")).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    /// Wait until it has logged `line` `n` times (it reached that point of
    /// the recording).
    pub fn reached(&self, line: &str, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.log().iter().filter(|l| *l == line).count() < n {
            assert!(Instant::now() < deadline, "the replay never reached {line:?} #{n}: {:?}", self.log());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Each time it was started: `{argv, cwd, pane}`.
    pub fn starts(&self) -> Vec<Value> {
        std::fs::read_to_string(self.side("argv"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}
