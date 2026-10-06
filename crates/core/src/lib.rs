//! The multiplexer's state, independent of PTYs and networking: sessions
//! hold tabs, tabs hold a split tree of panes, and clients change them by
//! sending intents. IDs follow tmux (`$session`, `@tab` for tmux's window,
//! `%pane`) and are never reused.

pub mod access;
pub mod layout;
pub mod mux;
pub mod names;
pub mod tree;

pub type SessionId = u32;
pub type TabId = u32;
pub type PaneId = u32;
pub type NodeId = u32;
pub type ClientId = u64;

pub use access::{Need, Role};
pub use layout::{Layout, Rect, SplitRect};
pub use mux::{Claim, Effect, Error, Intent, Mux, OptionMap, OptionScope, Options, SIZE_HOLD, Session, SizeHold, Tab};
pub use tree::{Child, Dir, Edge, Node};

#[cfg(test)]
mod tests;
