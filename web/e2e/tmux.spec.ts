// M5: one layout, two front ends. A tmux control-mode client (what iTerm2
// runs over ssh: `illogical tmux -CC`) and the browser attach to the same
// daemon; a split or drag made through either shows live in the other.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { active, menu, open, paneEl, panes, ready, tab, tabsInSession, text, type, closeContexts } from "./helpers";
import { ANY, daemonPort } from "./ports";
import { labs } from "./labs";

test.afterAll(closeContexts);

let base = "";
let daemon: ChildProcess;
let state: string;

test.use({ baseURL: async ({}, use) => use(base) });
test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  state = mkdtempSync(join(tmpdir(), "illogical-e2e-tmux-"));
  daemon = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", ANY, "--state-dir", labs(state)],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/tailscaled.sock"],
    ],
    { stdio: "ignore", env: { ...process.env, PS1: "$ " } },
  );
  base = `http://127.0.0.1:${await daemonPort(state, daemon)}`;
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${base}/api/host`)).ok) return;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("daemon did not start");
});

test.afterAll(() => {
  daemon?.kill("SIGKILL");
  rmSync(state, { recursive: true, force: true });
});

function sock(): string {
  try {
    return readFileSync(join(state, "sock.path"), "utf8").trim();
  } catch {
    return join(state, "sock");
  }
}

/** A tmux control-mode client, as iTerm2 is one. */
class ControlClient {
  proc: ChildProcess;
  lines: string[] = [];
  private buf = "";

  constructor() {
    this.proc = spawn("../target/debug/illogical", ["--socket", sock(), "tmux", "-CC"], {
      stdio: ["pipe", "pipe", "inherit"],
      env: { ...process.env, SHELL: "/bin/bash" },
    });
    this.proc.stdout!.on("data", (d: Buffer) => {
      this.buf += d.toString("utf8");
      let i;
      while ((i = this.buf.indexOf("\n")) >= 0) {
        this.lines.push(this.buf.slice(0, i).replace(/\r$/, "").replace("\x1bP1000p", ""));
        this.buf = this.buf.slice(i + 1);
      }
    });
  }

  send(line: string) {
    this.proc.stdin!.write(`${line}\r`);
  }

  /** Lines that match, waiting for at least one. */
  async waitFor(re: RegExp, from = 0): Promise<string> {
    await expect.poll(() => this.lines.slice(from).some((l) => re.test(l)), { timeout: 10_000 }).toBe(true);
    return this.lines.slice(from).find((l) => re.test(l))!;
  }

  /** Run one command; its reply's body. */
  async run(cmd: string): Promise<string[]> {
    const from = this.lines.length;
    this.send(cmd);
    const end = await this.waitFor(/^%(end|error) \d+ \d+ 1$/, from);
    const lines = this.lines.slice(from);
    const begin = lines.findIndex((l) => /^%begin \d+ \d+ 1$/.test(l));
    const stop = lines.indexOf(end);
    expect(end.startsWith("%end"), `${cmd}: ${lines.slice(begin, stop + 1).join(" / ")}`).toBe(true);
    return lines.slice(begin + 1, stop);
  }

  close() {
    this.proc.kill();
  }
}

test("a split and a drag from the tmux client show in the browser, and back", async ({ page }) => {
  await open(page);
  const first = await active(page);
  await ready(page, first);
  const cc = new ControlClient();
  try {
    await cc.waitFor(/^%session-changed \$\d+ /);
    // iTerm2 splits a pane...
    await cc.run(`split-window -h -t %${first}`);
    await expect.poll(() => panes(page)).toHaveLength(2);
    const [, second] = await panes(page);
    // ...the browser draws both, sized by the browser (it owns the size).
    await ready(page, second);
    const before = (await tab(page)).layout.splits[0].extents;
    // ...and a divider drag in iTerm2 moves the browser's divider.
    await cc.run(`resize-pane -R -t %${first} 5`);
    await expect.poll(async () => (await tab(page)).layout.splits[0].extents).toEqual([before[0] + 5, before[1] - 5]);
    // The client's own picture of it is what the browser shows.
    const [layout] = await cc.run(`display -p -t %${first} '#{window_layout}'`);
    const t = await tab(page);
    expect(layout).toContain(`${t.cols}x${t.rows},0,0{${before[0] + 5}x${t.rows},0,0,${first},`);

    // Typing in the tmux client runs in the pane the browser shows.
    await cc.run(`send -lt %${second} 'echo from-$((40+2))'`);
    await cc.run(`send -H -t %${second} 0d`);
    await expect.poll(() => text(page, second)).toContain("from-42");
    // Typing in the browser streams to the tmux client.
    const mark = cc.lines.length;
    await type(page, first, "echo web-$((6*7))\n");
    await cc.waitFor(new RegExp(`^%output %${first} .*web-42`), mark);

    // A split in the browser: the tmux client is told the new layout.
    const from = cc.lines.length;
    await menu(page, paneEl(page, second), "Split down");
    await expect.poll(() => panes(page)).toHaveLength(3);
    const third = (await panes(page)).find((p) => p !== first && p !== second)!;
    await cc.waitFor(new RegExp(`^%layout-change @\\d+ \\S*,${third}[\\]}].* \\*$`), from);

    // A window from the tmux client is a tab in the browser; closing it
    // there closes it here.
    const [win] = await cc.run(`new-window -d -P -F '#{window_id}'`);
    await expect.poll(() => tabsInSession(page)).toHaveLength(2);
    const before2 = cc.lines.length;
    await page.evaluate((id) => window.__illogical.client.intent({ op: "close_tab", tab: id }), Number(win.slice(1)));
    await cc.waitFor(new RegExp(`^%window-close ${win}$`), before2);
  } finally {
    cc.close();
  }
});
