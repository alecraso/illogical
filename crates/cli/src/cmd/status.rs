//! `illogical status`: how illogical is doing on this machine, one line
//! per part: the daemon (answering, its version), the service that runs it,
//! the binary that service runs and where its log is (#322), as the desktop
//! app's *Daemon* menu says them; and its standing with illogical control
//! (#325): joined where and whether connected, not joined, or dropped by
//! control and what it said. Each part is a [`Line`].
//!
//! The service, binary and log are this machine's: with `--host` or
//! `--ssh` only the daemon's own answer is shown.

use illogical_proto::{
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
                "join again: Getting started on this machine's page, or `illogicald leave` then `illogicald join {}`",
                c.url.as_deref().unwrap_or_default()
            ),
        }),
        "not_joined" => Some("`illogical join`, or Getting started on this machine's page".into()),
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
            Some(h) => {
                Line { part: "daemon", says: format!("{}: illogicald {}, running", h.name, h.version), fix: None }
            }
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
                format!("`systemctl --user start {}`, or the app's Daemon > Start", service::UNIT)
            }
            _ => "the app's Daemon > Start, or `illogicald install` (which keeps its flags)".into(),
        }),
        (None, Some(_)) => log.map(|l| format!("see why in {l}")),
        (None, None) => Some("`illogicald install` sets it up as a service and starts it".into()),
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
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    } else {
        for l in lines(host.as_ref().map(|h| (h, control.as_ref())), err.as_deref(), here.as_ref()) {
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
            url: Some("https://control.illogical.widgets.wtf".into()),
            kind: Some("team".into()),
            name: Some("arugula".into()),
            connected: true,
            ..Default::default()
        };
        let l = control_line(&joined);
        assert_eq!(l.says, "In the team arugula on control.illogical.widgets.wtf: connected");
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
            l.says.starts_with("Dropped by control: no longer in the team arugula on control.illogical.widgets.wtf"),
            "{}",
            l.says
        );
        assert!(l.fix.unwrap().contains("illogicald join https://control.illogical.widgets.wtf"));

        // A removed key: the join it asked for by itself (#330).
        let rejoining = ControlState {
            code: Some("ABCDE-FGHIJ".into()),
            approve: Some("https://control.illogical.widgets.wtf/#join=ABCDE-FGHIJ".into()),
            ..dropped
        };
        assert_eq!(
            control_line(&rejoining).fix.as_deref(),
            Some("joining again: approve code ABCDE-FGHIJ at https://control.illogical.widgets.wtf/#join=ABCDE-FGHIJ")
        );

        let none = ControlState { state: "not_joined".into(), ..Default::default() };
        assert_eq!(control_line(&none).says, "Not joined to illogical control");
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
            target: "gui/501/wtf.widgets.illogical.daemon".into(),
            file: PathBuf::from(service::APP_PLIST),
            running: true,
            loaded: true,
        };
        let log = service::Log::File("/Users/me/Library/Logs/illogicald.log".into());
        let here = Here { service: Some(agent.clone()), log: Some(log.clone()) };
        let ls = lines(Some((&host, Some(&not_joined))), None, Some(&here));
        let parts: Vec<_> = ls.iter().map(|l| l.part).collect();
        assert_eq!(parts, ["daemon", "binary", "log", "control"]);
        assert_eq!(
            ls[0].says,
            "jake-air: illogicald 0.21.0, running as the app's launch agent (wtf.widgets.illogical.daemon)"
        );
        assert_eq!(ls[1].says, service::APP_PROGRAM);
        assert_eq!(ls[2].says, "/Users/me/Library/Logs/illogicald.log");
        assert!(ls[3].fix.is_some(), "not joined says how to join");

        // Stopped: says so, and how to start it; no control line.
        let stopped = Here { service: Some(Service { running: false, loaded: false, ..agent }), log: Some(log) };
        let ls = lines(None, Some("connection refused"), Some(&stopped));
        assert!(ls[0].says.starts_with("Stopped (the app's launch agent"), "{}", ls[0].says);
        assert!(ls[0].fix.as_deref().unwrap().contains("Daemon > Start"));
        assert!(ls.iter().all(|l| l.part != "control"));

        // Nothing set up: install.
        let journal =
            Here { service: None, log: Some(service::Log::Journal("journalctl --user -u illogicald -e".into())) };
        let ls = lines(None, None, Some(&journal));
        assert_eq!(ls[0].fix.as_deref(), Some("`illogicald install` sets it up as a service and starts it"));
        assert_eq!(ls[1].says, "`journalctl --user -u illogicald -e`");

        // Another machine's daemon: what it says, not this machine's service.
        let ls = lines(Some((&host, None)), None, None);
        assert_eq!(ls[0].says, "jake-air: illogicald 0.21.0, running");
        assert_eq!(ls.len(), 2);
    }
}
