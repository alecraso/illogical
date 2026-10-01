//! Machines other than this host that panes run on: throwaway sandboxes
//! from a provider (wisp sprites, by default: Firecracker microVMs on a
//! host running wispd), and sandboxes that already exist, borrowed for a
//! shell (M4b's "open shell", with no daemon inside).
//!
//! A VM pane's terminal is an exec TTY session on its machine, through the
//! [`Provider`]. The daemon runs its own terminal engine and log over
//! those bytes, as for a local pane, so scrollback, snapshots, `tail` and
//! history don't depend on the provider's replay buffer.
//!
//! - **Close**: delete the sandbox, which ends every session on it at once
//!   (a borrowed one is left alone; its shells are hung up).
//! - **Machine gone**: the session drops and the sandbox is gone.

use std::sync::Arc;

use crate::provider::Provider;
pub use crate::provider::{Begin, Exec, ExecEvent};

/// Start (or resume) a session on `sprite`. Events go to `sink` in order,
/// from a task on `rt`.
pub fn start(
    rt: &tokio::runtime::Handle,
    provider: Arc<dyn Provider>,
    sprite: String,
    begin: Begin,
    size: (u16, u16),
    sink: impl Fn(ExecEvent) -> bool + Send + 'static,
) -> Exec {
    provider.exec_tty(rt, sprite, begin, size, Box::new(sink))
}
