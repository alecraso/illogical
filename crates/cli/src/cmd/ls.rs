//! `arugula ls`: the panes.

use super::Ctx;
use crate::http::request;
use crate::util::print_json;
use serde_json::Value;

pub fn run(ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let v = request(&sock, "GET", "/api/panes", None)?.json()?;
    if json_out {
        print_json(&v);
        return Ok(0);
    }
    for p in v.as_array().into_iter().flatten() {
        let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("");
        let tab =
            p.get("tab_name").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| format!("@{}", p["tab"]));
        let what = if !p["running"].as_bool().unwrap_or(false) {
            "(waiting)".to_owned()
        } else {
            p["current"]["text"].as_str().or(p["command"].as_str()).unwrap_or("").to_owned()
        };
        let attention = match s("attention") {
            "idle" | "" => String::new(),
            a => format!("  [{a}]"),
        };
        let host = p["host"].as_u64().map(|m| format!("  (vm m{m})")).unwrap_or_default();
        println!("%{:<4} {:<12} {:<14} {:<36} {what}{attention}{host}", p["id"], s("session_name"), tab, s("cwd"));
    }
    Ok(0)
}
