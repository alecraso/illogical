//! `arugula machines`: the machines panes run on.

use super::Ctx;
use crate::http::request;
use crate::util::print_json;
use serde_json::Value;

pub fn run(ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let v = request(&sock, "GET", "/api/machines", None)?.json()?;
    if json_out {
        print_json(&v);
        return Ok(0);
    }
    for m in v.as_array().into_iter().flatten() {
        let s = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or("");
        let image = m["image"].as_str().unwrap_or("default image");
        let owner = match (m["owner"]["tab"].as_u64(), m["owner"]["pane"].as_u64()) {
            (Some(t), _) => format!("@{t}"),
            (_, Some(p)) => format!("%{p}"),
            _ => "?".into(),
        };
        let (state, sprite, provider, name) = (s("state"), s("sprite"), s("provider"), s("name"));
        println!("m{:<4} {owner:<5} {state:<9} {name:<18} {sprite:<34} {provider} ({image})", m["id"]);
    }
    Ok(0)
}
