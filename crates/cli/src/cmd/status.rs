//! `illogical status`: how illogical is doing on this machine, one line
//! per part: the daemon (answering, its version) and its standing with
//! illogical control (#325): joined where and whether connected, not
//! joined, or dropped by control and what it said. Each part is a
//! [`Line`], so more parts go in the same list.

use illogical_proto::hosts::{ControlState, HostInfo};
use serde_json::{Value, json};

use super::Ctx;
use crate::http::request;

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

/// Every part's line, from what the daemon said (`None`: it didn't answer).
pub fn lines(host: Option<(&HostInfo, Option<&ControlState>)>, unreachable: Option<&str>) -> Vec<Line> {
    let Some((h, control)) = host else {
        return vec![Line {
            part: "daemon",
            says: format!("not answering ({})", unreachable.unwrap_or("no answer")),
            fix: Some("`illogicald install` starts it".into()),
        }];
    };
    let mut out =
        vec![Line { part: "daemon", says: format!("{} running, illogicald {}", h.name, h.version), fix: None }];
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
    if json_out {
        let v: Value = json!({
            "daemon": host.as_ref().map(|h| json!({ "name": h.name, "version": h.version })),
            "error": err,
            "control": control,
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    } else {
        for l in lines(host.as_ref().map(|h| (h, control.as_ref())), err.as_deref()) {
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
    fn a_daemon_that_doesnt_answer_says_how_to_start_it() {
        let l = lines(None, Some("connection refused"));
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].says, "not answering (connection refused)");
    }
}
