// S17: a real IDE registered beside the fake one. Runs code-server with Anthropic's Claude Code
// extension (from Open VSX) on work/proj, opened in headless Chrome so the extension activates and
// writes its lockfile, and holds it open until work/realide.stop exists (or 15 minutes pass).
// Meanwhile probe.sh / turn.sh point the real `claude` at one or the other.
//
//   node spikes/s17-editors/realide.mjs <code-server-dir>
//
// The server's HOME is a throwaway dir whose .claude/ide is a symlink to the real ~/.claude/ide
// (where the CLI looks), so nothing else the extension writes lands in the real home.
// Writes work/realide.json: the lockfile it wrote, and screenshots on request (work/realide.shot).

import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire("/home/me/dev/jhgaylor/illogical/web/package.json");
const { chromium } = require("@playwright/test");
const [dir] = process.argv.slice(2);
const work = path.join(here, "work");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const port = 7881;
const sdir = path.join(work, "realide-srv");
rmSync(sdir, { recursive: true, force: true });
mkdirSync(path.join(sdir, "home/.claude"), { recursive: true });
const ideDir = path.join(os.homedir(), ".claude/ide");
mkdirSync(ideDir, { recursive: true });
symlinkSync(ideDir, path.join(sdir, "home/.claude/ide"));
rmSync(path.join(work, "realide.stop"), { force: true });

const env = { ...process.env, HOME: path.join(sdir, "home"), XDG_CONFIG_HOME: path.join(sdir, "xdgc"), XDG_DATA_HOME: path.join(sdir, "xdg") };
for (const k of Object.keys(env)) if (k.startsWith("CLAUDE") || k === "AI_AGENT") delete env[k];
const common = ["--user-data-dir", path.join(sdir, "user"), "--extensions-dir", path.join(sdir, "ext"), "--config", path.join(sdir, "config.yaml")];
console.error(execFileSync(path.join(dir, "bin/code-server"), [...common, "--install-extension", "anthropic.claude-code"], { env }).toString().trim());
const before = new Set(readdirSync(ideDir));
const server = spawn(path.join(dir, "bin/code-server"), ["--bind-addr", `127.0.0.1:${port}`, "--auth", "none", "--disable-telemetry", "--disable-update-check", "--disable-workspace-trust", ...common], { env, stdio: "ignore", detached: true });
process.on("exit", () => {
  try {
    process.kill(-server.pid, "SIGTERM");
  } catch {}
});
for (let i = 0; i < 300; i++) {
  try {
    await fetch(`http://127.0.0.1:${port}/`);
    break;
  } catch {
    await sleep(100);
  }
}
const browser = await chromium.launch({ channel: "chrome" });
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
await page.goto(`http://127.0.0.1:${port}/?folder=${encodeURIComponent(path.join(work, "proj"))}`);
await page.locator(".explorer-folders-view .monaco-list-row", { hasText: "hello.py" }).first().waitFor({ timeout: 60000 });
const t0 = Date.now();
let lock = null;
while (!lock && Date.now() - t0 < 60000) {
  const fresh = readdirSync(ideDir).filter((f) => !before.has(f));
  if (fresh.length) lock = { file: fresh[0], after_ms: Date.now() - t0, ...JSON.parse(readFileSync(path.join(ideDir, fresh[0]), "utf8")) };
  else await sleep(250);
}
if (lock) lock.authToken = lock.authToken ? `<${lock.authToken.length} chars>` : null;
writeFileSync(path.join(work, "realide.json"), JSON.stringify({ lock }, null, 1));
console.error(JSON.stringify(lock));
const end = Date.now() + 15 * 60000;
while (Date.now() < end && !existsSync(path.join(work, "realide.stop"))) {
  if (existsSync(path.join(work, "realide.shot"))) {
    const name = readFileSync(path.join(work, "realide.shot"), "utf8").trim() || "shot";
    rmSync(path.join(work, "realide.shot"));
    await page.screenshot({ path: path.join(work, `realide-${name}.png`) });
  }
  if (existsSync(path.join(work, "realide.click"))) {
    // A button in the page by its accessible name, e.g. the diff's "Accept".
    const label = readFileSync(path.join(work, "realide.click"), "utf8").trim();
    rmSync(path.join(work, "realide.click"));
    try {
      await page.getByRole("button", { name: label }).first().click({ timeout: 5000 });
      console.error(`clicked ${label}`);
    } catch (e) {
      console.error(`no ${label}: ${String(e).slice(0, 120)}`);
    }
  }
  await sleep(500);
}
await browser.close();
process.kill(-server.pid, "SIGTERM");
await sleep(2000);
console.error("lockfiles left:", readdirSync(ideDir));
