// `just screenshots`: the images on the project page and in the README,
// made from a throwaway daemon with a demo HOME (a prompt, a small repo, a
// stand-in `cargo`) and a scripted agent, so they can be redone whenever
// the UI changes and never show anything real.

import { type ChildProcess, execFileSync, spawn } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { devices, expect, test, type Page } from "@playwright/test";
import type { PaneId } from "../src/proto";
import { open, paneEl, panes, ready, reset, text } from "../e2e/helpers";

const PORT = 7689;
const base = `http://127.0.0.1:${PORT}`;
const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "../../site/img");
const agent = join(here, "demo_acp.py");

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
  git(repo, "checkout", "-q", "-b", "fix-flaky-expiry");
  writeFileSync(join(repo, "src/session.rs"), SESSION_RS + "\n// TODO: sweep on a timer\n");
}

async function startDaemon() {
  const nvim = ["/usr/bin", `${process.env.HOME}/.local/bin`].find((d) => existsSync(join(d, "nvim")));
  daemon = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", `127.0.0.1:${PORT}`, "--state-dir", state],
      ...["--shell", "bash", "--no-manager-env", "--tailscale-socket", "/nonexistent/tailscaled.sock"],
    ],
    {
      stdio: "ignore",
      env: {
        HOME: home,
        SHELL: "/bin/bash",
        USER: "demo",
        LANG: "C.UTF-8",
        TERM: "xterm-256color",
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
  root = mkdtempSync(join(tmpdir(), "illogical-shots-"));
  state = join(root, "state");
  demoHome();
  mkdirSync(out, { recursive: true });
  await startDaemon();
});

test.afterAll(() => {
  daemon?.kill("SIGKILL");
  rmSync(root, { recursive: true, force: true });
});

const intent = (page: Page, i: object) => page.evaluate((i) => window.__illogical.client.intent(i as never), i);

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

const session = (page: Page) => page.evaluate(() => window.__illogical.client.session!);
const tabId = (page: Page) => page.evaluate(() => window.__illogical.client.tabView()!.id);

const tabIds = (page: Page) =>
  page.evaluate(() => {
    const c = window.__illogical.client;
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
        const a = window.__illogical.client.state!.panes.find((p) => p.type === "agent");
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
    await page.waitForTimeout(500);
    await block.screenshot({ path: join(out, "question.png") });
  });
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  test("one pane at a time, and what needs you", async ({ page }) => {
    await open(page);
    await page.evaluate((p) => window.__illogical.client.setActive(p), tests);
    await page.waitForTimeout(800);
    await page.screenshot({ path: join(out, "phone-terminal.png") });
    // The agent's question, answered with a thumb.
    await page.evaluate(() => {
      const c = window.__illogical.client;
      c.setActive(c.state!.panes.find((p) => p.type === "agent")!.id);
    });
    await expect(page.getByText("Inject the clock everywhere")).toBeVisible();
    await page.waitForTimeout(800);
    await page.screenshot({ path: join(out, "phone.png") });
    await page.evaluate((p) => window.__illogical.client.setActive(p), tests);
    await page.locator(".sheet-button").click();
    await expect(page.locator(".needs-you")).toContainText("Needs you");
    await page.waitForTimeout(500);
    await page.screenshot({ path: join(out, "phone-sheet.png") });
  });
});
