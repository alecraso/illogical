//! Gates waiting for a person (M34), whoever read them: the `gate` reason
//! the swarm's rail, push and the phone show, and approving one through
//! where it came from. The workspace block reads chant's; a studio app
//! block (M35) reads hud's; a forge block (M36) raises one for a review
//! asked of you. Neither the reason nor the card knows which.

use arugula_proto::{Action, Gate, GateSource, Reason, ReasonKind};

use crate::{review::Runner, store::now_ms};

/// Why a block with these gates waiting wants you: the first gate, with
/// how many more, bundled by its workspace. `allow` approves it.
pub fn reason(gates: &[Gate]) -> Option<Reason> {
    let g = gates.first()?;
    let more = if gates.len() > 1 { format!(" (+{} more)", gates.len() - 1) } else { String::new() };
    Some(Reason {
        kind: ReasonKind::Gate,
        since_ms: g.since.as_deref().and_then(rfc3339_ms).unwrap_or_else(now_ms),
        headline: format!("{}{more}", g.headline()),
        command: g.command.clone(),
        exit: None,
        duration_ms: None,
        bundle: Some(g.bundle()),
        ask: None,
        gate: Some(Box::new(g.clone())),
        actions: vec![Action::Allow, Action::Dismiss],
    })
}

/// Approve `gate` as `approver` (#75: the owner or an editor, by their
/// Arugula name), through its source. What the source said, on success.
pub async fn approve(gate: &Gate, approver: Option<&str>, via: &Via<'_>) -> Result<String, String> {
    match (&gate.source, via) {
        (GateSource::Chant { dir, .. }, Via::Chant { runner, chant }) => {
            // `chant approve <op> <gate> [--env E] --approver <name>`, in
            // the member's directory: chant finds the member's ledger from
            // there.
            let script = r#"cd "$1" || exit 1; c=$2; shift 2; exec "$c" approve "$@" 2>&1"#;
            let mut args = vec![dir.clone(), (*chant).to_owned(), gate.op.clone(), gate.gate.clone()];
            if let Some(env) = &gate.env {
                args.extend(["--env".into(), env.clone()]);
            }
            if let Some(by) = approver {
                args.extend(["--approver".into(), by.to_owned()]);
            }
            let (out, code) = runner.sh(script, &args).await?;
            let said = plain(&String::from_utf8_lossy(&out));
            match code {
                Some(0) => Ok(said),
                _ if said.is_empty() => Err(format!("chant approve failed (exit {})", code.unwrap_or(-1))),
                _ => Err(said),
            }
        }
        (GateSource::Hud { .. }, Via::Hud { session, follower }) => {
            // hud re-reads `workspace status` and approves only a gate
            // pending there, with `--actor` its roster name, or with a
            // follower credential, the person named here.
            let mut body = serde_json::json!({
                "member": gate.member, "component": gate.op, "gate": gate.gate, "env": gate.env,
            });
            if *follower && let Some(name) = approver.and_then(crate::apps::hud::hud_name) {
                body["onBehalfOf"] = serde_json::json!({ "name": name, "via": "arugula" });
            }
            session.approve_gate(&body).await
        }
        // M36: a review asked of you is approved as a review, by the forge
        // block itself (`act` calls its `review` method).
        (GateSource::Forge { .. }, _) => Err("a review is approved through its forge block".into()),
        (GateSource::Chant { .. }, Via::Hud { .. }) | (GateSource::Hud { .. }, Via::Chant { .. }) => {
            Err("this gate isn't this block's to approve".into())
        }
    }
}

/// What a source needs to approve one of its gates.
pub enum Via<'a> {
    /// The workspace's chant, run on the block's host with the user's
    /// shell environment.
    Chant { runner: &'a Runner, chant: &'a str },
    /// The app block's session with hud in its box (M35); `follower`:
    /// it's a follower credential, so hud is told who approves.
    Hud { session: &'a crate::apps::hud::Session, follower: bool },
}

/// Text without terminal colours, trimmed.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            if it.peek() == Some(&'[') {
                it.next();
                for c in it.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out.trim().to_owned()
}

/// `2026-10-02T21:09:51.548Z` as ms since the epoch (UTC only, as chant
/// writes them).
pub(crate) fn rfc3339_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || !s.ends_with('Z') {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    let ms = match s.get(19..s.len() - 1) {
        Some(f) if f.starts_with('.') => format!("{:0<3}", &f[1..f.len().min(4)]).parse::<i64>().ok()?,
        _ => 0,
    };
    // Days from civil (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + ms).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(member: &str) -> Gate {
        Gate {
            member: member.into(),
            op: "ship".into(),
            gate: "approve-ship".into(),
            env: None,
            since: Some("2026-10-02T21:09:51.548Z".into()),
            expires: None,
            approvals: 0,
            needed: 1,
            command: Some("chant approve ship approve-ship".into()),
            source: GateSource::Chant { root: "/w".into(), dir: format!("/w/{member}"), machine: None },
        }
    }

    #[test]
    fn a_reason_from_gates() {
        assert_eq!(reason(&[]), None);
        let r = reason(&[gate("delivery"), gate("app")]).unwrap();
        assert_eq!(r.kind, ReasonKind::Gate);
        assert_eq!(r.headline, "delivery: ship waits at gate approve-ship (+1 more)");
        assert_eq!(r.bundle.as_deref(), Some("gate:/w"));
        assert_eq!(r.since_ms, 1_790_975_391_548);
        assert_eq!(r.gate.unwrap().key(), "delivery/ship/approve-ship");
        assert_eq!(r.actions, [Action::Allow, Action::Dismiss]);
        let one = reason(&[gate("delivery")]).unwrap();
        assert_eq!(one.headline, "delivery: ship waits at gate approve-ship");
    }

    #[test]
    fn times_and_text() {
        assert_eq!(rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339_ms("2000-03-01T00:00:00.5Z"), Some(951_868_800_500));
        assert_eq!(rfc3339_ms("yesterday"), None);
        assert_eq!(plain("\x1b[32mGate \"g\" resolved\x1b[0m\n"), "Gate \"g\" resolved");
    }
}
