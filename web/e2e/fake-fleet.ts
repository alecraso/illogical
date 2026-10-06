// A fake fleet for the swarm's tests and screenshots (M26): a few daemons
// ("workstation", "build-01", "build-02") with scripted panes that look like real
// work to M23's classifier and M24's reasons. Builds, tests, servers, logs
// and editors run stand-in programs named like the real ones (`cargo`,
// `npm`, `journalctl`, `nvim`, from a bin directory first on PATH), in git
// repos for some projects and plain directories for the rest. Agents are a
// stand-in `claude` that asks through the real hooks (`illogical hook` for
// a permission, `illogical ask` for a question) and then waits for
// follow-ups through `illogical inbox`. `trouble(machine)` fails a batch of
// tests on one machine at once; `finish(machine)` ends a long build.
//
// Panes start in tabs nobody shows, so nobody is looking at them.

import { spawn, execFileSync, type ChildProcess } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { daemonPort } from "./ports.ts";
import { labs } from "./labs";

const fixtures = resolve(import.meta.dirname, "../../crates/daemon/tests/fixtures");

/** The stand-in programs. */
const BIN: Record<string, string> = {
  cargo: `#!/bin/bash
# cargo stand-in: build and test print like the real thing.
case "$*" in
  *"-p relay"*) for i in 1 2 3 4 5 6 7; do echo "test grid::reflow_$i ... ok"; sleep 0.5; done
            echo "test relay::expires ... FAILED"; echo "test result: FAILED. 6 passed; 1 failed"; exit 101 ;;
  *--release*) for i in $(seq 1 11); do echo "   Compiling crate-$i v0.1.$i"; sleep 0.5; done; echo "    Finished release"; exit 0 ;;
  test*) while true; do echo "test vt::parser::case_$RANDOM ... ok"; sleep 0.7; done ;;
  *) while true; do echo "   Compiling serde-$RANDOM v1.0.0"; sleep 0.4; done ;;
esac
`,
  npm: `#!/bin/bash
echo "  VITE ready in 312 ms"; while true; do echo "GET /api/session 200 $((RANDOM % 40))ms"; sleep 1.1; done
`,
  journalctl: `#!/bin/bash
while true; do echo "illogicald: pane %$((RANDOM % 400)) output $((RANDOM % 90)) lines"; sleep 2.3; done
`,
  nvim: `#!/bin/bash
echo "-- NORMAL --"; sleep 100000
`,
  pytest: `#!/bin/bash
while true; do echo "tests/test_$((RANDOM % 40)).py::test_case_$RANDOM PASSED"; sleep 0.$((RANDOM % 9 + 1)); done
`,
  go: `#!/bin/bash
case "$1" in
  test) while true; do echo "ok  	example.com/svc/pkg$((RANDOM % 20))	0.$((RANDOM % 900))s"; sleep 1.$((RANDOM % 9)); done ;;
  *) while true; do echo "building example.com/svc/cmd$((RANDOM % 9))"; sleep 0.$((RANDOM % 6 + 2)); done ;;
esac
`,
  make: `#!/bin/bash
while true; do echo "cc -O2 -c src/mod_$((RANDOM % 80)).c"; sleep 0.$((RANDOM % 5 + 1)); done
`,
  docker: `#!/bin/bash
while true; do echo "web-1  | $(date +%T) GET /health 200"; sleep $((RANDOM % 4 + 2)); done
`,
  tail: `#!/bin/bash
while true; do echo "$(date +%T) worker[$((RANDOM % 8))]: job $RANDOM done"; sleep $((RANDOM % 6 + 1)).$((RANDOM % 9)); done
`,
  uvicorn: `#!/bin/bash
echo "INFO:     Uvicorn running on http://127.0.0.1:8000"; while true; do echo "INFO:     127.0.0.1 - \\"GET /items/$RANDOM HTTP/1.1\\" 200"; sleep $((RANDOM % 3)).$((RANDOM % 9)); done
`,
  vim: `#!/bin/bash
echo "-- INSERT --"; sleep 100000
`,
  claude: `#!/bin/bash
# claude stand-in: works a little, asks through the real hooks, then
# waits for follow-ups (what Claude Code's settings would run).
mode=\${1:-perm}; what=\${2:-cargo test}; sid="fake-$$"
echo "● Read src/main.rs"; sleep 0.3
if [ "$mode" = ask ]; then
  illogical ask < "$FAKE_FIXTURES/s13-hook-ask.json" | head -c 160; echo
elif [ "$mode" = perm ]; then
  printf '{"hook_event_name":"PreToolUse","session_id":"%s","tool_name":"Bash","tool_input":{"command":"%s"},"tool_use_id":"toolu_%s"}' "$sid" "$what" "$$" | illogical hook
  printf '{"hook_event_name":"PermissionRequest","session_id":"%s","tool_name":"Bash","tool_input":{"command":"%s"},"permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"%s"}],"behavior":"allow","destination":"localSettings"}]}' "$sid" "$what" "$what" | illogical hook
fi
echo "● Done"
while true; do printf '{"hook_event_name":"Stop","session_id":"%s"}' "$sid" | illogical inbox 2>&1 | sed 's/^/● got: /'; done
`,
};

export interface FakeMachine {
  name: string;
  port: number;
  url: string;
  proc: ChildProcess;
}

export class FakeFleet {
  readonly root = mkdtempSync(join(tmpdir(), "ilg-fake-fleet-"));
  readonly home = join(this.root, "home", "fake");
  machines: FakeMachine[] = [];
  private bin = join(this.root, "bin");

  /** `projects`: the git repositories under ~/src. */
  constructor(readonly projects = ["api", "web", "infra"]) {
    mkdirSync(this.bin, { recursive: true });
    for (const [name, body] of Object.entries(BIN)) {
      writeFileSync(join(this.bin, name), body);
      chmodSync(join(this.bin, name), 0o755);
    }
    // Projects that are git repositories, and directories that aren't.
    for (const p of projects) {
      const d = join(this.home, "src", p);
      mkdirSync(d, { recursive: true });
      execFileSync("git", ["init", "-q", d]);
    }
    mkdirSync(join(this.home, "scratch"), { recursive: true });
    mkdirSync(join(this.root, "var"), { recursive: true });
  }

  dir(where: string): string {
    if (where === "scratch") return join(this.home, "scratch");
    if (where === "var") return join(this.root, "var");
    return join(this.home, "src", where);
  }

  /** Start a daemon; the first is the home daemon the page comes from, and
   * lists the others. On `port`, or one of its choosing. */
  async machine(name: string, port = 0): Promise<FakeMachine> {
    const state = join(this.root, `state-${name}`);
    const home = this.machines[0]?.url;
    const proc = spawn(
      resolve(import.meta.dirname, "../../target/debug/illogicald"),
      [
        ...["--listen", `127.0.0.1:${port}`, "--name", name, "--state-dir", labs(state)],
        ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/sock"],
        ...(home ? ["--allow-origin", home] : []),
      ],
      {
        stdio: "ignore",
        env: { ...process.env, HOME: this.home, PATH: `${this.bin}:${process.env.PATH}`, FAKE_FIXTURES: fixtures },
      },
    );
    if (!port) port = await daemonPort(state, proc);
    const url = `http://127.0.0.1:${port}`;
    for (let i = 0; i < 100; i++) {
      try {
        if ((await fetch(`${url}/api/host`)).ok) break;
      } catch {
        // not yet
      }
      await new Promise((r) => setTimeout(r, 100));
    }
    const m = { name, port, url, proc };
    this.machines.push(m);
    if (home) await post(home, "/api/hosts", { name, urls: [url] });
    return m;
  }

  get(name: string): FakeMachine {
    return this.machines.find((m) => m.name === name)!;
  }

  /** A shell in `where`, in a tab of its own, running `line` (if any). */
  async pane(machine: string, where: string, line?: string): Promise<number> {
    const m = this.get(machine);
    const pane = (await post(m.url, "/api/run", { cwd: this.dir(where) })).pane as number;
    if (line) {
      // Its prompt first, so shell integration sees the command start.
      for (let i = 0; i < 50; i++) {
        const info = ((await (await fetch(`${m.url}/api/panes`)).json()) as { id: number; running: boolean }[]).find((p) => p.id === pane);
        if (info?.running) break;
        await new Promise((r) => setTimeout(r, 100));
      }
      await new Promise((r) => setTimeout(r, 300));
      await post(m.url, `/api/panes/${pane}/send`, { text: line, enter: true });
    }
    return pane;
  }

  /** A pane running `command` by itself (or a shell at its prompt), in
   * `where`: quick to make in bulk, with no wait for a prompt. */
  async run(machine: string, where: string, command?: string): Promise<number> {
    const body = { cwd: this.dir(where), ...(command ? { command } : {}) };
    return (await post(this.get(machine).url, "/api/run", body)).pane as number;
  }

  /** The everyday work: a few of each kind on each machine (the first
   * three started, in the roles of "workstation", "build-01" and "build-02"). */
  async populate() {
    const name = (n: string) => this.machines[["workstation", "build-01", "build-02"].indexOf(n)].name;
    const work: [string, string, string | undefined][] = [
      ["workstation", "api", "cargo build"],
      ["workstation", "api", "cargo test"],
      ["workstation", "api", "npm run dev"],
      ["workstation", "web", "nvim app.py"],
      ["workstation", "scratch", undefined],
      ["workstation", "var", "journalctl -f"],
      ["build-01", "api", "cargo test"],
      ["build-01", "infra", "npm run dev"],
      ["build-01", "scratch", undefined],
      ["build-02", "web", "cargo build"],
      ["build-02", "var", "journalctl -f"],
      ["build-02", "infra", undefined],
    ];
    for (const [m, where, line] of work) await this.pane(name(m), where, line);
  }

  /** An agent asking to run `command` (a permission card). */
  agentAsks(machine: string, where: string, command: string) {
    return this.pane(machine, where, `claude perm '${command}'`);
  }

  /** An agent asking a question (AskUserQuestion's card). */
  agentQuestion(machine: string, where: string) {
    return this.pane(machine, where, "claude ask");
  }

  /** Make trouble: `n` test runs fail on one machine at once. */
  async trouble(machine: string, n = 3): Promise<number[]> {
    const out: number[] = [];
    for (let i = 0; i < n; i++) out.push(await this.pane(machine, "api", "cargo test -p relay"));
    return out;
  }

  /** A long build finishes (a "done" card). */
  finish(machine: string) {
    return this.pane(machine, "api", "cargo build --release");
  }

  /** Close a pane (an agent that's done with). */
  async close(machine: string, pane: number) {
    await post(this.get(machine).url, `/api/panes/${pane}/close`, {});
  }

  stop() {
    for (const m of this.machines) m.proc.kill("SIGKILL");
    // Its panes' programs may still be writing for a moment.
    try {
      rmSync(this.root, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
    } catch {
      // A scratch directory under /tmp: left to the system.
    }
  }
}

async function post(base: string, path: string, body: unknown): Promise<Record<string, unknown>> {
  const res = await fetch(base + path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
  if (!res.ok) throw new Error(`${path}: ${res.status} ${await res.text()}`);
  return (await res.json()) as Record<string, unknown>;
}
