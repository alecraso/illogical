//! The protocol side of agent blocks, without a process: frames in, state
//! out, and the same state again from the log.

use super::*;

fn frame(d: &str, m: Value) -> Vec<u8> {
    json!({ "t": 1000, "d": d, "m": m }).to_string().into_bytes()
}

/// A session with one turn that asked for permission, as the log has it.
fn log_lines() -> Vec<Vec<u8>> {
    vec![
        frame("note", json!({ "e": "spawn", "argv": ["fake"] })),
        frame("out", json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} })),
        frame(
            "in",
            json!({ "jsonrpc": "2.0", "id": 1, "result": { "protocolVersion": 1, "agentCapabilities": { "loadSession": true, "sessionCapabilities": { "resume": {} } }, "agentInfo": { "name": "fake", "version": "1" } } }),
        ),
        frame("out", json!({ "jsonrpc": "2.0", "id": 2, "method": "session/new", "params": { "cwd": "/" } })),
        frame("in", json!({ "jsonrpc": "2.0", "id": 2, "result": { "sessionId": "s1" } })),
        frame(
            "out",
            json!({ "jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": { "sessionId": "s1", "prompt": [{ "type": "text", "text": "make x" }] } }),
        ),
        frame(
            "in",
            json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "s1", "update": { "sessionUpdate": "usage_update", "cost": { "amount": 0.5, "currency": "USD" } } } }),
        ),
        frame(
            "in",
            json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "s1", "update": { "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "touch x", "kind": "execute", "status": "pending", "rawInput": { "command": "touch x" } } } }),
        ),
        frame(
            "in",
            json!({ "jsonrpc": "2.0", "id": 0, "method": "session/request_permission", "params": { "sessionId": "s1", "toolCall": { "toolCallId": "t1", "name": "Bash", "title": "touch x", "kind": "execute", "rawInput": { "command": "touch x" } }, "options": [
                { "optionId": "allow-once", "name": "Yes", "kind": "allow_once" },
                { "optionId": "allow-with-updates", "name": "Yes, always", "kind": "allow_always" },
                { "optionId": "reject", "name": "No", "kind": "reject_once" } ] } }),
        ),
    ]
}

fn rebuilt() -> Inner {
    let mut inner = Inner::new(Config::default(), None);
    for l in log_lines() {
        inner.rebuild_line(&l);
    }
    inner
}

#[test]
fn the_log_rebuilds_what_is_outstanding() {
    let g = rebuilt();
    assert_eq!(g.cfg.session_id.as_deref(), Some("s1"));
    assert_eq!(g.next_id, 4, "ids after a restart don't collide");
    assert_eq!(g.prompt_id, Some(3));
    assert_eq!(g.ours.get(&3), Some(&Purpose::Prompt));
    assert_eq!(g.status, Status::Working);
    assert_eq!(g.pending.len(), 1);
    let p = &g.pending[0];
    assert_eq!((p.id.as_str(), p.tool.as_str(), p.title.as_str(), p.rpc.clone()), ("0", "Bash", "touch x", json!(0)));
    assert_eq!(p.options.len(), 3);
    assert_eq!(g.attention(), (Attention::NeedsInput, "wants to run touch x".into()));
    assert_eq!(g.t.entries.len(), 2);
}

#[test]
fn answering_clears_the_card_and_the_turn_reports_its_cost() {
    let mut g = rebuilt();
    let answer = json!({ "jsonrpc": "2.0", "id": 0, "result": { "outcome": { "outcome": "selected", "optionId": "allow-once" } } });
    g.on_out(&answer, 2000);
    assert!(g.pending.is_empty());
    assert_eq!(g.attention().0, Attention::Working);
    let u = |update: Value| json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "update": update } });
    g.on_in(
        &u(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed", "_meta": { "terminal_output": { "data": "ok" }, "terminal_exit": { "exit_code": 0 } } })),
        2100,
    );
    g.on_in(&u(json!({ "sessionUpdate": "usage_update", "cost": { "amount": 0.75, "currency": "USD" } })), 2200);
    let fx = g.on_in(
        &json!({ "jsonrpc": "2.0", "id": 3, "result": { "stopReason": "end_turn", "usage": { "totalTokens": 99 } } }),
        2300,
    );
    assert_eq!(fx, vec![Effect::TurnEnded]);
    assert_eq!(g.status, Status::Ready);
    let turn = g.turns.last().unwrap();
    assert_eq!((turn.cost, turn.stop.as_deref()), (Some(0.75), Some("end_turn")), "all of the first turn");
    assert_eq!(g.attention().0, Attention::Done);
    assert_eq!(g.t.tool("t1").unwrap().output, "ok");

    // A second turn's cost is its own delta.
    g.on_out(&json!({ "jsonrpc": "2.0", "id": 4, "method": "session/prompt", "params": { "prompt": [{ "type": "text", "text": "again" }] } }), 3000);
    g.on_in(&u(json!({ "sessionUpdate": "usage_update", "cost": { "amount": 1.0 } })), 3100);
    assert_eq!(g.turns.last().unwrap().cost, Some(0.25));
    g.on_in(&json!({ "jsonrpc": "2.0", "id": 4, "result": { "stopReason": "cancelled" } }), 3200);
    assert_eq!(g.attention().0, Attention::Idle, "a cancelled turn isn't news");
}

#[test]
fn a_finished_tool_call_drops_its_card() {
    // Fountain refuses an unanswered request after 5 minutes, saying only
    // that the tool call failed.
    let mut g = rebuilt();
    g.on_in(
        &json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "update": { "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "failed", "rawOutput": "User refused permission to run tool" } } }),
        5000,
    );
    assert!(g.pending.is_empty());
    assert_eq!(g.t.tool("t1").unwrap().text, "User refused permission to run tool");
}

#[test]
fn a_new_server_drops_what_the_old_one_had_open() {
    let mut g = rebuilt();
    g.rebuild_line(&frame("note", json!({ "e": "exit", "why": "exited with code 1" })));
    assert!(g.pending.is_empty() && g.ours.is_empty() && g.prompt_id.is_none());
    assert_eq!(g.status, Status::Exited);
    assert!(g.interrupted);
    assert_eq!(g.attention().0, Attention::NeedsInput);
    g.rebuild_line(&frame("note", json!({ "e": "spawn", "resume": true })));
    assert_eq!(g.status, Status::Starting);
    assert!(g.t.markdown().contains("The agent exited with code 1"));
}

#[test]
fn a_missing_adapter_is_said_once_and_kept_until_it_starts() {
    let mut g = rebuilt();
    let adapter = json!({ "kind": "claude", "state": "missing", "npm": "npm install --prefix ~/x/claude p@1" });
    g.rebuild_line(&frame(
        "note",
        json!({ "e": "exit", "why": "couldn't start: Claude Code's adapter isn't installed", "adapter": adapter }),
    ));
    assert_eq!(g.error.as_deref(), Some("the agent couldn't start: Claude Code's adapter isn't installed"));
    assert_eq!(g.adapter.as_ref(), Some(&adapter));
    // #335: its text says how to go on, where the page has Install.
    let fix = adapter_fix(&adapter, 7).unwrap();
    assert!(fix.contains("`arugula setup claude`"), "{fix}");
    assert!(fix.contains("`npm install --prefix ~/x/claude p@1`") && fix.contains("`arugula call %7 resume`"), "{fix}");
    let node = json!({ "kind": "codex", "state": "no_node", "npm": "npm i", "node_major": 20 });
    assert!(adapter_fix(&node, 7).unwrap().contains("install Node 20+"));
    assert!(adapter_fix(&json!({ "kind": "claude", "state": "installed" }), 7).is_none());
    assert_eq!(g.t.markdown().matches("couldn't start").count(), 1);
    g.rebuild_line(&frame("note", json!({ "e": "spawn", "resume": true })));
    assert!(g.adapter.is_none() && g.error.is_none());
    // Any other exit has none.
    g.rebuild_line(&frame("note", json!({ "e": "exit", "why": "exited with code 1", "adapter": null })));
    assert!(g.adapter.is_none());
}

#[test]
fn a_load_replay_is_merged_not_duplicated() {
    let mut g = rebuilt();
    g.rebuild_line(&frame("note", json!({ "e": "spawn", "resume": true })));
    g.on_out(&json!({ "jsonrpc": "2.0", "id": 10, "method": "session/load", "params": {} }), 1);
    let u = |update: Value| json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "update": update } });
    // Fountain's replay: no user messages, the tool finished, a new reply.
    g.on_in(
        &u(json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "touch x", "status": "completed" })),
        2,
    );
    g.on_in(&u(json!({ "sessionUpdate": "agent_message_chunk", "messageId": "m9", "content": { "type": "text", "text": "Made x." } })), 2);
    assert_eq!(g.t.tool("t1").unwrap().status, "pending", "not applied until the load ends");
    let fx = g.on_in(&json!({ "jsonrpc": "2.0", "id": 10, "result": {} }), 3);
    assert_eq!(fx, vec![Effect::SessionOpen { fresh: false }]);
    assert_eq!(g.t.tool("t1").unwrap().status, "completed");
    let users = g.t.entries.iter().filter(|e| matches!(e, Entry::User { .. })).count();
    assert_eq!(users, 1);
    assert!(g.t.markdown().contains("Made x."));
}

#[test]
fn unknown_requests_are_refused_and_duplicates_ignored() {
    let mut g = rebuilt();
    let fx = g.on_in(&json!({ "jsonrpc": "2.0", "id": 7, "method": "fs/read_text_file", "params": {} }), 1);
    assert_eq!(fx, vec![Effect::Unsupported(json!(7), "fs/read_text_file".into())]);
    // The same request again (re-read after a crash) isn't a second card.
    let again = &log_lines()[8];
    g.rebuild_line(again);
    assert_eq!(g.pending.len(), 1);
}

#[test]
fn config_keeps_rules_but_not_the_first_prompt() {
    let cfg: Config = serde_json::from_value(json!({
        "agent": "claude", "cwd": "/src", "prompt": "hi", "model": "haiku",
        "allow": [{ "tool": "Bash", "title": "make" }]
    }))
    .unwrap();
    assert_eq!(cfg.prompt.as_deref(), Some("hi"));
    let saved = serde_json::to_value(&cfg).unwrap();
    assert_eq!(
        saved,
        json!({ "agent": "claude", "cwd": "/src", "model": "haiku", "allow": [{ "tool": "Bash", "title": "make" }] })
    );
}

#[test]
fn a_real_node_from_mise_not_its_shims() {
    let root = std::env::temp_dir().join(format!("ilg-node-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for v in ["20.1.0", "22.9.1", "22.23.3", "24.21.0"] {
        let bin = root.join(v).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("node"), "").unwrap();
    }
    // Aliases mise keeps beside the versions are ignored.
    std::fs::create_dir_all(root.join("lts/bin")).unwrap();
    assert_eq!(super::mise_node(&root), Some(root.join("22.23.3/bin")));
    std::fs::remove_dir_all(root.join("22.9.1")).unwrap();
    std::fs::remove_dir_all(root.join("22.23.3")).unwrap();
    assert_eq!(super::mise_node(&root), Some(root.join("24.21.0/bin")));
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_prompt_queued_while_the_agent_is_down_survives_a_restart() {
    let mut inner = Inner::new(Config::default(), None);
    for l in [
        frame("note", json!({ "e": "exit", "why": "exited with code 1" })),
        frame("note", json!({ "e": "queue", "text": "whats this project?", "front": false })),
    ] {
        inner.rebuild_line(&l);
    }
    assert_eq!(inner.queue, [Queued { text: "whats this project?".into(), images: vec![] }]);
    // Sending it takes it off, here as when it's rebuilt.
    inner.rebuild_line(&frame(
        "out",
        json!({ "jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": { "sessionId": "s1", "prompt": [{ "type": "text", "text": "whats this project?" }] } }),
    ));
    assert!(inner.queue.is_empty());
    inner.rebuild_line(&frame("note", json!({ "e": "queue", "text": "later", "front": false })));
    inner.rebuild_line(&frame("note", json!({ "e": "queue_clear" })));
    assert!(inner.queue.is_empty(), "cancel drops what's queued");
}

/// Frames as claude-agent-acp 0.81.2 sent them in S13: the tool call, its
/// questions, then the form.
fn question_lines() -> Vec<Vec<u8>> {
    let u = |update: Value| {
        frame("in", json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "update": update } }))
    };
    let meta = json!({ "claudeCode": { "toolName": "AskUserQuestion" } });
    let mut lines = log_lines()[..6].to_vec();
    lines.extend([
        u(json!({ "_meta": meta, "toolCallId": "toolu_1", "sessionUpdate": "tool_call", "name": "AskUserQuestion", "rawInput": {}, "status": "pending", "title": "Asking for your input", "kind": "other" })),
        u(json!({ "_meta": meta, "toolCallId": "toolu_1", "sessionUpdate": "tool_call_update", "rawInput": { "questions": [
            { "question": "Which colour?", "header": "Colour", "multiSelect": false, "options": [{ "label": "Red", "preview": "R" }, { "label": "Blue" }] }] } })),
        frame(
            "in",
            json!({ "jsonrpc": "2.0", "id": 0, "method": "elicitation/create", "params": { "mode": "form", "sessionId": "s1", "toolCallId": "toolu_1", "message": "Which colour?",
                "requestedSchema": { "type": "object", "properties": { "question_0": { "type": "string", "title": "Colour", "oneOf": [{ "const": "Red", "title": "Red" }, { "const": "Blue", "title": "Blue" }] }, "question_0_custom": { "type": "string", "title": "Other" } } } } }),
        ),
    ]);
    lines
}

#[test]
fn a_question_is_rebuilt_from_the_log_as_a_question_card() {
    let mut g = Inner::new(Config::default(), None);
    for l in question_lines() {
        g.rebuild_line(&l);
    }
    assert_eq!(g.asks.len(), 1);
    let a = &g.asks[0].ask;
    assert_eq!((a.id.as_str(), a.kind, a.tool_call_id.as_deref()), ("0", AskKind::Questions, Some("toolu_1")));
    assert_eq!(a.questions.as_ref().unwrap()[0]["options"][0]["preview"], "R", "from the tool call's input");
    assert_eq!(g.attention(), (Attention::NeedsInput, "Which colour?".into()));
    let md = g.t.markdown();
    assert!(md.contains("**Asked** `Asking for your input` (pending)\n\n- Colour: Which colour? (Red / Blue)"), "{md}");
    // Again (re-read after a crash): still one card.
    g.rebuild_line(&question_lines()[8]);
    assert_eq!(g.asks.len(), 1);
    // Answered: gone.
    g.on_out(
        &json!({ "jsonrpc": "2.0", "id": 0, "result": { "action": "accept", "content": { "question_0": "Red" } } }),
        2,
    );
    assert!(g.asks.is_empty());
    assert_eq!(g.attention().0, Attention::Working);
}

#[test]
fn the_agent_withdraws_its_question_and_links_close_when_complete() {
    let mut g = Inner::new(Config::default(), None);
    for l in question_lines() {
        g.rebuild_line(&l);
    }
    g.on_in(&json!({ "jsonrpc": "2.0", "method": "$/cancel_request", "params": { "requestId": 0 } }), 3);
    assert!(g.asks.is_empty());
    assert!(g.t.markdown().contains("_The question was withdrawn_"));

    // A form without a tool call is a form, whatever its fields.
    let form = json!({ "jsonrpc": "2.0", "id": 1, "method": "elicitation/create", "params": { "mode": "form", "message": "Order",
        "requestedSchema": { "type": "object", "properties": { "size": { "type": "string", "enum": ["S", "M"] } } } } });
    g.on_in(&form, 4);
    assert_eq!(g.asks[0].ask.kind, AskKind::Form);
    // The turn ending takes it with it.
    g.on_in(&json!({ "jsonrpc": "2.0", "id": 3, "result": { "stopReason": "cancelled" } }), 5);
    assert!(g.asks.is_empty());

    // A link: opening it accepts, and it waits for elicitation/complete.
    let url = json!({ "jsonrpc": "2.0", "id": 2, "method": "elicitation/create", "params": { "mode": "url", "message": "Sign in",
        "url": "https://example.com/x", "elicitationId": "e1" } });
    g.on_in(&url, 6);
    assert_eq!(g.attention(), (Attention::NeedsInput, "Sign in".into()));
    g.on_out(&json!({ "jsonrpc": "2.0", "id": 2, "result": { "action": "accept" } }), 7);
    assert!(g.asks[0].ask.accepted);
    assert!(g.open_ask().is_none(), "an opened link doesn't ask for you");
    g.on_in(&json!({ "jsonrpc": "2.0", "method": "elicitation/complete", "params": { "elicitationId": "e1" } }), 8);
    assert!(g.asks.is_empty());
}

/// #505: a running agent server an illogical daemon started (its marker
/// empty) has the token under the old name; one of ours, under both.
#[test]
fn the_token_is_referenced_by_the_name_its_server_has() {
    let dir = std::env::temp_dir().join(format!("arugula-token-env-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(TOKEN_IN_ENV), "").unwrap();
    assert_eq!(token_env(&dir), "ILLOGICAL_MCP_BLOCK_TOKEN");
    std::fs::write(dir.join(TOKEN_IN_ENV), MCP_TOKEN_ENV).unwrap();
    assert_eq!(token_env(&dir), "ARUGULA_MCP_BLOCK_TOKEN");
    std::fs::remove_file(dir.join(TOKEN_IN_ENV)).unwrap();
    assert_eq!(token_env(&dir), "ARUGULA_MCP_BLOCK_TOKEN");
    std::fs::remove_dir_all(&dir).unwrap();
}
