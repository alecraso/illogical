//! M56 (#219): illogicald runs panes on Windows. The real binary, its
//! named pipe and its TCP port, and PowerShell on a pseudoconsole.

#![cfg(windows)]

use std::time::Duration;

use illogical_testkit::illogicald;
use serde_json::json;

fn capture(d: &illogical_testkit::Daemon, pane: u64) -> String {
    d.get(&format!("/api/panes/{pane}/capture")).as_str().unwrap_or_default().to_owned()
}

#[test]
fn a_command_runs_in_powershell_with_its_output_and_exit_code() {
    let d = illogicald!("win-run").start();
    let sock = std::fs::read_to_string(d.state.join("sock.path")).unwrap();
    assert!(sock.starts_with(r"\\.\pipe\illogical-"), "{sock}");
    let pane =
        d.post("/api/run", json!({"command": "Write-Output ('from' + 'windows'); exit 4"}))["pane"].as_u64().unwrap();
    let waited = d.get(&format!("/api/panes/{pane}/wait?until=exit&timeout=30"));
    assert_eq!(waited["code"], 4, "{waited}");
    assert!(capture(&d, pane).contains("fromwindows"));
}

#[test]
fn a_cmdlet_that_fails_exits_1_and_a_native_programs_code_is_kept() {
    let d = illogicald!("win-codes").start();
    let failed = d.post("/api/run", json!({"command": "Get-Item C:\\no\\such\\path"}))["pane"].as_u64().unwrap();
    assert_eq!(d.get(&format!("/api/panes/{failed}/wait?until=exit&timeout=30"))["code"], 1);
    let native = d.post("/api/run", json!({"command": "cmd /c exit 9"}))["pane"].as_u64().unwrap();
    assert_eq!(d.get(&format!("/api/panes/{native}/wait?until=exit&timeout=30"))["code"], 9);
}

#[test]
fn an_interactive_pane_takes_input_and_splits_and_closes() {
    let d = illogicald!("win-shell").start();
    let pane = d.post("/api/run", json!({}))["pane"].as_u64().unwrap();
    d.wait_for("the prompt", || capture(&d, pane).contains("PS "));
    d.post(&format!("/api/panes/{pane}/send"), json!({"text": "'ab' + 'cd'\r"}));
    d.wait_for("the answer", || capture(&d, pane).contains("abcd"));

    let split = d.post("/api/run", json!({"split": pane}))["pane"].as_u64().unwrap();
    d.wait_for("the split's prompt", || capture(&d, split).contains("PS "));
    let ids = |d: &illogical_testkit::Daemon| -> Vec<u64> {
        let panes = d.get("/api/panes");
        let list = panes.as_array().cloned().or_else(|| panes["panes"].as_array().cloned()).unwrap_or_default();
        list.iter().filter_map(|p| p["id"].as_u64()).collect()
    };
    assert!(ids(&d).contains(&split));
    d.post(&format!("/api/panes/{split}/close"), json!({}));
    d.wait_for("the split to close", || !ids(&d).contains(&split));
    assert!(ids(&d).contains(&pane));
    std::thread::sleep(Duration::from_millis(100));
}
