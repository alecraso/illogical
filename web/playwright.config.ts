import { chmodSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "@playwright/test";

// Directories for the run, made once (workers load this config too, and
// inherit them) and removed when the runner exits, after the daemon (#62).
const made: string[] = [];
function runDir(env: string, prefix: string): string {
  if (!process.env[env]) made.push((process.env[env] = mkdtempSync(join(tmpdir(), prefix))));
  return process.env[env]!;
}
process.on("exit", () => {
  for (const d of made) rmSync(d, { recursive: true, force: true });
});

// Daemons the tests start register as Claude Code's IDE (M28) here, not in
// ~/.claude/ide: every spec's daemon inherits this (workers too).
runDir("ILLOGICAL_CLAUDE_IDE_DIR", "illogical-e2e-ide-");

// M33: the daemon lists Claude Code conversations from a Claude directory
// of the run's own (conversations.spec.ts seeds it), and Claude Code's
// adapter is the fake ACP agent, so no test reaches a real Claude.
mkdirSync(join(runDir("CLAUDE_CONFIG_DIR", "illogical-e2e-claude-"), "sessions"), { recursive: true });
runDir("FAKE_ACP_DIR", "illogical-e2e-fake-acp-");
{
  const bin = join(runDir("ILLOGICAL_AGENTS_DIR", "illogical-e2e-agents-"), "claude/node_modules/.bin");
  mkdirSync(bin, { recursive: true });
  const fake = fileURLToPath(new URL("../crates/daemon/tests/fake_acp.py", import.meta.url));
  writeFileSync(join(bin, "claude-agent-acp"), `#!/bin/sh\nexec python3 ${fake} "$@"\n`);
  chmodSync(join(bin, "claude-agent-acp"), 0o755);
}

// M36: forge blocks read through a stand-in `tea` (and, M38, `gh`) on the daemon's PATH,
// never the person's own: its logins are whatever forge.spec.ts writes
// (its fake Forgejo), and its credential helper hands out a fixed token.
{
  const tea = runDir("ILLOGICAL_E2E_TEA_DIR", "illogical-e2e-tea-");
  if (!existsSync(join(tea, "logins.json"))) writeFileSync(join(tea, "logins.json"), "[]");
  writeFileSync(
    join(tea, "tea"),
    `#!/bin/sh\nd='${tea}'\ncase "$1 $2" in\n  "logins list") cat "$d/logins.json" ;;\n  "login helper") cat >/dev/null; echo username=jhgaylor; echo password=e2e-forge-token ;;\n  *) exit 2 ;;\nesac\n`,
  );
  chmodSync(join(tea, "tea"), 0o755);
  // M38: and a stand-in `gh`, whose `auth token` hands out a fixed token
  // for the hosts in gh-hosts (forge-github.spec.ts's fake GitHub), as the
  // real one has a login only for some hosts.
  if (!existsSync(join(tea, "gh-hosts"))) writeFileSync(join(tea, "gh-hosts"), "");
  writeFileSync(
    join(tea, "gh"),
    `#!/bin/sh\nd='${tea}'\ncase "$1 $2" in\n  "auth token") grep -qx -- "$4" "$d/gh-hosts" || exit 1; echo e2e-github-token ;;\n  *) exit 2 ;;\nesac\n`,
  );
  chmodSync(join(tea, "gh"), 0o755);
  if (!process.env.PATH?.startsWith(`${tea}:`)) process.env.PATH = `${tea}:${process.env.PATH}`;
  process.env.ILLOGICAL_FORGE_POLL_MS ??= "300,1500";
}

// By default runs against a throwaway debug daemon on 7683 (which serves
// web/dist from disk), driving the system Chrome. Set E2E_BASE_URL to test a
// daemon that is already running, e.g. through `tailscale serve`.
// E2E_PORT runs it elsewhere (beside another worktree's run, say).
const port = Number(process.env.E2E_PORT) || 7683;
const external = process.env.E2E_BASE_URL || undefined;
// E2E_DAEMON_LOG=/path/to/file keeps the test daemon's debug log.
const log = process.env.E2E_DAEMON_LOG ? ` >>${process.env.E2E_DAEMON_LOG} 2>&1` : "";

export default defineConfig({
  testDir: "e2e",
  timeout: 30_000,
  fullyParallel: false,
  workers: 1,
  use: {
    baseURL: external ?? `http://127.0.0.1:${port}`,
    channel: "chrome",
    viewport: { width: 1000, height: 640 },
  },
  webServer: external
    ? undefined
    : {
        command: `RUST_LOG=illogicald=debug ../target/debug/illogicald --listen 127.0.0.1:${port} --shell "bash --norc --noprofile" --no-manager-env --state-dir "${runDir("ILLOGICAL_E2E_STATE", "illogical-e2e-")}"${log}`,
        url: `http://127.0.0.1:${port}/`,
        reuseExistingServer: false,
        stdout: "ignore",
        stderr: "ignore",
      },
});
