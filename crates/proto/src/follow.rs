//! What a followed editor sends its followers (M28, S17's follow stream):
//! [`crate::ServerMsg::Follow`]'s `msg`. The daemon passes these on as the
//! editor wrote them, less its `t`; these types say what they hold. Lines
//! count from 1, columns from 0.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(untagged)]
pub enum FollowMsg {
    /// Where the cursor is, and what's selected and in view.
    Cursor(FollowCursor),
    /// The file it shows now, whole (no `text` when it's too big).
    Open {
        open: FollowOpen,
    },
    /// Changes to that file since.
    Edit {
        edit: FollowEdit,
    },
    Diagnostics {
        diagnostics: FollowDiagnostics,
    },
    /// The editor left.
    Gone {
        #[cfg_attr(feature = "ts", ts(type = "true"))]
        gone: bool,
    },
}

/// A range: start line and column, end line and column.
pub type Range = [u32; 4];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FollowCursor {
    pub file: String,
    pub line: u32,
    pub col: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional = nullable))]
    pub sel: Option<Range>,
    /// The first and last lines in view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional = nullable))]
    pub view: Option<[u32; 2]>,
    /// nvim's mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FollowOpen {
    pub file: String,
    pub version: u64,
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub lang: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub too_big: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FollowEdit {
    pub file: String,
    pub version: u64,
    pub changes: Vec<FollowChange>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FollowChange {
    pub range: Range,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FollowDiagnostics {
    pub file: String,
    pub items: Vec<FollowDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FollowDiagnostic {
    pub range: Range,
    /// `error`, `warning`, `information`, `hint`.
    pub severity: String,
    pub message: String,
}
