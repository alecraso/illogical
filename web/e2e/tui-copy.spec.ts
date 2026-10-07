// M32: copy mode in the TUI. `arugula tui` runs in a detached tmux
// session with `set-clipboard on`, so what the TUI copies with OSC 52 lands
// in tmux's paste buffer, as it would in the laptop's clipboard over ssh.
// The mouse is driven with SGR mouse reports typed into the TUI.

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { ANY, daemonPort } from "./ports";
import { labs } from "./labs";

let base = "";
const TMUX = ["-L", `arugula-e2e-tui-copy-${process.pid}`];
// The TUI's sidebar and its divider: the pane starts at this column.
const LEFT = 27;
let daemon: ChildProcess;
let state: string;
let sock: string;

const hasTmux = (() => {
  try {
    execFileSync("tmux", ["-V"]);
    return true;
  } catch {
    return false;
  }
})();

test.describe.configure({ mode: "serial" });
test.skip(!hasTmux, "needs tmux to give the TUI a terminal");

test.beforeAll(async () => {
  state = mkdtempSync(join(tmpdir(), "arugula-e2e-tui-copy-"));
  daemon = spawn(
    "../target/debug/arugulad",
    [
      ...["--listen", ANY, "--state-dir", labs(state)],
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
  sock = (() => {
    try {
      return readFileSync(join(state, "sock.path"), "utf8").trim();
    } catch {
      return join(state, "sock");
    }
  })();
  tmux("new-session", "-d", "-s", "tui", "-x", "120", "-y", "30", `TERM=xterm-256color ../target/debug/arugula --socket ${sock} tui`);
  tmux("set", "-s", "set-clipboard", "on");
  await expect.poll(() => screen()).toContain("Arugula");
  await expect.poll(() => screen()).toMatch(/│\$/);
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

const screen = () => tmux("capture-pane", "-p", "-t", "tui");
/** What the TUI last put on the clipboard. */
const clipboard = () => {
  try {
    return execFileSync("tmux", [...TMUX, "show-buffer"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
  } catch {
    return "";
  }
};
const keys = (s: string) => tmux("send-keys", "-t", "tui", "-l", s);
const typeTui = (s: string) => {
  keys(s);
  tmux("send-keys", "-t", "tui", "Enter");
};
const command = (key: string) => {
  tmux("send-keys", "-t", "tui", "C-]");
  keys(key);
};
/** An SGR mouse report at the pane's column `x`, screen row `y` (from 0). */
const mouse = (button: number, x: number, y: number, release = false) =>
  keys(`\x1b[<${button};${LEFT + x + 1};${y + 1}${release ? "m" : "M"}`);
/** The screen row (from 0) showing `text` in the pane. */
const rowOf = (text: string) => screen().split("\n").findIndex((l) => l.includes(`│${text}`));
const clear = async () => {
  typeTui("clear");
  await expect.poll(() => rowOf("$ clear")).toBe(-1);
};
const lastCommand = () => execFileSync("../target/debug/arugula", ["--socket", sock, "capture", "--last-command", "1"], { encoding: "utf8" });

test("a drag, a double-click and a triple-click copy to the clipboard", async () => {
  await clear();
  typeTui("echo foo.bar baz-qux end; echo second-line");
  await expect.poll(() => rowOf("second-line")).toBeGreaterThan(0);
  const y = rowOf("foo.bar");

  // Drag from "baz" across the line break.
  mouse(0, 8, y);
  mouse(32, 15, y);
  mouse(32, 5, y + 1);
  mouse(0, 5, y + 1, true);
  await expect.poll(clipboard).toBe("baz-qux end\nsecond");
  await expect.poll(screen).toContain("Copied 2 lines");

  for (let i = 0; i < 2; i++) {
    mouse(0, 10, y);
    mouse(0, 10, y, true);
  }
  await expect.poll(clipboard).toBe("baz-qux");
  await new Promise((r) => setTimeout(r, 600));
  for (let i = 0; i < 3; i++) {
    mouse(0, 1, y + 1);
    mouse(0, 1, y + 1, true);
  }
  await expect.poll(clipboard).toBe("second-line");
});

test("the program gets the drag when it takes the mouse, and Shift-drag still selects", async () => {
  await clear();
  typeTui("printf 'shift-me\\n\\e[?1000h\\e[?1006h'; cat -v");
  await expect.poll(() => rowOf("shift-me")).toBeGreaterThan(0);
  const y = rowOf("shift-me");
  mouse(0, 0, y);
  mouse(0, 0, y, true);
  // cat -v shows the report it was sent.
  await expect.poll(screen).toContain(`^[[<0;1;${y + 1}M`);
  mouse(4, 0, y);
  mouse(36, 7, y);
  mouse(4, 7, y, true);
  await expect.poll(clipboard).toBe("shift-me");
  tmux("send-keys", "-t", "tui", "C-c");
  typeTui("printf '\\e[?1000l\\e[?1006l'");
});

test("the keyboard selects and copies; the wheel scrolls back under a marker until you type", async () => {
  await clear();
  typeTui("seq 1 50; echo kb-one; echo kb-two");
  await expect.poll(() => rowOf("kb-two")).toBeGreaterThan(0);
  await expect.poll(() => screen()).toMatch(/│kb-two\n.*│\$/);

  // From the prompt: up a row, the whole line.
  command("[");
  await expect.poll(screen).toContain("COPY");
  keys("kVy");
  await expect.poll(clipboard).toBe("kb-two");
  await expect.poll(screen).not.toContain("COPY ");
  // Up two, to the start, three cells.
  command("[");
  keys("kk0vlly");
  await expect.poll(clipboard).toBe("kb-");

  const y = rowOf("45");
  mouse(64, 3, y);
  mouse(64, 3, y);
  await expect.poll(screen).toContain("↑6");
  await expect.poll(() => rowOf("45")).toBe(y + 6);
  keys("x");
  await expect.poll(screen).not.toContain("↑6");
  await expect.poll(() => rowOf("45")).toBe(y);
  tmux("send-keys", "-t", "tui", "BSpace");
});

test("a search finds a line 30k rows up, and the view lands on it", async () => {
  await clear();
  typeTui("echo needle-$((40+2)); seq 1 40000");
  await expect.poll(() => rowOf("40000"), { timeout: 30_000 }).toBeGreaterThan(0);
  await expect.poll(() => rowOf("$")).toBeGreaterThan(0);
  command("[");
  await expect.poll(screen).toContain("COPY");
  keys("?");
  typeTui("needle-42");
  // The TUI holds 10k rows: it fetches the rest from the daemon and looks again.
  await expect.poll(() => rowOf("needle-42"), { timeout: 15_000 }).toBeGreaterThan(0);
  expect(screen()).toMatch(/↑\d{5}/);
  keys("y");
  await expect.poll(clipboard).toBe("needle-42");
  // Leaving copy mode goes back to the bottom.
  await expect.poll(() => rowOf("needle-42")).toBe(-1);
  await expect.poll(() => rowOf("40000")).toBeGreaterThan(0);
});

test("a command's output selected by its mark copies what capture --last-command prints", async () => {
  await clear();
  typeTui("echo first; ls /nonexistent-dir 2>&1; printf 'two  words\\n\\n' ; seq 3");
  await expect.poll(() => screen()).toMatch(/│3\n.*│\$/);
  command("[");
  // Up to the last command's prompt, then its output.
  keys("[");
  keys("o");
  keys("y");
  const want = lastCommand();
  expect(want).toContain("two  words\n\n1");
  await expect.poll(clipboard).toBe(want);
  await expect.poll(screen).not.toContain("COPY ");
});
