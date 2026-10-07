//! Remote blocks (#17, M4's option (a)): a place in this daemon's layout
//! for a pane that lives on another daemon in the host list.
//!
//! The block is only a reference, `{host, pane}`. Its terminal, size,
//! restart policy and history are the other daemon's, as for any of its
//! panes; clients connect to that daemon directly for its bytes, so this
//! one never relays them (M4). Here it can be split beside, moved, docked
//! and closed like any block, and it's kept in `layout.json` like one.
//!
//! This daemon never talks to the other about it. A client asks the other
//! daemon for the pane (`/api/run` there) and then records it here
//! (`/api/blocks`, type `remote`); closing it here closes only the
//! reference, and the client closes the pane on its host too. A client that
//! sees the pane gone from its host closes the reference.

use arugula_proto::{BlockType, RemoteRef};
use futures_util::future::BoxFuture;
use serde_json::Value;

use crate::block::{Block, BlockCtx, no_method};

pub struct Remote {
    at: RemoteRef,
}

impl Remote {
    pub fn create(ctx: BlockCtx, config: Value) -> Result<std::sync::Arc<dyn Block>, String> {
        let at = parse(&config)?;
        // Clients that are connected learn where it is (new ones do as
        // they connect).
        ctx.changed();
        Ok(std::sync::Arc::new(Self { at }))
    }
}

/// A remote block's config: `{"host": "mac", "pane": 7}`.
pub fn parse(config: &Value) -> Result<RemoteRef, String> {
    let at: RemoteRef =
        serde_json::from_value(config.clone()).map_err(|_| "a remote block needs {\"host\": …, \"pane\": N}")?;
    if at.host.trim().is_empty() {
        return Err("a remote block needs a host".into());
    }
    Ok(at)
}

impl Block for Remote {
    fn kind(&self) -> BlockType {
        BlockType::Remote
    }

    fn config(&self) -> Value {
        serde_json::to_value(&self.at).unwrap_or_default()
    }

    fn state(&self) -> Value {
        self.config()
    }

    fn text(&self) -> String {
        format!("%{} on {}\n", self.at.pane, self.at.host)
    }

    fn call(&self, method: &str, _args: Value) -> BoxFuture<'static, Result<Value, String>> {
        let result = match method {
            "state" => Ok(self.state()),
            m => Err(no_method(BlockType::Remote, m)),
        };
        Box::pin(async move { result })
    }

    /// The pane stays on its host: the client that closed this closes it
    /// there, and if it can't reach the host, the pane is still in the
    /// host's own layout.
    fn close(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_is_host_and_pane() {
        let at = parse(&serde_json::json!({"host": "mac", "pane": 7})).unwrap();
        assert_eq!(at, RemoteRef { host: "mac".into(), pane: 7 });
        assert!(parse(&serde_json::json!({"host": "", "pane": 7})).is_err());
        assert!(parse(&serde_json::json!({"host": "mac"})).is_err());
        assert!(parse(&serde_json::json!({"url": "x"})).is_err());
    }
}
