//! Server-side terminal state for a pane.
//!
//! [`VtEngine`] is what the daemon needs from a terminal emulator: feed it
//! PTY output, resize it, collect its replies to terminal queries, and take a
//! snapshot that reproduces the screen in a fresh terminal. The only
//! implementation is [`GhosttyEngine`] (libghostty-vt). See
//! `spikes/s1-ghostty/README.md` for why and for what the snapshot fixes up.

mod compat;
mod ghostty;

pub use compat::Capabilities;
pub use ghostty::{CheckpointError, GhosttyEngine, engine_tag};

pub trait VtEngine {
    /// Process PTY output.
    fn feed(&mut self, bytes: &[u8]);
    /// Change the terminal size (reflows the primary screen).
    fn resize(&mut self, cols: u16, rows: u16);
    fn size(&self) -> (u16, u16);
    /// Bytes the terminal wants written back to the PTY: answers to device
    /// attribute, status and color queries, limited to what the client
    /// renderer supports ([`Capabilities`]). Drained on each call.
    fn take_replies(&mut self) -> Vec<u8>;
    /// VT bytes that recreate the current state (both screens, scrollback,
    /// modes, cursor, title) when written to a fresh terminal of the same size.
    fn snapshot(&mut self) -> Vec<u8>;
    /// Plain text of the active screen, including scrollback when the
    /// primary screen is active.
    fn plain_text(&self) -> String;
    /// The same with colors and styles as escape sequences (no modes or
    /// cursor), for `capture --ansi`.
    fn vt_text(&self) -> String;
    /// The same as HTML with inline styles.
    fn html(&self) -> String;
    fn title(&self) -> String;
    /// Whether a full-screen program's alternate screen is showing.
    fn alt_screen(&self) -> bool;
    /// Whether a DEC private mode (`CSI ? n h`) is set.
    fn dec_mode(&self, mode: u16) -> bool;
    fn pwd(&self) -> String;
}
