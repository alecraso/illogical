// S17: where does a workspace extension run, and can it reach illogicald's unix socket there?
//
//   node spikes/s17-editors/remote_probe.mjs <openvscode|code-server|serve-web> <server-dir> <host|container|container-sock>
//
// host            the server runs on this machine (the shape of Remote-SSH: the browser is the UI,
//                 the server's extension host runs the workspace extension on the server's machine)
// container       the server runs in a Docker container (the shape of a dev container), with the
//                 workspace mounted but not the socket's directory
// container-sock  the same, with the socket's directory bind-mounted into the container
//
// The probe extension (probe-ext) connects to $ILLOGICAL_PROBE_SOCK and says hello (hostname,
// pid, remoteName, whether it's in a container); then a few keystrokes should arrive as events.
// If it can't connect it leaves .probe-error.json in the workspace. Prints what arrived.

import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire("/home/me/dev/jhgaylor/illogical/web/package.json");
const { chromium } = require("@playwright/test");
const [which, dir, where] = process.argv.slice(2);
const work = path.join(here, "work");
const py = path.join(work, "venv/bin/python");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const port = 7851;
const tag = `${which}-${where}`;

const proj = path.join(work, `rp-${tag}`);
rmSync(proj, { recursive: true, force: true });
mkdirSync(proj, { recursive: true });
writeFileSync(path.join(proj, "app.ts"), "// probe\n");
const sdir = path.join(work, `rp-srv-${tag}`);
rmSync(sdir, { recursive: true, force: true });
mkdirSync(path.join(sdir, "home"), { recursive: true });
const sockDir = "/tmp/s17-sock"; // unix socket paths max out at 108 bytes
mkdirSync(sockDir, { recursive: true });
const sock = path.join(sockDir, `probe-${tag}.sock`);
const out = path.join(work, `rp-${tag}.jsonl`);
rmSync(out, { force: true });
const vsix = execFileSync(py, [path.join(here, "pack_vsix.py")]).toString().trim();

let bin, args;
const ext = path.join(sdir, "ext");
const inside = where !== "host";
const bind = inside ? "0.0.0.0" : "127.0.0.1";
if (which === "openvscode") {
  bin = path.join(dir, "bin/openvscode-server");
  const common = ["--server-data-dir", path.join(sdir, "server"), "--user-data-dir", path.join(sdir, "user"), "--extensions-dir", ext];
  execFileSync(bin, [...common, "--install-extension", vsix], { env: { ...process.env, HOME: path.join(sdir, "home") } });
  args = ["--host", bind, "--port", String(port), "--without-connection-token", "--accept-server-license-terms", "--telemetry-level", "off", ...common];
} else if (which === "code-server") {
  bin = path.join(dir, "bin/code-server");
  const common = ["--user-data-dir", path.join(sdir, "user"), "--extensions-dir", ext, "--config", path.join(sdir, "config.yaml")];
  execFileSync(bin, [...common, "--install-extension", vsix], { env: { ...process.env, HOME: path.join(sdir, "home"), XDG_DATA_HOME: path.join(sdir, "xdg"), XDG_CONFIG_HOME: path.join(sdir, "xdgc") } });
  args = ["--bind-addr", `${bind}:${port}`, "--auth", "none", "--disable-telemetry", "--disable-update-check", "--disable-workspace-trust", ...common];
} else {
  // Microsoft's own server, as Remote-SSH installs it, through `code serve-web` (downloads it on first use).
  bin = "code";
  args = ["serve-web", "--host", bind, "--port", String(port), "--without-connection-token", "--accept-server-license-terms", "--disable-telemetry", "--server-data-dir", path.join(sdir, "server"), "--cli-data-dir", path.join(sdir, "cli")];
}

const listener = spawn(py, [path.join(here, "listen.py"), sock, out], { stdio: "inherit" });
await sleep(300);
const env = { ...process.env, HOME: path.join(sdir, "home"), XDG_DATA_HOME: path.join(sdir, "xdg"), XDG_CONFIG_HOME: path.join(sdir, "xdgc"), ILLOGICAL_PROBE_SOCK: sock };
let server;
const name = `s17-probe-${process.pid}`;
if (!inside) {
  server = spawn(bin, args, { env, stdio: "ignore", detached: true });
} else {
  const mounts = [dir, sdir, proj, path.dirname(vsix)].flatMap((m) => ["-v", `${m}:${m}`]);
  if (where === "container-sock") mounts.push("-v", `${sockDir}:${sockDir}`);
  const envs = Object.entries({ HOME: env.HOME, XDG_DATA_HOME: env.XDG_DATA_HOME, XDG_CONFIG_HOME: env.XDG_CONFIG_HOME, ILLOGICAL_PROBE_SOCK: sock }).flatMap(([k, v]) => ["-e", `${k}=${v}`]);
  execFileSync("docker", ["run", "-d", "--rm", "--name", name, "--user", `${process.getuid()}:${process.getgid()}`, "-p", `127.0.0.1:${port}:${port}`, ...mounts, ...envs, "debian:bookworm-slim", bin, ...args]);
}
process.on("exit", () => {
  for (const f of [() => listener.kill(), () => !inside && process.kill(-server.pid, "SIGTERM"), () => inside && execFileSync("docker", ["rm", "-f", name], { stdio: "ignore" })]) {
    try {
      f();
    } catch {}
  }
});
let up = false;
for (let i = 0; i < 600 && !up; i++) {
  try {
    await fetch(`http://127.0.0.1:${port}/`);
    up = true;
  } catch {
    await sleep(100);
  }
}

if (which === "serve-web") {
  // The first request downloads Microsoft's server build; install the probe with its own CLI.
  const t = Date.now();
  await fetch(`http://127.0.0.1:${port}/`);
  let found = "";
  while (!found && Date.now() - t < 300000) {
    found = execFileSync("find", [sdir, "-path", "*/bin/code-server", "-not", "-path", "*staging*", "-type", "f"]).toString().trim().split("\n")[0];
    if (!found) await sleep(1000);
  }
  await sleep(2000);
  console.error(`serve-web: server ready to install into after ${Date.now() - t} ms, at ${found}`);
  execFileSync(found, ["--install-extension", vsix, "--extensions-dir", path.join(sdir, "server/extensions")], { env });
}
const browser = await chromium.launch({ channel: "chrome" });
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
const result = { server: which, where, up };
try {
  await page.goto(`http://127.0.0.1:${port}/?folder=${encodeURIComponent(proj)}`);
  await page.locator(".explorer-folders-view .monaco-list-row", { hasText: "app.ts" }).first().waitFor({ timeout: 90000 });
  await sleep(500);
  const dialog = page.locator(".monaco-dialog-box");
  if (await dialog.count()) await dialog.locator(".monaco-button", { hasText: "Yes, I trust" }).first().click();
  if (which === "serve-web") {
    // The probe was installed into a running server: reload so a fresh extension host picks it up.
    await sleep(2000);
    await page.reload();
    await page.locator(".explorer-folders-view .monaco-list-row", { hasText: "app.ts" }).first().waitFor({ timeout: 90000 });
  }
  await page.locator(".explorer-folders-view .monaco-list-row", { hasText: "app.ts" }).first().click();
  await page.locator(".monaco-editor .view-lines").first().click();
  await sleep(4000);
  await page.keyboard.type("let x = 1;", { delay: 80 });
  await sleep(2000);
} catch (e) {
  result.error = String(e).slice(0, 300);
}
await browser.close();
if (inside) execFileSync("docker", ["rm", "-f", name], { stdio: "ignore" });
else process.kill(-server.pid, "SIGTERM");
listener.kill();
await sleep(500);
const got = existsSync(out) ? readFileSync(out, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)) : [];
result.hello = got.find((e) => e.ev === "hello") ?? null;
result.events = got.filter((e) => !["hello", "ready", "bye"].includes(e.ev)).length;
const errf = path.join(proj, ".probe-error.json");
result.probe_error = existsSync(errf) ? JSON.parse(readFileSync(errf, "utf8")) : null;
console.log(JSON.stringify(result, null, 1));
