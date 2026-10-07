// `just screenshots`: the images on the project page and in the README,
// made from a throwaway daemon with a demo HOME (a prompt, a small repo, a
// stand-in `cargo`) and a scripted agent, so they can be redone whenever
// the UI changes and never show anything real.

import { type ChildProcess, execFileSync, spawn } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { devices, expect, test, type Page } from "@playwright/test";
import type { PaneId } from "../src/proto";
import { menu, open, paneEl, panes, ready, reset, text } from "../e2e/helpers";
import { labs } from "../e2e/labs";
import "../e2e/local-token";

// SHOTS_PORT moves the daemon (and its block port, the next one up) and
// SHOTS_DEV_PORT the demo dev server, when these are taken.
const PORT = Number(process.env.SHOTS_PORT ?? 7689);
const BLOCK_PORT = PORT + 1;
const DEV_PORT = Number(process.env.SHOTS_DEV_PORT ?? 5173);
const base = `http://127.0.0.1:${PORT}`;
const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "../../site/img");
const agent = join(here, "demo_acp.py");

const codeServer = (() => {
  const dir = join(process.env.HOME ?? "", ".cache/arugula/code-server");
  if (!existsSync(dir)) return null;
  const bin = readdirSync(dir).map((v) => join(dir, v, "bin/code-server")).find(existsSync);
  return bin ?? null;
})();

let root: string;
let home: string;
let state: string;
let daemon: ChildProcess | undefined;

test.use({ baseURL: base });
// xterm's WebGL renderer draws at the wrong size at an emulated 2x pixel
// ratio; the DOM renderer (what phones get, chosen by this query) is right.
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const mm = window.matchMedia.bind(window);
    window.matchMedia = (q: string) =>
      q === "(pointer: coarse)" ? ({ ...mm(q), matches: true, media: q } as MediaQueryList) : mm(q);
  });
});
test.describe.configure({ mode: "serial" });

const SESSION_RS = `use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::token::{Token, TokenError};

/// Sessions by id, each with an expiry. Expired sessions are swept
/// lazily, on the next read, and by \`sweep\` on a timer.
pub struct Store {
    sessions: HashMap<SessionId, Session>,
    ttl: Duration,
    clock: Box<dyn Clock>,
}

impl Store {
    pub fn new(ttl: Duration, clock: impl Clock + 'static) -> Self {
        Self { sessions: HashMap::new(), ttl, clock: Box::new(clock) }
    }

    pub fn create(&mut self, user: UserId) -> (SessionId, Token) {
        let id = SessionId::random();
        let expires = self.clock.now() + self.ttl;
        self.sessions.insert(id, Session { user, expires });
        (id, Token::sign(id, expires))
    }

    pub fn get(&mut self, id: &SessionId) -> Option<&Session> {
        let now = self.clock.now();
        if self.sessions.get(id).is_some_and(|s| s.expires <= now) {
            self.sessions.remove(id);
        }
        self.sessions.get(id)
    }

    pub fn refresh(&mut self, id: &SessionId) -> Result<Token, TokenError> {
        let now = self.clock.now();
        let s = self.sessions.get_mut(id).ok_or(TokenError::Unknown)?;
        s.expires = now + self.ttl;
        Ok(Token::sign(*id, s.expires))
    }
}
`;

const BASHRC = `PS1='\\[\\e]0;\\w\\a\\]\\[\\e[1;32m\\]demo@workstation\\[\\e[0m\\]:\\[\\e[1;34m\\]\\w\\[\\e[0m\\]\\$ '
alias ls='ls --color=auto'
export EDITOR=nvim LESS=-R GIT_PAGER=cat
`;

// A stand-in for cargo: the first test run fails, the rest pass.
const CARGO = `#!/bin/sh
f="$HOME/.cache/demo-cargo-runs"; n=$(cat "$f" 2>/dev/null || echo 0); echo $((n + 1)) > "$f"
g() { printf '\\033[1;32m%12s\\033[0m %s\\n' "$1" "$2"; }
g Compiling "auth v0.4.2 (/home/demo/src/auth)"
sleep 0.4
g Finished "\\\`test\\\` profile [unoptimized + debuginfo] target(s) in 2.31s"
g Running "unittests src/lib.rs"
echo; echo "running 6 tests"
for t in creates_a_session refreshes_before_expiry rejects_a_forged_token expires_after_ttl revokes_on_logout survives_a_restart; do
  if [ "$n" = 0 ] && [ $t = expires_after_ttl ]; then r='\\033[31mFAILED\\033[0m'; else r='\\033[32mok\\033[0m'; fi
  printf "test session::tests::%s ... $r\\n" $t
done
echo
if [ "$n" = 0 ]; then
  echo "---- session::tests::expires_after_ttl stdout ----"
  echo "thread 'session::tests::expires_after_ttl' panicked at src/session.rs:212:9:"
  echo "assertion failed: store.get(&id).is_none()"
  echo; printf 'test result: \\033[31mFAILED\\033[0m. 5 passed; 1 failed; 0 ignored; finished in 0.14s\\n'
  exit 101
fi
printf 'test result: \\033[32mok\\033[0m. 6 passed; 0 failed; 0 ignored; finished in 0.02s\\n'
`;

// A stand-in for npm: `npm run dev` says what Vite would, then serves web/.
const NPM = () => `#!/bin/sh
printf '\\n> auth-admin@0.4.2 dev\\n> vite\\n\\n'
sleep 0.3
printf '  \\033[1;32mVITE\\033[0m \\033[32mv6.3.5\\033[0m  ready in \\033[1m284\\033[0m ms\\n\\n'
printf '  \\033[32m➜\\033[0m  \\033[1mLocal\\033[0m:   \\033[36mhttp://localhost:\\033[1m${DEV_PORT}\\033[0;36m/\\033[0m\\n'
printf '  \\033[2m➜  Network: use --host to expose\\033[0m\\n'
printf '  \\033[2m➜  press h + enter to show help\\033[0m\\n'
exec python3 -m http.server ${DEV_PORT} --bind 127.0.0.1 --directory web >/dev/null 2>&1
`;

// What the dev server serves: the auth crate's admin page.
const ADMIN_HTML = `<!doctype html>
<html><head><meta charset="utf-8"><title>auth admin</title>
<style>
body { margin: 0; font: 14px/1.5 system-ui, sans-serif; background: #f6f7fb; color: #1f2333; }
header { display: flex; align-items: center; gap: 10px; padding: 14px 24px; background: #fff; border-bottom: 1px solid #e3e5ee; }
header b { font-size: 16px; }
header span { margin-left: auto; color: #6b7088; font-size: 13px; }
main { padding: 24px; }
.tiles { display: grid; grid-template-columns: repeat(3, 1fr); gap: 14px; margin-bottom: 22px; }
.tile { background: #fff; border: 1px solid #e3e5ee; border-radius: 10px; padding: 14px 16px; }
.tile small { color: #6b7088; }
.tile div { font-size: 26px; font-weight: 650; }
table { width: 100%; border-collapse: collapse; background: #fff; border: 1px solid #e3e5ee; border-radius: 10px; overflow: hidden; }
th, td { text-align: left; padding: 9px 14px; border-bottom: 1px solid #eef0f5; }
th { color: #6b7088; font-weight: 500; font-size: 12px; text-transform: uppercase; letter-spacing: .04em; }
.ok { color: #1a7f37; } .old { color: #b42318; }
</style></head><body>
<header><b>🔐 auth</b> admin <span>v0.4.2 · dev</span></header>
<main>
<div class="tiles">
<div class="tile"><small>Live sessions</small><div>1,284</div></div>
<div class="tile"><small>Expiring in 5 min</small><div>37</div></div>
<div class="tile"><small>Swept today</small><div>9,912</div></div>
</div>
<table>
<tr><th>Session</th><th>User</th><th>Expires</th><th>State</th></tr>
<tr><td>s_7f3a…c21</td><td>ana</td><td>in 58 min</td><td class="ok">live</td></tr>
<tr><td>s_19be…04d</td><td>sam</td><td>in 41 min</td><td class="ok">live</td></tr>
<tr><td>s_c0d2…9aa</td><td>kim</td><td>in 12 min</td><td class="ok">live</td></tr>
<tr><td>s_44e1…7f0</td><td>lee</td><td>in 3 min</td><td class="ok">live</td></tr>
<tr><td>s_a8b9…e13</td><td>ana</td><td>2 min ago</td><td class="old">expired</td></tr>
<tr><td>s_5d70…b62</td><td>jo</td><td>9 min ago</td><td class="old">expired</td></tr>
</table>
</main></body></html>
`;

// The fix an agent made, for the changes shot: a Clock, and the sweep.
const CLOCK_RS = `use std::time::Instant;

/// Where the session store reads the time, so tests can move it.
pub trait Clock: Send {
    fn now(&self) -> Instant;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}
`;

const SWEEP = `
    /// Drop every expired session; how many went.
    pub fn sweep(&mut self) -> usize {
        let now = self.clock.now();
        let before = self.sessions.len();
        self.sessions.retain(|_, s| s.expires > now);
        before - self.sessions.len()
    }
}
`;

/** A Claude Code session from a terminal in the auth repo, for the
 * conversations shot: in the demo HOME's ~/.claude, as Claude Code writes it. */
function seedConversation(cwd: string) {
  const id = "3b6f0c1e-5a2d-4c8e-9f7a-1d2e3c4b5a69";
  const dir = join(home, ".claude/projects", cwd.replace(/[/.]/g, "-"));
  mkdirSync(dir, { recursive: true });
  let n = 0;
  let parent: string | null = null;
  const lines: object[] = [];
  const t0 = Date.parse("2026-09-28T09:12:00Z");
  const add = (type: string, message: object) => {
    const uuid = `00000000-0000-4000-8000-${String(++n).padStart(12, "0")}`;
    lines.push({
      type, uuid, parentUuid: parent, sessionId: id, cwd, gitBranch: "fix-flaky-expiry", entrypoint: "cli", version: "2.1.288",
      timestamp: new Date(t0 + n * 9000).toISOString(), isSidechain: false, message,
    });
    parent = uuid;
  };
  const model = "claude-opus-5-5";
  const say = (text: string) => add("assistant", { id: `msg_${n}`, role: "assistant", model, content: [{ type: "text", text }] });
  const tool = (name: string, input: object, result: string) => {
    const tid = `toolu_${n}`;
    add("assistant", { id: `msg_${n}`, role: "assistant", model, content: [{ type: "tool_use", id: tid, name, input }] });
    add("user", { role: "user", content: [{ type: "tool_result", tool_use_id: tid, content: result }] });
  };
  add("user", { role: "user", content: "expires_after_ttl fails about one run in five on CI. find out why" });
  say("I'll look at the test and the store's expiry check.");
  tool("Bash", { command: "grep -n 'Instant::now\\|sleep' src/session.rs" }, "18:        let expires = Instant::now() + self.ttl;\n26:        let now = Instant::now();\n204:        std::thread::sleep(ttl + Duration::from_millis(5));");
  tool("Read", { file_path: "src/session.rs", offset: 196, limit: 20 }, "196  #[test]\n197  fn expires_after_ttl() {\n198      let ttl = Duration::from_millis(50);\n...");
  say(
    "Found it. The test sleeps for the TTL plus 5 ms and then expects the session to be gone, but get compares against an Instant::now() taken after the lookup starts. On a loaded CI runner the sleep can return early relative to the store's clock reads, so the session is still live.\n\nThe fix is to stop sleeping: give the store a Clock and have the test advance a fake one past the TTL.",
  );
  add("user", { role: "user", content: "do that, and add a sweep while you're there" });
  tool("Write", { file_path: "src/clock.rs", content: CLOCK_RS }, "File created successfully");
  tool("Edit", { file_path: "src/session.rs", old_string: "Instant::now()", new_string: "self.clock.now()", replace_all: true }, "The file has been updated.");
  tool("Bash", { command: "cargo test -p auth session" }, "running 6 tests\ntest session::tests::expires_after_ttl ... ok\n...\ntest result: ok. 6 passed; 0 failed");
  say("Done: Store takes a Clock, the test uses a fake one, and sweep() drops expired sessions. All 6 tests pass, 50 runs in a row.");
  lines.push({ type: "ai-title", aiTitle: "Fix flaky expires_after_ttl", sessionId: id });
  writeFileSync(join(dir, `${id}.jsonl`), lines.map((l) => JSON.stringify(l)).join("\n") + "\n");
}

function git(cwd: string, ...args: string[]) {
  execFileSync("git", args, {
    cwd,
    env: {
      ...process.env,
      GIT_AUTHOR_NAME: "Demo",
      GIT_AUTHOR_EMAIL: "demo@example.com",
      GIT_COMMITTER_NAME: "Demo",
      GIT_COMMITTER_EMAIL: "demo@example.com",
      GIT_AUTHOR_DATE: "2026-09-28T10:00:00Z",
      GIT_COMMITTER_DATE: "2026-09-28T10:00:00Z",
      HOME: home,
    },
  });
}

function demoHome() {
  home = join(root, "demo");
  const repo = join(home, "src/auth");
  mkdirSync(join(repo, "src"), { recursive: true });
  mkdirSync(join(home, "bin"));
  mkdirSync(join(home, ".cache"));
  writeFileSync(join(home, ".bashrc"), BASHRC);
  writeFileSync(join(home, "bin/cargo"), CARGO);
  chmodSync(join(home, "bin/cargo"), 0o755);
  // The scripted agent, under a name that says what it is.
  writeFileSync(join(home, "bin/demo-agent"), `#!/bin/sh\nexec python3 ${agent}\n`);
  chmodSync(join(home, "bin/demo-agent"), 0o755);
  writeFileSync(join(repo, "Cargo.toml"), '[package]\nname = "auth"\nversion = "0.4.2"\nedition = "2024"\n');
  git(repo, "init", "-q", "-b", "main");
  const commits: [string, string, string][] = [
    ["src/lib.rs", "pub mod session;\npub mod token;\n", "Start the auth crate"],
    ["src/token.rs", "// Signed session tokens.\n", "Signed tokens with an expiry"],
    ["src/session.rs", SESSION_RS.replace(/clock\.now\(\)/g, "Instant::now()"), "A session store with a TTL"],
    ["README.md", "# auth\n", "Document the session lifecycle"],
    ["src/session.rs", SESSION_RS, "Sessions read the time from a Clock"],
  ];
  for (const [file, body, msg] of commits) {
    writeFileSync(join(repo, file), body);
    git(repo, "add", "-A");
    git(repo, "commit", "-q", "-m", msg);
  }
  writeFileSync(join(home, "bin/npm"), NPM());
  chmodSync(join(home, "bin/npm"), 0o755);
  // The admin page, out of git's way (the shots show `git status`).
  mkdirSync(join(repo, "web"));
  writeFileSync(join(repo, "web/index.html"), ADMIN_HTML);
  writeFileSync(join(repo, ".git/info/exclude"), "web/\n");
  git(repo, "checkout", "-q", "-b", "fix-flaky-expiry");
  writeFileSync(join(repo, "src/session.rs"), SESSION_RS + "\n// TODO: sweep on a timer\n");
}

async function startDaemon() {
  const nvim = ["/usr/bin", `${process.env.HOME}/.local/bin`].find((d) => existsSync(join(d, "nvim")));
  daemon = spawn(
    "../target/debug/arugulad",
    [
      ...["--listen", `127.0.0.1:${PORT}`, "--state-dir", labs(state)],
      ...["--shell", "bash", "--no-manager-env", "--tailscale-socket", "/nonexistent/tailscaled.sock"],
      ...["--block-listen", `127.0.0.1:${BLOCK_PORT}`],
      // The pinned code-server, if this machine has it already, rather
      // than a download into the demo HOME.
      ...(codeServer ? ["--code-server", codeServer] : []),
    ],
    {
      stdio: "ignore",
      env: {
        HOME: home,
        SHELL: "/bin/bash",
        USER: "demo",
        LANG: "C.UTF-8",
        TERM: "xterm-256color",
        // No "a newer release is out" chip in the pictures.
        ARUGULA_NO_UPDATE_CHECK: "true",
        // Not this machine's Fountain runner, if it is one (no menu item).
        ARUGULA_FOUNTAIN_UNIT_FILE: join(root, "no-fountain-runner.service"),
        // The browser's cookie and Node's fetch carry this (e2e/local-token.ts).
        ARUGULA_LOCAL_TOKEN_FILE: process.env.ARUGULA_LOCAL_TOKEN_FILE!,
        PATH: [join(home, "bin"), nvim, "/usr/local/bin", "/usr/bin", "/bin"].filter(Boolean).join(":"),
      },
    },
  );
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${base}/api/host`)).ok) return;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("daemon did not start");
}

test.beforeAll(async () => {
  root = mkdtempSync(join(tmpdir(), "arugula-shots-"));
  state = join(root, "state");
  demoHome();
  mkdirSync(out, { recursive: true });
  await startDaemon();
});

test.afterAll(() => {
  daemon?.kill("SIGKILL");
  rmSync(root, { recursive: true, force: true });
});

const intent = (page: Page, i: object) => page.evaluate((i) => window.__arugula.client.intent(i as never), i);

/** Split a pane and return the new one. */
async function split(page: Page, pane: PaneId, edge: "right" | "bottom"): Promise<PaneId> {
  const before = await panes(page);
  await intent(page, { op: "split", pane, edge });
  await expect.poll(async () => (await panes(page)).length).toBe(before.length + 1);
  const id = (await panes(page)).find((p) => !before.includes(p))!;
  await ready(page, id);
  return id;
}

/** Type a line into a pane's shell. */
async function line(page: Page, pane: PaneId, s: string) {
  await paneEl(page, pane).click({ position: { x: 60, y: 60 } });
  await page.keyboard.type(`${s}\n`, { delay: 4 });
}

async function prompted(page: Page, pane: PaneId, n: number) {
  await expect.poll(async () => ((await text(page, pane)).match(/demo@workstation/g) ?? []).length).toBeGreaterThanOrEqual(n);
}

const session = (page: Page) => page.evaluate(() => window.__arugula.client.session!);
const tabId = (page: Page) => page.evaluate(() => window.__arugula.client.tabView()!.id);

const tabIds = (page: Page) =>
  page.evaluate(() => {
    const c = window.__arugula.client;
    return c.state!.sessions.find((s) => s.id === c.session)!.tabs as number[];
  });

/** A new tab, named, shown; returns its id. */
async function newTab(page: Page, name: string): Promise<number> {
  const before = await tabIds(page);
  await intent(page, { op: "new_tab", session: await session(page), from_pane: null });
  await expect.poll(async () => (await tabIds(page)).length).toBe(before.length + 1);
  const id = (await tabIds(page)).find((t) => !before.includes(t))!;
  await intent(page, { op: "rename_tab", tab: id, name });
  await expect.poll(() => tabId(page)).toBe(id);
  return id;
}

let editor: PaneId;
let tests: PaneId;
let gitPane: PaneId;
let authTab: number;

test.describe("desktop", () => {
  test.use({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });

  test("tabs and splits", async ({ page }) => {
    await reset(page);
    [editor] = await panes(page);
    await prompted(page, editor, 1);
    await intent(page, { op: "rename_session", session: await session(page), name: "work" });
    authTab = await tabId(page);
    await intent(page, { op: "rename_tab", tab: authTab, name: "auth" });

    tests = await split(page, editor, "right");
    gitPane = await split(page, tests, "bottom");
    await prompted(page, tests, 1);
    await line(page, tests, "cd ~/src/auth && cargo test -p auth session");
    await prompted(page, tests, 2);
    await line(page, tests, "cargo test -p auth session");
    await prompted(page, tests, 3);
    await prompted(page, gitPane, 1);
    await line(page, gitPane, "cd ~/src/auth && git log --oneline --graph --decorate --color && git status -sb");
    await prompted(page, gitPane, 2);
    await line(page, editor, "cd ~/src/auth && nvim src/session.rs");
    await expect.poll(() => text(page, editor)).toContain("pub struct Store");

    // Two more tabs, then back to the first.
    for (const name of ["web", "notes"]) await newTab(page, name);
    await page.locator(`[data-tab-id="${authTab}"]`).click();
    await expect.poll(() => tabId(page)).toBe(authTab);
    await page.mouse.move(0, 0);
    await page.waitForTimeout(500);
    await page.screenshot({ path: join(out, "desktop.png") });

    // The pane menu.
    await paneEl(page, tests).click({ button: "right", position: { x: 220, y: 120 } });
    await expect(page.getByRole("menuitem").first()).toBeVisible();
    await page.waitForTimeout(300);
    await page.screenshot({ path: join(out, "menu.png") });
    await page.keyboard.press("Escape");
  });

  test("panes come back with their scrollback", async ({ page }) => {
    daemon?.kill("SIGKILL");
    await new Promise((r) => daemon!.once("exit", r));
    await startDaemon();
    await open(page);
    await page.locator(`[data-tab-id="${authTab}"]`).click();
    await expect.poll(() => text(page, tests)).toContain("restored");
    await expect.poll(() => text(page, gitPane)).toContain("restored");
    await page.mouse.move(0, 0);
    await page.waitForTimeout(800);
    await page.screenshot({ path: join(out, "restored.png") });
  });

  test("an agent beside its terminal", async ({ page }) => {
    await open(page);
    // A tab of its own: a shell, and the agent beside it.
    await newTab(page, "agent");
    const [shell] = await panes(page);
    await ready(page, shell);
    await prompted(page, shell, 1);
    await line(page, shell, "cd ~/src/auth && git diff --stat && git status -sb");
    await prompted(page, shell, 2);
    await paneEl(page, shell).click({ button: "right", position: { x: 220, y: 120 } });
    await page.getByRole("menuitem", { name: "Start an agent…" }).click();
    const dialog = page.getByRole("dialog", { name: "Start an agent" });
    await dialog.locator("select[name=agent]").selectOption("acp");
    await dialog.locator("input[name=acp]").fill("demo-agent");
    await dialog.locator("textarea[name=prompt]").fill("fix the flaky session test");
    await dialog.getByRole("button", { name: "Start" }).click();
    const id = await page.evaluate(async () => {
      for (;;) {
        const a = window.__arugula.client.state!.panes.find((p) => p.type === "agent");
        if (a) return a.id;
        await new Promise((r) => setTimeout(r, 50));
      }
    });
    const block = paneEl(page, id);
    await expect(block.getByRole("button", { name: "Approve" })).toBeVisible();
    await expect(block.locator(".agent-tool .xterm-rows").first()).toContainText("expires_after_ttl");
    await page.mouse.move(0, 0);
    await page.waitForTimeout(500);
    await page.screenshot({ path: join(out, "agent.png") });

    await block.getByRole("button", { name: "Approve" }).click();
    await expect(block.locator(".agent-msg").last()).toContainText("Committed");
    await block.locator(".agent-composer textarea").fill("and the other sleeps?");
    await block.getByRole("button", { name: "Send" }).click();
    await expect(block.getByText("Inject the clock everywhere")).toBeVisible();
    await page.mouse.move(0, 0);
    await page.waitForTimeout(500);
    await block.screenshot({ path: join(out, "question.png") });
  });
});

test.describe("blocks", () => {
  test.use({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });

  const openBlock = (page: Page, body: object) =>
    page.evaluate(async (b) => (await window.__arugula.client.openBlock(b as never))!, body);
  const repo = () => join(home, "src/auth");
  const settle = async (page: Page, ms = 800) => {
    await page.mouse.move(0, 0);
    await page.waitForTimeout(ms);
  };

  test("what an agent changed, beside its terminal", async ({ page }) => {
    await open(page);
    await newTab(page, "review");
    const [shell] = await panes(page);
    await ready(page, shell);
    await prompted(page, shell, 1);
    // The agent's fix: a Clock, the store reading it, and a sweep.
    writeFileSync(join(repo(), "src/clock.rs"), CLOCK_RS);
    writeFileSync(join(repo(), "src/lib.rs"), "pub mod clock;\npub mod session;\npub mod token;\n");
    writeFileSync(
      join(repo(), "src/session.rs"),
      SESSION_RS.replace("use crate::token", "use crate::clock::Clock;\nuse crate::token").replace(/\n}\n$/, "\n" + SWEEP),
    );
    await line(page, shell, "cd ~/src/auth && cargo test -p auth session");
    await prompted(page, shell, 2);
    const diff = await openBlock(page, { type: "diff", config: {}, from_pane: shell, split: shell });
    const view = page.locator(`[data-diff="${diff}"]`);
    const row = view.locator('.diff-file[data-file="src/session.rs"]');
    await expect(row).toBeVisible({ timeout: 30_000 });
    await row.locator(".diff-file-head").click();
    const at = row.locator(".dl.add", { hasText: "pub fn sweep" });
    await expect(at).toBeVisible();
    await at.click();
    const file = await page.evaluate(async () => {
      for (;;) {
        const f = window.__arugula.client.state!.panes.find((p) => p.type === "file");
        if (f) return f.id;
        await new Promise((r) => setTimeout(r, 50));
      }
    });
    await expect(page.locator(`[data-file-block="${file}"] .cm-mark-line`)).toContainText("pub fn sweep", { timeout: 30_000 });
    await settle(page);
    await page.screenshot({ path: join(out, "changes.png") });
  });

  test("a dev server beside its terminal", async ({ page }) => {
    await open(page);
    await newTab(page, "admin");
    const [shell] = await panes(page);
    await ready(page, shell);
    await prompted(page, shell, 1);
    await line(page, shell, "cd ~/src/auth && npm run dev");
    await expect.poll(() => text(page, shell)).toContain("ready in");
    for (let i = 0; i < 50; i++) {
      try {
        if ((await fetch(`http://127.0.0.1:${DEV_PORT}/`)).ok) break;
      } catch {
        // not up yet
      }
      await page.waitForTimeout(100);
    }
    const block = await openBlock(page, { type: "browser", config: { port: DEV_PORT, path: "/" }, split: shell });
    const frame = paneEl(page, block).frameLocator("iframe");
    await expect(frame.getByText("Live sessions")).toBeVisible({ timeout: 30_000 });
    await settle(page);
    await page.screenshot({ path: join(out, "port.png") });
  });

  test("VS Code where the pane runs", async ({ page }) => {
    test.skip(!codeServer, "code-server isn't in ~/.cache/arugula yet");
    test.setTimeout(180_000);
    await open(page);
    await newTab(page, "edit");
    const [shell] = await panes(page);
    await ready(page, shell);
    await prompted(page, shell, 1);
    await line(page, shell, "cd ~/src/auth && cargo test -p auth session");
    await prompted(page, shell, 2);
    const block = await openBlock(page, {
      type: "editor",
      config: { path: join(repo(), "src/session.rs"), line: 47 },
      from_pane: shell,
      split: shell,
    });
    const frame = paneEl(page, block).frameLocator("iframe");
    await expect(frame.locator(".monaco-editor .view-lines").first()).toContainText("pub fn sweep", { timeout: 120_000 });
    await settle(page, 2500);
    await page.screenshot({ path: join(out, "editor.png") });
  });

  test("a Claude Code conversation from a terminal", async ({ page }) => {
    seedConversation(repo());
    await open(page);
    await newTab(page, "claude");
    const [shell] = await panes(page);
    await ready(page, shell);
    await prompted(page, shell, 1);
    await line(page, shell, "cd ~/src/auth && git log --oneline -3 && git status -sb");
    await prompted(page, shell, 2);
    await menu(page, paneEl(page, shell), "Claude Code conversations…");
    const dialog = page.getByRole("dialog", { name: "Claude Code conversations" });
    await expect(dialog.getByText("Fix flaky expires_after_ttl")).toBeVisible();
    const agents = await page.evaluate(() => window.__arugula.client.state!.panes.filter((p) => p.type === "agent").map((p) => p.id));
    await dialog.getByText("Fix flaky expires_after_ttl").click();
    await expect(dialog).toBeHidden();
    const block = await page.evaluate(async (old) => {
      for (;;) {
        const a = window.__arugula.client.state!.panes.find((p) => p.type === "agent" && !old.includes(p.id));
        if (a) return a.id;
        await new Promise((r) => setTimeout(r, 50));
      }
    }, agents);
    await expect(paneEl(page, block).locator(".agent-msg").last()).toContainText("50 runs in a row");
    await settle(page);
    await page.screenshot({ path: join(out, "conversation.png") });
  });
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  test("one pane at a time, and what needs you", async ({ page }) => {
    await open(page);
    await page.evaluate((p) => window.__arugula.client.setActive(p), tests);
    await page.waitForTimeout(800);
    await page.screenshot({ path: join(out, "phone-terminal.png") });
    // The agent's question, answered with a thumb.
    await page.evaluate(() => {
      const c = window.__arugula.client;
      c.setActive(c.state!.panes.find((p) => p.type === "agent")!.id);
    });
    await expect(page.getByText("Inject the clock everywhere")).toBeVisible();
    await page.waitForTimeout(800);
    await page.screenshot({ path: join(out, "phone.png") });
    await page.evaluate((p) => window.__arugula.client.setActive(p), tests);
    await page.locator(".sheet-button").click();
    await expect(page.locator(".needs-you")).toContainText("Needs you");
    await page.waitForTimeout(500);
    await page.screenshot({ path: join(out, "phone-sheet.png") });
  });
});
