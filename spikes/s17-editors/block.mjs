// S17: an editor server inside an M6a browser block, with no auth of its own.
//
//   node spikes/s17-editors/block.mjs <openvscode|code-server> <server-dir>
//
// Starts a throwaway illogicald (target/debug, its own state dir, loopback ports 7861/7862 in the
// dev scheme, so each block is http://b-<id>-<key>.localhost:7862), the editor server on
// 127.0.0.1:7863 with no token or password, opens the app in headless Chrome, runs
// `illogical open :7863/?folder=…`, and waits for the workbench, the explorer and a file inside the
// block's frame. Typing into the file checks the WebSockets (the extension host and the file
// service) work through the proxy. Timed: block opened to file shown, warm server.
// Then checks what the proxy already does about the server's lack of auth: the block's origin is
// the only way in from the browser, but the port itself still answers any local process.

import { execFileSync, spawn } from "node:child_process";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire("/home/me/dev/jhgaylor/illogical/web/package.json");
const { chromium } = require("@playwright/test");
const [which, dir] = process.argv.slice(2);
const repo = "/home/me/dev/jhgaylor/illogical";
const work = path.join(here, "work");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const APP = 7861, BLOCKS = 7862, ED = 7863;

const state = path.join(work, `blk-state-${which}`);
const sdir = path.join(work, `blk-srv-${which}`);
const proj = path.join(work, `blk-proj`);
for (const d of [state, sdir, proj]) {
  rmSync(d, { recursive: true, force: true });
  mkdirSync(d, { recursive: true });
}
writeFileSync(path.join(proj, "auth.rs"), readFileSync(path.join(repo, "crates/daemon/src/access.rs"), "utf8"));

const daemon = spawn(path.join(repo, "target/debug/illogicald"), ["--listen", `127.0.0.1:${APP}`, "--block-listen", `127.0.0.1:${BLOCKS}`, "--shell", "bash --norc --noprofile"], {
  stdio: "ignore",
  env: { ...process.env, ILLOGICAL_STATE_DIR: state, ILLOGICAL_WISP_TOKEN_FILE: "/nonexistent" },
});
const env = { ...process.env, HOME: path.join(sdir, "home"), XDG_CONFIG_HOME: path.join(sdir, "xdgc"), XDG_DATA_HOME: path.join(sdir, "xdg") };
let args, bin;
if (which === "openvscode") {
  bin = path.join(dir, "bin/openvscode-server");
  args = ["--host", "127.0.0.1", "--port", String(ED), "--without-connection-token", "--accept-server-license-terms", "--telemetry-level", "off", "--server-data-dir", path.join(sdir, "server"), "--user-data-dir", path.join(sdir, "user"), "--extensions-dir", path.join(sdir, "ext")];
} else {
  bin = path.join(dir, "bin/code-server");
  args = ["--bind-addr", `127.0.0.1:${ED}`, "--auth", "none", "--disable-telemetry", "--disable-update-check", "--disable-workspace-trust", "--user-data-dir", path.join(sdir, "user"), "--extensions-dir", path.join(sdir, "ext"), "--config", path.join(sdir, "config.yaml")];
}
const server = spawn(bin, args, { env, stdio: "ignore", detached: true });
process.on("exit", () => {
  for (const f of [() => daemon.kill("SIGKILL"), () => process.kill(-server.pid, "SIGTERM"), () => rmSync(readFileSync(path.join(state, "sock.path"), "utf8").trim(), { force: true })]) {
    try {
      f();
    } catch {}
  }
});
const wait = async (url) => {
  for (let i = 0; i < 300; i++) {
    try {
      await fetch(url);
      return;
    } catch {
      await sleep(100);
    }
  }
  throw new Error(`nothing at ${url}`);
};
await wait(`http://127.0.0.1:${APP}/`);
await wait(`http://127.0.0.1:${ED}/`);
// Warm the server once (first workbench load compiles and caches), as M27's "once the server is warm".
const browser = await chromium.launch({ channel: "chrome" });
{
  const p = await browser.newPage();
  await p.goto(`http://127.0.0.1:${ED}/?folder=${encodeURIComponent(proj)}`);
  await p.locator(".monaco-workbench").waitFor({ timeout: 60000 });
  await sleep(2000);
  await p.close();
}

const result = { server: which };
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
const wsUrls = [];
page.on("websocket", (ws) => wsUrls.push(ws.url().replace(/[?].*/, "")));
await page.goto(`http://127.0.0.1:${APP}/`);
await page.waitForFunction(() => window.__illogical?.client.connected && window.__illogical.client.state);
const cli = path.join(repo, "target/debug/illogical");
const t0 = Date.now();
// A state dir too deep for a socket path puts the socket elsewhere and says where in sock.path.
const sockPath = (() => {
  try {
    return readFileSync(path.join(state, "sock.path"), "utf8").trim();
  } catch {
    return path.join(state, "sock");
  }
})();
const out = execFileSync(cli, ["--json", "open", `:${ED}/?folder=${encodeURIComponent(proj)}`], { env: { ...process.env, ILLOGICAL_SOCK: sockPath } }).toString();
const block = JSON.parse(out).block;
result.block = block;
// It opens in a tab of its own; show it.
await page.waitForFunction((b) => window.__illogical.client.state.panes.some((p) => p.id === b), block);
await page.evaluate((b) => window.__illogical.client.setActive(b), block);
const frame = page.frameLocator(`[data-pane="${block}"] iframe`);
try {
  await frame.locator(".monaco-workbench").waitFor({ timeout: 60000 });
  result.workbench_ms = Date.now() - t0;
  const dialog = frame.locator(".monaco-dialog-box");
  await sleep(300);
  if (await dialog.count()) {
    result.dialog = (await dialog.innerText()).replace(/\s+/g, " ").slice(0, 80);
    await dialog.locator(".monaco-button", { hasText: "Yes, I trust" }).first().click();
  }
  await frame.locator(".explorer-folders-view .monaco-list-row", { hasText: "auth.rs" }).first().click({ timeout: 60000 });
  await frame.locator(".monaco-editor .view-lines", { hasText: "use" }).first().waitFor({ timeout: 30000 });
  result.file_shown_ms = Date.now() - t0;
  await frame.locator(".monaco-editor .view-lines").first().click();
  await page.keyboard.press("Control+Home");
  await page.keyboard.type("// typed through the block\n", { delay: 30 });
  await page.keyboard.press("Control+s");
  await sleep(1500);
  result.saved = readFileSync(path.join(proj, "auth.rs"), "utf8").startsWith("// typed through the block");
  const f = page.frames().find((f) => f.url().includes(`b-${block}-`));
  result.frame_origin = f ? new URL(f.url()).origin.replace(/-[a-z0-9]{20}\./, "-<key>.") : null;
  result.websockets = [...new Set(wsUrls.map((u) => u.replace(/-[a-z0-9]{20}\./, "-<key>.")))];
} catch (e) {
  result.error = String(e).slice(0, 400);
  await page.screenshot({ path: path.join(work, `block-${which}.png`) });
}
// What guards the server: a direct request from any local process (no cookie, no token).
const direct = await fetch(`http://127.0.0.1:${ED}/?folder=${encodeURIComponent(proj)}`);
result.direct_port_status = direct.status;
await browser.close();
daemon.kill("SIGKILL");
process.kill(-server.pid, "SIGTERM");
console.log(JSON.stringify(result, null, 1));
