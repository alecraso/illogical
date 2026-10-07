//! `arugula status`: how Arugula is doing on this machine, one line
//! per part: the daemon (answering, its version), the service that runs it,
//! the binary that service runs and where its log is (#322), as the desktop
//! app's *Daemon* menu says them; its standing with Arugula control
//! (#325): joined where and whether connected, not joined, or dropped by
//! control and what it said; and the agents (#335): each ACP adapter's
//! state and whether Claude Code has Arugula's MCP server. Each part is
//! a [`Line`].
//!
//! The service, binary and log are this machine's: with `--host` or
//! `--ssh` only the daemon's own answer is shown.

use arugula_proto::{
    hosts::{ControlState, HostInfo},
    service::{self, Service},
};
use serde_json::{Value, json};

use super::Ctx;
use crate::http::{Target, request};

/// One part's line: what it is, how it's doing, and what to do about it.
pub struct Line {
    pub part: &'static str,
    pub says: String,
    /// A command or place that fixes it, when something's wrong.
    pub fix: Option<String>,
}

/// The control line (#325), as the desktop app's tray says it too.
pub fn control_line(c: &ControlState) -> Line {
    let fix = match c.state.as_str() {
        "dropped" => Some(match (&c.code, &c.approve) {
            // Its key was removed, so it asked to join again by itself
            // (#330), or someone started a join.
            (Some(code), Some(at)) => format!("joining again: approve code {code} at {at}"),
            _ => format!(
                "join again: Getting started on this machine's page, or `arugulad leave` then `arugulad join {}`",
                c.url.as_deref().unwrap_or_default()
            ),
        }),
        "not_joined" => Some("`arugula join`, or Getting started on this machine's page".into()),
        _ => None,
    };
    let mut says = c.line();
    if c.is_dropped()
        && let Some(at) = c.dropped_ms
    {
        says.push_str(&format!(", {}", crate::util::time(at)));
    }
    Line { part: "control", says, fix }
}

/// The service here, if it's this machine's daemon that's asked: what runs
/// it, and where its log is.
pub struct Here {
    pub service: Option<Service>,
    pub log: Option<service::Log>,
}

/// The daemon's lines (#322): how it's doing and, here, which service runs
/// it (`service::line`), the binary and the log.
pub fn daemon_lines(host: Option<&HostInfo>, unreachable: Option<&str>, here: Option<&Here>) -> Vec<Line> {
    let Some(here) = here else {
        // Another machine's daemon: only what it says of itself.
        return vec![match host {
            Some(h) => Line { part: "daemon", says: format!("{}: arugulad {}, running", h.name, h.version), fix: None },
            None => Line {
                part: "daemon",
                says: format!("not answering ({})", unreachable.unwrap_or("no answer")),
                fix: None,
            },
        }];
    };
    let (svc, log) = (here.service.as_ref(), here.log.as_ref());
    let mut says = service::line(host.map(|h| h.version.as_str()), svc);
    if let Some(h) = host {
        says = format!("{}: {says}", h.name);
    } else if let Some(why) = unreachable {
        says.push_str(&format!(" ({why})"));
    }
    let fix = match (host, svc) {
        (Some(_), _) => None,
        (None, Some(s)) if !s.running => Some(match s.kind {
            service::Kind::Systemd => {
                format!("`systemctl --user start {}`, or the app's Daemon > Start", s.target)
            }
            _ => "the app's Daemon > Start, or `arugulad install` (which keeps its flags)".into(),
        }),
        (None, Some(_)) => log.map(|l| format!("see why in {l}")),
        (None, None) => Some("`arugulad install` sets it up as a service and starts it".into()),
    };
    let mut out = vec![Line { part: "daemon", says, fix }];
    if let Some(p) = svc.and_then(Service::program) {
        out.push(Line { part: "binary", says: p.display().to_string(), fix: None });
    }
    if let Some(l) = log {
        out.push(Line { part: "log", says: l.to_string(), fix: None });
    }
    out
}

/// Every part's line, from what the daemon said (`None`: it didn't answer)
/// and, for this machine's, the service found here.
pub fn lines(
    host: Option<(&HostInfo, Option<&ControlState>)>,
    unreachable: Option<&str>,
    here: Option<&Here>,
) -> Vec<Line> {
    let mut out = daemon_lines(host.map(|(h, _)| h), unreachable, here);
    let Some((h, control)) = host else { return out };
    match control {
        Some(c) => out.push(control_line(c)),
        // An older daemon: only whether it's joined.
        None => out.push(Line {
            part: "control",
            says: h
                .control
                .as_ref()
                .map_or("not joined".into(), |u| format!("joined to {u} (this daemon says no more)")),
            fix: None,
        }),
    }
    out
}

const SETUP: &str = "`arugula setup claude`, or Getting started's Agents step";

/// The agents' lines (#335), from `/api/setup?part=agents`: each adapter,
/// then Claude Code's MCP server.
pub fn agent_lines(v: &Value) -> Vec<Line> {
    let mut out = Vec::new();
    for a in v["adapters"].as_array().into_iter().flatten() {
        let s = |k: &str| a[k].as_str().unwrap_or_default();
        let (label, kind) = (s("label"), s("kind"));
        let found = a["found"].as_bool().unwrap_or(false);
        let setup = if kind == "claude" { SETUP.to_owned() } else { format!("`arugula setup {kind}`") };
        let (says, fix) = match s("state") {
            "installed" if a["outdated"] == true => (
                format!("{label}'s adapter {}, out of date (Arugula uses {})", s("version"), s("pinned")),
                Some(format!("{setup} updates it")),
            ),
            "installed" if a["on_path"] == true => (format!("{label}'s adapter, on PATH"), None),
            "installed" => (format!("{label}'s adapter {}", a["version"].as_str().unwrap_or(s("pinned"))), None),
            "no_node" => (
                format!("{}: agent blocks can't run {label}", s("why")),
                Some(format!("install Node (`mise use -g node@22`), then {setup}")),
            ),
            // Missing: only worth fixing where the agent is used.
            _ if found => (format!("{label}'s adapter isn't installed, and {label} is on this machine"), Some(setup)),
            _ => (format!("{label}'s adapter isn't installed ({label} isn't on this machine)"), None),
        };
        out.push(Line { part: "adapter", says, fix });
    }
    let c = &v["claude"];
    if c.is_object() {
        let (says, fix) = match (c["installed"].as_bool(), c["tools"].as_bool()) {
            // Under the old name too (#505): setup takes that one out.
            (_, Some(true)) if c["old"] == true => (
                "Claude Code has Arugula's MCP server, and also one under the old name (illogical)".to_owned(),
                Some(SETUP.to_owned()),
            ),
            (_, Some(true)) => ("Claude Code has Arugula's MCP server".to_owned(), None),
            (_, _) if c["old"] == true => (
                "Claude Code has Arugula's MCP server under the old name (illogical); it works until renamed"
                    .to_owned(),
                Some(SETUP.to_owned()),
            ),
            (Some(true), _) => ("Claude Code doesn't have Arugula's MCP server".to_owned(), Some(SETUP.to_owned())),
            _ => ("Claude Code isn't on this machine".to_owned(), None),
        };
        out.push(Line { part: "mcp", says, fix });
    }
    out
}

pub fn run(ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let got = request(&sock, "GET", "/api/host", None).and_then(|r| r.json());
    let (raw, err) = match got {
        Ok(v) => (Some(v), None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    let host = raw.as_ref().and_then(|v| serde_json::from_value::<HostInfo>(v.clone()).ok());
    let control = raw.as_ref().and_then(ControlState::of_host);
    let here = matches!(sock, Target::Socket(_)).then(|| Here { service: service::find(), log: service::log() });
    // The agents (#335): an older daemon answers with all of /api/setup
    // (no adapters), or not at all.
    let agents =
        host.as_ref().and_then(|_| request(&sock, "GET", "/api/setup?part=agents", None).and_then(|r| r.json()).ok());
    if json_out {
        let svc = here.as_ref().and_then(|h| h.service.as_ref());
        let v: Value = json!({
            "daemon": host.as_ref().map(|h| json!({ "name": h.name, "version": h.version })),
            "error": err,
            "service": svc.map(|s| json!({
                "name": s.name(),
                "target": s.target,
                "file": s.file,
                "running": s.running,
                "binary": s.program(),
            })),
            "log": here.as_ref().and_then(|h| h.log.as_ref()).map(|l| l.to_string()),
            "control": control,
            "agents": agents,
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    } else {
        let mut ls = lines(host.as_ref().map(|h| (h, control.as_ref())), err.as_deref(), here.as_ref());
        ls.extend(agents.as_ref().map(agent_lines).unwrap_or_default());
        for l in ls {
            println!("{:<8} {}", l.part, l.says);
            if let Some(f) = l.fix {
                println!("{:<8} {f}", "");
            }
        }
    }
    // Something to look at: the daemon's down or control dropped it.
    let bad = host.is_none() || control.as_ref().is_some_and(ControlState::is_dropped);
    Ok(if bad { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_control_line_says_where_and_how() {
        let joined = ControlState {
            state: "joined".into(),
            url: Some("https://control.arugula.io".into()),
            kind: Some("team".into()),
            name: Some("arugula".into()),
            connected: true,
            ..Default::default()
        };
        let l = control_line(&joined);
        assert_eq!(l.says, "In the team arugula on control.arugula.io: connected");
        assert!(l.fix.is_none());

        let dropped = ControlState {
            state: "dropped".into(),
            said: Some("not an enrolled daemon (left, or revoked?)".into()),
            dropped_ms: Some(0),
            connected: false,
            ..joined.clone()
        };
        let l = control_line(&dropped);
        assert!(
            l.says.starts_with("Dropped by control: no longer in the team arugula on control.arugula.io"),
            "{}",
            l.says
        );
        assert!(l.fix.unwrap().contains("arugulad join https://control.arugula.io"));

        // A removed key: the join it asked for by itself (#330).
        let rejoining = ControlState {
            code: Some("ABCDE-FGHIJ".into()),
            approve: Some("https://control.arugula.io/#join=ABCDE-FGHIJ".into()),
            ..dropped
        };
        assert_eq!(
            control_line(&rejoining).fix.as_deref(),
            Some("joining again: approve code ABCDE-FGHIJ at https://control.arugula.io/#join=ABCDE-FGHIJ")
        );

        let none = ControlState { state: "not_joined".into(), ..Default::default() };
        assert_eq!(control_line(&none).says, "Not joined to Arugula control");
    }

    #[test]
    fn the_agent_lines_say_each_adapter_and_the_mcp_server() {
        let v = json!({
            "claude": { "installed": true, "tools": false },
            "adapters": [
                { "kind": "claude", "label": "Claude Code", "state": "missing", "found": true, "pinned": "0.85.0" },
                { "kind": "codex", "label": "Codex", "state": "missing", "found": false, "pinned": "2.1.0" },
            ],
        });
        let ls = agent_lines(&v);
        let parts: Vec<_> = ls.iter().map(|l| l.part).collect();
        assert_eq!(parts, ["adapter", "adapter", "mcp"]);
        assert_eq!(ls[0].says, "Claude Code's adapter isn't installed, and Claude Code is on this machine");
        assert!(ls[0].fix.as_deref().unwrap().contains("arugula setup claude"));
        assert!(ls[1].fix.is_none(), "no Codex here: nothing to do");
        assert_eq!(ls[2].says, "Claude Code doesn't have Arugula's MCP server");
        assert!(ls[2].fix.is_some());

        let v = json!({
            "claude": { "installed": true, "tools": true },
            "adapters": [
                { "kind": "claude", "label": "Claude Code", "state": "installed", "version": "0.81.2",
                  "outdated": true, "pinned": "0.85.0", "found": true },
                { "kind": "codex", "label": "Codex", "state": "installed", "version": "2.1.0",
                  "outdated": false, "pinned": "2.1.0", "found": true },
            ],
        });
        let ls = agent_lines(&v);
        assert_eq!(ls[0].says, "Claude Code's adapter 0.81.2, out of date (Arugula uses 0.85.0)");
        assert!(ls[0].fix.as_deref().unwrap().ends_with("updates it"));
        assert_eq!(ls[1].says, "Codex's adapter 2.1.0");
        assert!(ls[1].fix.is_none());
        assert_eq!(ls[2].says, "Claude Code has Arugula's MCP server");

        // Registered under the old name (#505): there, with a fix.
        let old = agent_lines(&json!({ "claude": { "installed": true, "tools": false, "old": true } }));
        assert!(old[0].says.contains("under the old name"), "{}", old[0].says);
        assert!(old[0].fix.is_some());
        let both = agent_lines(&json!({ "claude": { "installed": true, "tools": true, "old": true } }));
        assert!(both[0].says.contains("also one under the old name"));

        // An older daemon: no adapters, no lines for them.
        assert_eq!(agent_lines(&json!({})).len(), 0);
    }

    #[test]
    fn the_daemon_lines_say_its_service_binary_and_log() {
        use std::path::PathBuf;
        let host = HostInfo {
            name: "jake-air".into(),
            version: "0.21.0".into(),
            protocol: None,
            tailnet_url: None,
            tailnet_seen: false,
            control: None,
            team: None,
            fountain_runner: None,
            features: None,
        };
        let not_joined = ControlState { state: "not_joined".into(), ..Default::default() };
        let agent = Service {
            kind: service::Kind::AppAgent,
            target: format!("gui/501/{}", service::APP_LABEL),
            file: PathBuf::from(service::APP_PLIST),
            running: true,
            loaded: true,
        };
        let log = service::Log::File("/Users/me/Library/Logs/arugulad.log".into());
        let here = Here { service: Some(agent.clone()), log: Some(log.clone()) };
        let ls = lines(Some((&host, Some(&not_joined))), None, Some(&here));
        let parts: Vec<_> = ls.iter().map(|l| l.part).collect();
        assert_eq!(parts, ["daemon", "binary", "log", "control"]);
        assert_eq!(
            ls[0].says,
            "jake-air: arugulad 0.21.0, running as the app's launch agent (io.arugula.desktop.daemon)"
        );
        assert_eq!(ls[1].says, service::APP_PROGRAM);
        assert_eq!(ls[2].says, "/Users/me/Library/Logs/arugulad.log");
        assert!(ls[3].fix.is_some(), "not joined says how to join");

        // Stopped: says so, and how to start it; no control line.
        let stopped = Here { service: Some(Service { running: false, loaded: false, ..agent }), log: Some(log) };
        let ls = lines(None, Some("connection refused"), Some(&stopped));
        assert!(ls[0].says.starts_with("Stopped (the app's launch agent"), "{}", ls[0].says);
        assert!(ls[0].fix.as_deref().unwrap().contains("Daemon > Start"));
        assert!(ls.iter().all(|l| l.part != "control"));

        // Nothing set up: install.
        let journal =
            Here { service: None, log: Some(service::Log::Journal("journalctl --user -u arugulad -e".into())) };
        let ls = lines(None, None, Some(&journal));
        assert_eq!(ls[0].fix.as_deref(), Some("`arugulad install` sets it up as a service and starts it"));
        assert_eq!(ls[1].says, "`journalctl --user -u arugulad -e`");

        // Another machine's daemon: what it says, not this machine's service.
        let ls = lines(Some((&host, None)), None, None);
        assert_eq!(ls[0].says, "jake-air: arugulad 0.21.0, running");
        assert_eq!(ls.len(), 2);
    }
}
