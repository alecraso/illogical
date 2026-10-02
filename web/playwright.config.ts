import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { defineConfig } from "@playwright/test";

// Daemons the tests start register as Claude Code's IDE (M28) here, not in
// ~/.claude/ide: every spec's daemon inherits this (workers too).
process.env.ILLOGICAL_CLAUDE_IDE_DIR ??= mkdtempSync(join(tmpdir(), "illogical-e2e-ide-"));

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
        command: `RUST_LOG=illogicald=debug ../target/debug/illogicald --listen 127.0.0.1:${port} --shell "bash --norc --noprofile" --no-manager-env --state-dir "$(mktemp -d -t illogical-e2e-XXXXXX)"${log}`,
        url: `http://127.0.0.1:${port}/`,
        reuseExistingServer: false,
        stdout: "ignore",
        stderr: "ignore",
      },
});
