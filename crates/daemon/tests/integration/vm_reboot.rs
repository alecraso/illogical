//! VM tabs across a real wisp reboot (#256, #214 section 4). Two VM tabs,
//! one split so two panes share its machine. With the daemon stopped (a
//! host reboot takes it down too), both machines reboot: wispd's operator
//! endpoints suspend each sprite and drop its memory snapshot ("cool"),
//! which is what `--warm-ttl` does, so the next wake is a real boot
//! (`web/e2e/resident.spec.ts` does the same). Then the daemon starts and:
//!
//! - every pane says its session was lost and gets a shell again, on its
//!   tab's machine, under a new boot id with its home directory kept;
//! - each tab still has exactly one machine, on the sprite it had;
//! - wisp has no other `illogical-eph-<this daemon>-*` sprite, and none
//!   once the tabs are closed.
//!
//! Skips without a wisp token on this host (ARUGULA_WISP_TOKEN_FILE or
//! ~/.local/share/wisp/token), like `machines.rs`. With a token, wispd must
//! answer at 127.0.0.1:7788.

// VM tabs are Linux's (wisp).
#![cfg(unix)]

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use arugula_testkit::{arugulad, wait_for};
use serde_json::{Value, json};

const WISP: &str = "http://127.0.0.1:7788";

/// A boot takes a few seconds; the daemon retries a while first.
const BOOT: Duration = Duration::from_secs(180);

fn token() -> Option<String> {
    let file = std::env::var_os("ARUGULA_WISP_TOKEN_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap()).join(".local/share/wisp/token"));
    std::fs::read_to_string(file).ok().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

/// The names of wisp's sprites starting with `prefix`, if wispd answered.
fn try_sprites(token: &str, prefix: &str) -> Option<Vec<String>> {
    let out = Command::new("curl")
        .args(["-s", "-f", "-H", &format!("Authorization: Bearer {token}")])
        .arg(format!("{WISP}/v1/sprites?prefix={prefix}"))
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let list: Value = serde_json::from_slice(&out.stdout).ok()?;
    let mut names: Vec<String> =
        list["sprites"].as_array()?.iter().filter_map(|s| s["name"].as_str().map(str::to_owned)).collect();
    names.sort();
    Some(names)
}

fn sprites(token: &str, prefix: &str) -> Vec<String> {
    try_sprites(token, prefix).unwrap_or_else(|| panic!("no sprite list from wispd at {WISP}"))
}

fn status(token: &str, sprite: &str) -> String {
    let out = Command::new("curl")
        .args(["-s", "-H", &format!("Authorization: Bearer {token}")])
        .arg(format!("{WISP}/v1/sprites/{sprite}"))
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    v["status"].as_str().unwrap_or_default().to_owned()
}

/// A session cookie for wispd's operator endpoints (its web UI's). The
/// token goes on stdin, not the command line.
fn ui_login(token: &str) -> String {
    let mut c = Command::new("curl")
        .args(["-s", "-o", "/dev/null", "-D", "-", "-X", "POST", "-H", "Content-Type: application/json"])
        .args(["--data-binary", "@-"])
        .arg(format!("{WISP}/ui/login"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(json!({ "token": token }).to_string().as_bytes()).unwrap();
    let out = c.wait_with_output().unwrap();
    let headers = String::from_utf8_lossy(&out.stdout);
    assert!(headers.lines().next().is_some_and(|l| l.contains(" 204")), "wispd's /ui/login: {headers}");
    headers
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("set-cookie").then(|| v.trim().split(';').next().unwrap().to_owned())
        })
        .expect("a session cookie from /ui/login")
}

/// What wisp's warm TTL does, now: suspend, then drop the memory snapshot,
/// so the next wake boots the guest.
fn reboot(token: &str, cookie: &str, sprite: &str) {
    for what in ["suspend", "cool"] {
        let out = Command::new("curl")
            .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST"])
            .args(["-H", &format!("Cookie: {cookie}"), "-H", "X-Wisp-UI: 1"])
            // sandpitd, which took over wispd, names the header its own way.
            .args(["-H", "X-Sandpit-UI: 1"])
            .arg(format!("{WISP}/ui/api/sprites/{sprite}/{what}"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "200", "{what} {sprite}");
    }
    assert_eq!(status(token, sprite), "cold", "{sprite} after cool");
}

/// A testkit daemon that deletes the machines it left behind.
struct Daemon(arugula_testkit::Daemon);

impl std::ops::Deref for Daemon {
    type Target = arugula_testkit::Daemon;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Daemon {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.0.halt();
        // A failed test leaves its machines behind: delete this daemon's.
        if let (Some(token), Some(prefix)) = (token(), self.prefix()) {
            for n in try_sprites(&token, &prefix).unwrap_or_default() {
                let _ = Command::new("curl")
                    .args(["-s", "-o", "/dev/null", "-X", "DELETE", "-H", &format!("Authorization: Bearer {token}")])
                    .arg(format!("{WISP}/v1/sprites/{n}"))
                    .status();
            }
        }
    }
}

impl Daemon {
    fn new() -> Self {
        Self(arugulad!("vmreboot").args(["--wisp-url", WISP]).wait_secs(20).start())
    }

    /// The names this daemon gives its machines' sprites.
    fn prefix(&self) -> Option<String> {
        let id = std::fs::read_to_string(self.state.join("daemon-id")).ok()?;
        Some(format!("illogical-eph-{}-", id.trim()))
    }

    fn log(&self, pane: u64) -> String {
        log_text(&self.state.join("blocks").join(pane.to_string()))
    }

    fn pane(&self, pane: u64) -> Value {
        self.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).cloned().unwrap_or_default()
    }

    fn send(&self, pane: u64, text: &str) {
        self.post(&format!("/api/panes/{pane}/send"), json!({ "text": text, "enter": true }));
    }

    /// The sprite of a pane's machine.
    fn sprite_of(&self, pane: u64) -> String {
        let host = self.pane(pane)["host"].clone();
        let machines = self.get("/api/machines");
        let m = machines.as_array().unwrap().iter().find(|m| m["id"] == host).cloned();
        m.unwrap_or_else(|| panic!("%{pane}'s machine in {machines}"))["sprite"].as_str().unwrap().to_owned()
    }
}

fn log_text(dir: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(dir) else { return String::new() };
    let mut segs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    segs.retain(|p| p.file_name().unwrap().to_string_lossy().starts_with("seg-"));
    segs.sort();
    segs.iter().map(|p| String::from_utf8_lossy(&std::fs::read(p).unwrap()).into_owned()).collect()
}

#[test]
fn vm_tabs_come_back_on_their_machines_after_a_real_reboot() {
    let Some(token) = token() else {
        eprintln!("SKIP: no wisp token on this host (ARUGULA_WISP_TOKEN_FILE or ~/.local/share/wisp/token)");
        return;
    };
    assert!(
        try_sprites(&token, "illogical-eph-").is_some(),
        "wispd doesn't answer at {WISP} with this host's token (the token file is there)"
    );
    let mut d = Daemon::new();
    let prefix = d.prefix().expect("the daemon's id");

    // Tab A with a split on its machine, tab B on its own.
    let a1 = d.post("/api/run", json!({ "vm_tab": true }))["pane"].as_u64().unwrap();
    let a2 = d.post("/api/run", json!({ "split": a1, "join": true }))["pane"].as_u64().unwrap();
    let b1 = d.post("/api/run", json!({ "vm_tab": true }))["pane"].as_u64().unwrap();
    let panes = [a1, a2, b1];
    for p in panes {
        wait_for(&format!("%{p}'s guest prompt"), BOOT, || d.pane(p)["cwd"] == "/home/sprite");
    }
    let (sa, sb) = (d.sprite_of(a1), d.sprite_of(b1));
    assert_eq!(d.sprite_of(a2), sa, "the split shares its tab's machine");
    assert_ne!(sa, sb);
    let machines = d.get("/api/machines");
    assert_eq!(machines.as_array().unwrap().len(), 2, "{machines}");
    assert!(machines.as_array().unwrap().iter().all(|m| m["owner"].get("tab").is_some()), "{machines}");
    let mut ours = vec![sa.clone(), sb.clone()];
    ours.sort();
    assert_eq!(sprites(&token, &prefix), ours);

    // What the reboot should change (the boot id) and keep (the disk).
    for p in [a1, b1] {
        d.send(p, "cat /proc/sys/kernel/random/boot_id > ~/boot-before && echo SAVED-$((6*7))");
        wait_for("the boot id saved", Duration::from_secs(30), || d.log(p).contains("SAVED-42"));
    }

    d.stop();
    let cookie = ui_login(&token);
    reboot(&token, &cookie, &sa);
    reboot(&token, &cookie, &sb);
    d.start();

    // Each pane finds its session gone and starts a shell again by its
    // policy, on the same machine, which boots.
    for p in panes {
        wait_for(&format!("%{p} to see its session lost"), BOOT, || {
            d.log(p).contains("the session on the machine was lost")
        });
    }
    let check = "[ -s ~/boot-before ] && [ \"$(cat /proc/sys/kernel/random/boot_id)\" != \"$(cat ~/boot-before)\" ] \
                 && echo REBOOTED-$((6*7))";
    for p in panes {
        wait_for(&format!("%{p}'s shell"), BOOT, || d.pane(p)["running"] == true);
        d.send(p, check);
    }
    for p in panes {
        wait_for(&format!("%{p} on a rebooted machine with its disk"), BOOT, || {
            d.log(p).matches("REBOOTED-42").count() == 1
        });
    }

    // One machine per tab, the sprite it had, nothing else of ours on wisp.
    assert_eq!((d.sprite_of(a1), d.sprite_of(a2), d.sprite_of(b1)), (sa.clone(), sa.clone(), sb.clone()));
    let machines = d.get("/api/machines");
    assert_eq!(machines.as_array().unwrap().len(), 2, "{machines}");
    assert_eq!(sprites(&token, &prefix), ours);

    // Closing the tabs takes their machines.
    for p in panes {
        d.post(&format!("/api/panes/{p}/close"), json!({}));
    }
    wait_for("the machines to go", Duration::from_secs(60), || sprites(&token, &prefix).is_empty());
    assert_eq!(d.get("/api/machines"), json!([]));
}
