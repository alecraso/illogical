// M31: one layout, the TUI and the browser. `illogical tui` runs in a
// detached tmux session (the terminal it draws in), driven with send-keys and
// read with capture-pane; the browser attaches to the same daemon. Splits,
// typing, renames and closes made on either side show on the other.

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { menu, open, paneEl, panes, ready, tab, text, type, closeContexts } from "./helpers";
import { ANY, daemonPort } from "./ports";

test.afterAll(closeContexts);

let base = "";
const TMUX = ["-L", `illogical-e2e-tui-${process.pid}`];
let daemon: ChildProcess;
let state: string;

const hasTmux = (() => {
  try {
    execFileSync("tmux", ["-V"]);
    return true;
  } catch {
    return false;
  }
})();

test.use({ baseURL: async ({}, use) => use(base) });
test.describe.configure({ mode: "serial" });
test.skip(!hasTmux, "needs tmux to give the TUI a terminal");

test.beforeAll(async () => {
  state = mkdtempSync(join(tmpdir(), "illogical-e2e-tui-"));
  daemon = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", ANY, "--state-dir", state],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/tailscaled.sock"],
    ],
    { stdio: "ignore", env: { ...process.env, PS1: "$ " } },
  );
  base = `http://127.0.0.1:${await daemonPort(state, daemon)}`;
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${base}/api/host`)).ok) break;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  const sock = (() => {
    try {
      return readFileSync(join(state, "sock.path"), "utf8").trim();
    } catch {
      return join(state, "sock");
    }
  })();
  tmux("new-session", "-d", "-s", "tui", "-x", "160", "-y", "45", `TERM=xterm-256color ../target/debug/illogical --socket ${sock} tui`);
  await expect.poll(() => screen()).toContain("illogical");
});

test.afterAll(() => {
  try {
    tmux("kill-server");
  } catch {
    // gone already
  }
  daemon?.kill("SIGKILL");
  rmSync(state, { recursive: true, force: true });
});

function tmux(...args: string[]): string {
  return execFileSync("tmux", [...TMUX, ...args], { encoding: "utf8" });
}

/** What the TUI shows. */
const screen = () => tmux("capture-pane", "-p", "-t", "tui");
/** Ctrl-] then a key: a TUI command. */
const command = (key: string) => {
  tmux("send-keys", "-t", "tui", "C-]");
  tmux("send-keys", "-t", "tui", "-l", key);
};
const typeTui = (s: string) => {
  tmux("send-keys", "-t", "tui", "-l", s);
  tmux("send-keys", "-t", "tui", "Enter");
};
/** Rows of the TUI's screen that are a horizontal divider. */
const hDividers = () => screen().split("\n").filter((l) => l.includes("─────")).length;

test("a split in the TUI shows in the browser, and one in the browser shows in the TUI", async ({ page }) => {
  await open(page);
  await expect.poll(() => panes(page)).toHaveLength(1);
  const first = (await panes(page))[0];
  await ready(page, first);

  command("v");
  await expect.poll(() => panes(page)).toHaveLength(2);
  const second = (await panes(page)).find((p) => p !== first)!;
  // The TUI focuses its new pane: typing lands there, and the browser sees it.
  typeTui("echo from-tui-$((6*7))");
  await ready(page, second);
  await expect.poll(() => text(page, second)).toContain("from-tui-42");

  await menu(page, paneEl(page, first), "Split down");
  await expect.poll(() => panes(page)).toHaveLength(3);
  await expect.poll(hDividers).toBeGreaterThan(0);
  const third = (await panes(page)).find((p) => p !== first && p !== second)!;
  await ready(page, third);
  await type(page, third, "echo from-web-$((5*5))\n");
  await expect.poll(screen).toContain("from-web-25");
});

test("a rename and a close in one show in the other", async ({ page }) => {
  await open(page);
  command("r");
  tmux("send-keys", "-t", "tui", "C-u");
  typeTui("both-ways");
  await expect.poll(async () => (await tab(page)).name).toBe("both-ways");

  const before = await panes(page);
  // The one below another: closing it leaves no horizontal divider.
  const lower = (await tab(page)).layout.panes.find(([, r]) => r.y > 0)![0];
  await menu(page, paneEl(page, lower), "Close pane");
  await expect.poll(() => panes(page)).toHaveLength(before.length - 1);
  await expect.poll(hDividers).toBe(0);
  // And the TUI's own close.
  command("x");
  await expect.poll(() => panes(page)).toHaveLength(before.length - 2);
});
