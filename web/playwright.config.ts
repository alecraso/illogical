import { chmodSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "@playwright/test";
// Loopback callers show the daemon's local token: every daemon of the run
// shares one, which browsers carry as a cookie.
import { tokenCookies } from "./e2e/local-token.ts";

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

// #111: *Install* runs this stand-in npm, which "installs" an adapter as
// the fake ACP agent, so no test downloads one.
{
  const dir = runDir("ILLOGICAL_E2E_NPM_DIR", "illogical-e2e-npm-");
  const fake = fileURLToPath(new URL("../crates/daemon/tests/fake_acp.py", import.meta.url));
  writeFileSync(
    join(dir, "npm"),
    `#!/bin/sh\nwhile [ $# -gt 0 ]; do case "$1" in --prefix) p=$2; shift 2 ;; install|--*) shift ;; *) pkg=$1; shift ;; esac; done\n` +
      `name=\${pkg%@*}; bin=\${name##*/}\nmkdir -p "$p/node_modules/.bin" "$p/node_modules/$name"\n` +
      `printf '{"version": "%s"}\\n' "\${pkg##*@}" >"$p/node_modules/$name/package.json"\n` +
      `printf '#!/bin/sh\\nexec python3 ${fake} "$@"\\n' >"$p/node_modules/.bin/$bin"\nchmod +x "$p/node_modules/.bin/$bin"\necho "added 1 package"\n`,
  );
  chmodSync(join(dir, "npm"), 0o755);
  process.env.ILLOGICAL_NPM = join(dir, "npm");
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
  // M39: and a stand-in `glab` beside it, whose default host (and the one
  // it has a token for) is whatever forge-gitlab.spec.ts writes to
  // glab-host: none, so a GitLab block reads anonymously, until it does.
  writeFileSync(
    join(tea, "glab"),
    `#!/bin/sh\nd='${tea}'\nh=$(cat "$d/glab-host" 2>/dev/null)\ncase "$1 $2 $3" in\n  "config get host") echo "$h" ;;\n  "config get token") [ -n "$h" ] && [ "$5" = "$h" ] && echo e2e-gitlab-token ;;\nesac\nexit 0\n`,
  );
  chmodSync(join(tea, "glab"), 0o755);
  if (!process.env.PATH?.startsWith(`${tea}:`)) process.env.PATH = `${tea}:${process.env.PATH}`;
  process.env.ILLOGICAL_FORGE_POLL_MS ??= "300,1500";
}

// M43: Fountain blocks read the credentials file fountain.spec.ts writes
// (pointing at its fake Fountain), never ~/.fountain; and *Run on
// Fountain* runs a stand-in `fountain` that is the fake ACP agent.
{
  const dir = runDir("ILLOGICAL_E2E_FOUNTAIN_DIR", "illogical-e2e-fountain-");
  process.env.ILLOGICAL_FOUNTAIN_CREDENTIALS = join(dir, "credentials");
  const fake = fileURLToPath(new URL("../crates/daemon/tests/fake_acp.py", import.meta.url));
  writeFileSync(join(dir, "fountain"), `#!/bin/sh\nexec python3 ${fake} "$@"\n`);
  chmodSync(join(dir, "fountain"), 0o755);
  process.env.ILLOGICAL_FOUNTAIN_BIN = join(dir, "fountain");
  for (const k of ["FOUNTAIN_API_KEY", "FOUNTAIN_BASE_URL", "FOUNTAIN_PROFILE"]) delete process.env[k];
  process.env.ILLOGICAL_FOUNTAIN_POLL_MS ??= "1500";
  // M45b: the runner view's unit file (fountain-runner.spec.ts writes it;
  // without it this host isn't a runner), a `systemctl` that says active,
  // and a `sudo` that runs bash as this user: no test runs sudo.
  process.env.ILLOGICAL_FOUNTAIN_UNIT_FILE = join(dir, "fountain-runner.service");
  writeFileSync(join(dir, "systemctl"), `#!/bin/sh\n[ "$1 $2" = "is-active fountain-runner" ] && echo active\n`);
  chmodSync(join(dir, "systemctl"), 0o755);
  process.env.ILLOGICAL_FOUNTAIN_SYSTEMCTL = join(dir, "systemctl");
  writeFileSync(
    join(dir, "sudo"),
    `#!/bin/sh\n[ "$1 $2 $3 $4" = "-n -u fountain /bin/bash" ] || { echo "sudo: a password is required" >&2; exit 1; }\nshift 4\nexec /bin/bash "$@"\n`,
  );
  chmodSync(join(dir, "sudo"), 0o755);
  process.env.ILLOGICAL_FOUNTAIN_SUDO = join(dir, "sudo");
  // M44: a worn agent's variables never reach the person's Infisical or
  // gh (and its bundle goes in the run's own cache: the daemon's command).
  process.env.ILLOGICAL_INFISICAL_BIN = "/bin/false";
  process.env.ILLOGICAL_GH_BIN = "/bin/false";
  // #145: no `chant audit --agents` of the person's agent config, so every
  // screen rule set runs whatever is configured on the host.
  process.env.ILLOGICAL_CHANT = "";
}

// By default runs against a throwaway debug daemon on 7683 (which serves
// web/dist from disk), driving the system Chrome (E2E_CHROMIUM=1: Playwright's
// own Chromium, for machines without Chrome, like CI's; the full build, as the
// headless shell denies notifications whatever's granted); `*.webkit.spec.ts` drive
// Playwright's WebKit (#94: Safari's engine, where device keys behave
// differently), which needs `pnpm exec playwright install webkit`. Set E2E_BASE_URL to test a
// daemon that is already running, e.g. through `tailscale serve`.
// E2E_PORT runs it elsewhere (beside another worktree's run, say).
const port = Number(process.env.E2E_PORT) || 7683;
const external = process.env.E2E_BASE_URL || undefined;
// M40: forge webhooks reach the test daemon on its own port.
if (!external) process.env.ILLOGICAL_FORGE_HOOK_BASE ??= `http://127.0.0.1:${port}`;
// E2E_DAEMON_LOG=/path/to/file keeps the test daemon's debug log.
const log = process.env.E2E_DAEMON_LOG ? ` >>${process.env.E2E_DAEMON_LOG} 2>&1` : "";

// CI splits the specs (#287): E2E_SET=stack runs only those on the Docker
// test stack, E2E_SET=perf only those that time things (the frame rates,
// and editors.spec's files in under 3 s), on a host with nothing else of
// the run on it, E2E_SET=rest everything else, sharded across runners.
const SETS: Record<string, RegExp> = {
  stack: /(team-swarm-phones|testnet-hosts|editor-remote-ssh)\.spec\.ts$/,
  perf: /(swarm-fps|editors)\.spec\.ts$/,
};
const set = process.env.E2E_SET;
const WEBKIT = /\.webkit\.spec\.ts$/;
const chrome = set && SETS[set] ? { testMatch: SETS[set] } : { testIgnore: set === "rest" ? [WEBKIT, ...Object.values(SETS)] : WEBKIT };

export default defineConfig({
  testDir: "e2e",
  timeout: 30_000,
  // CI's hosts run other jobs beside the specs (#287): an expectation gets
  // longer there, and a spec that fails once runs again, reported as flaky.
  expect: { timeout: process.env.CI ? 10_000 : 5_000 },
  retries: process.env.CI ? 1 : 0,
  fullyParallel: false,
  workers: 1,
  use: {
    baseURL: external ?? `http://127.0.0.1:${port}`,
    viewport: { width: 1000, height: 640 },
    storageState: { cookies: tokenCookies, origins: [] },
  },
  projects: [
    // Chrome hands a granted notification to the desktop over the session's
    // D-Bus, and geek's CI runners run in the person's session: pointed at
    // no bus, it keeps them to itself (getNotifications still sees them).
    {
      name: "chrome",
      use: {
        channel: process.env.E2E_CHROMIUM ? "chromium" : "chrome",
        launchOptions: { env: { ...process.env, DBUS_SESSION_BUS_ADDRESS: "unix:path=/nonexistent" } },
      },
      ...chrome,
    },
    // The stack's and the frame rates' specs are all Chrome's.
    { name: "webkit", use: { browserName: "webkit" }, testMatch: set && SETS[set] ? /^$/ : WEBKIT },
  ],
  webServer: external
    ? undefined
    : {
        // Only the daemon's cache: Playwright keeps its browsers in XDG_CACHE_HOME.
        command: `XDG_CACHE_HOME="${runDir("ILLOGICAL_E2E_CACHE", "illogical-e2e-cache-")}" RUST_LOG=illogicald=debug ../target/debug/illogicald --listen 127.0.0.1:${port} --shell "bash --norc --noprofile" --no-manager-env --state-dir "${runDir("ILLOGICAL_E2E_STATE", "illogical-e2e-")}"${log}`,
        url: `http://127.0.0.1:${port}/`,
        reuseExistingServer: false,
        stdout: "ignore",
        stderr: "ignore",
      },
});
