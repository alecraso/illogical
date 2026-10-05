//! S30 MLS half: openmls for illogical team channels. See FINDINGS.md.
pub mod bench;
pub mod client;
pub mod ds;
pub mod ident;
pub mod roster;
pub mod scenarios;
#[cfg(target_arch = "wasm32")]
mod wasm;

/// A transcript that works the same natively and in wasm.
#[derive(Default)]
pub struct Log {
    pub lines: Vec<String>,
    pub quiet: bool,
}

impl Log {
    pub fn quiet() -> Self {
        Log { lines: vec![], quiet: true }
    }
    pub fn say(&mut self, s: impl Into<String>) {
        if self.quiet {
            return;
        }
        let s = s.into();
        #[cfg(not(target_arch = "wasm32"))]
        println!("{s}");
        self.lines.push(s);
    }
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}
