// S17: openvscode-server vs code-server on this machine: cold start, warm start, memory.
//
//   node spikes/s17-editors/servers.mjs <ovs-dir> <cs-dir> [runs] > spikes/s17-editors/work/servers.json
//
// For each server and run: start it on a throwaway loopback port with fresh (cold) or reused (warm)
// data dirs and no auth of its own, time until `/` answers, then open the workbench in headless
// Chrome on work/proj and time until the explorer shows hello.py and an editor shows its text.
// Memory is PSS summed over the server's process tree (/proc/<pid>/smaps_rollup), measured with
// the server alone, with one client attached (5 s settle), and 15 s after the client left.
// Nothing is written outside spikes/s17-editors/work (HOME-like dirs are redirected there).

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const tryReq = (p) => {
  try {
    return createRequire(p)("@playwright/test");
  } catch {
    return null;
  }
};
const { chromium } =
  tryReq(path.join(here, "../../web/package.json")) ?? tryReq("/home/me/dev/jhgaylor/illogical/web/package.json");

const [ovsDir, csDir, runsArg] = process.argv.slice(2);
const RUNS = Number(runsArg ?? 3);
const work = path.join(here, "work");
const proj = path.join(work, "proj");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function tree(root) {
  const kids = new Map();
  for (const d of readdirSync("/proc")) {
    if (!/^\d+$/.test(d)) continue;
    try {
      const st = readFileSync(`/proc/${d}/stat`, "utf8");
      const ppid = Number(st.slice(st.lastIndexOf(")") + 2).split(" ")[1]);
      if (!kids.has(ppid)) kids.set(ppid, []);
      kids.get(ppid).push(Number(d));
    } catch {}
  }
  const out = [];
  const walk = (p) => {
    out.push(p);
    for (const k of kids.get(p) ?? []) walk(k);
  };
  walk(root);
  return out;
}

function pssMB(root) {
  let kb = 0;
  const procs = [];
  for (const p of tree(root)) {
    try {
      const m = readFileSync(`/proc/${p}/smaps_rollup`, "utf8").match(/^Pss:\s+(\d+)/m);
      const cmd = readFileSync(`/proc/${p}/cmdline`, "utf8")
        .split("\0")
        .map((a) => (a.startsWith("/") ? path.basename(a) : a))
        .filter((a) => a && !a.startsWith("--user-data-dir") && !a.startsWith("--extensions-dir"))
        .slice(0, 4)
        .join(" ");
      if (m) {
        kb += Number(m[1]);
        procs.push({ pid: p, mb: Math.round(Number(m[1]) / 1024), cmd: cmd.slice(0, 120) });
      }
    } catch {}
  }
  return { mb: Math.round(kb / 1024), procs };
}

const servers = {
  openvscode: (port, dir) => ({
    cmd: path.join(ovsDir, "bin/openvscode-server"),
    args: [
      "--host", "127.0.0.1", "--port", String(port), "--without-connection-token",
      "--server-data-dir", path.join(dir, "server"), "--user-data-dir", path.join(dir, "user"),
      "--extensions-dir", path.join(dir, "ext"), "--telemetry-level", "off", "--accept-server-license-terms",
    ],
    settings: [path.join(dir, "server/data/User/settings.json"), path.join(dir, "user/User/settings.json")],
  }),
  "code-server": (port, dir) => ({
    cmd: path.join(csDir, "bin/code-server"),
    args: [
      "--bind-addr", `127.0.0.1:${port}`, "--auth", "none", "--disable-telemetry", "--disable-update-check",
      "--disable-workspace-trust", "--user-data-dir", path.join(dir, "user"), "--extensions-dir", path.join(dir, "ext"),
      "--config", path.join(dir, "config.yaml"),
    ],
    settings: [path.join(dir, "user/User/settings.json")],
  }),
  // The same, with VS Code's built-in chat/agent features off (1.14x starts an agent host for them).
  "code-server-noai": (port, dir) => ({ ...servers["code-server"](port, dir), extra: { "chat.disableAIFeatures": true } }),
};

async function waitHttp(url, t0, limit = 60000) {
  while (performance.now() - t0 < limit) {
    try {
      const r = await fetch(url, { redirect: "manual" });
      if (r.status < 500) return performance.now() - t0;
    } catch {}
    await sleep(25);
  }
  throw new Error(`no answer from ${url}`);
}

const browser = await chromium.launch({ channel: "chrome" });
const results = [];
let port = 7811;
for (const [name, mk] of Object.entries(servers)) {
  const dir = path.join(work, `srv-${name}`);
  rmSync(dir, { recursive: true, force: true });
  for (let run = 0; run < RUNS + 1; run++) {
    const cold = run === 0;
    const p = port++;
    const s = mk(p, dir);
    mkdirSync(dir, { recursive: true });
    for (const f of s.settings) {
      mkdirSync(path.dirname(f), { recursive: true });
      if (!existsSync(f)) writeFileSync(f, JSON.stringify({ "security.workspace.trust.enabled": false, "workbench.startupEditor": "none", "telemetry.telemetryLevel": "off", ...(s.extra ?? {}) }));
    }
    const env = { ...process.env, HOME: path.join(dir, "home"), XDG_CONFIG_HOME: path.join(dir, "xdg-config"), XDG_DATA_HOME: path.join(dir, "xdg-data"), XDG_CACHE_HOME: path.join(dir, "xdg-cache") };
    const t0 = performance.now();
    const child = spawn(s.cmd, s.args, { env, stdio: ["ignore", "pipe", "pipe"], detached: true });
    let log = "";
    child.stdout.on("data", (d) => (log += d));
    child.stderr.on("data", (d) => (log += d));
    const r = { server: name, run, cold, port: p };
    try {
      r.http_ms = Math.round(await waitHttp(`http://127.0.0.1:${p}/`, t0));
      await sleep(1000);
      r.mem_server_only = pssMB(child.pid).mb;
      const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const page = await ctx.newPage();
      const bytes = { n: 0, reqs: 0 };
      page.on("requestfinished", async (req) => {
        bytes.reqs++;
        try {
          const size = (await req.sizes()).responseBodySize; // as sent (compressed when it was)
          bytes.n += size;
          if (size > (bytes.biggest?.size ?? 0)) {
            const h = await (await req.response()).allHeaders();
            bytes.biggest = { url: req.url().split("/").pop().slice(0, 60), size, encoding: h["content-encoding"] ?? "none" };
          }
        } catch {}
      });
      const tp = performance.now();
      await page.goto(`http://127.0.0.1:${p}/?folder=${encodeURIComponent(proj)}`);
      await page.locator(".monaco-workbench").waitFor({ timeout: 60000 });
      r.workbench_ms = Math.round(performance.now() - tp);
      await page.locator(".explorer-folders-view .monaco-list-row", { hasText: "hello.py" }).first().waitFor({ timeout: 60000 });
      r.explorer_ms = Math.round(performance.now() - tp);
      r.cold_total_ms = Math.round(performance.now() - t0);
      // A modal (workspace trust on openvscode-server, which has no --disable-workspace-trust and
      // didn't take the setting from settings.json) blocks clicks: note it and trust the folder.
      await sleep(300);
      const dialog = page.locator(".monaco-dialog-box");
      if (await dialog.count()) {
        r.dialog = (await dialog.innerText()).replace(/\s+/g, " ").slice(0, 160);
        await dialog.locator(".monaco-button", { hasText: "Yes, I trust" }).first().click();
      }
      await page.locator(".explorer-folders-view .monaco-list-row", { hasText: "hello.py" }).first().click();
      await page.locator(".monaco-editor .view-lines", { hasText: "greet" }).first().waitFor({ timeout: 30000 });
      r.file_open_ms = Math.round(performance.now() - tp);
      r.page_bytes_kb = Math.round(bytes.n / 1024);
      r.page_requests = bytes.reqs;
      r.biggest_response = bytes.biggest;
      await sleep(5000);
      const m = pssMB(child.pid);
      r.mem_with_client = m.mb;
      r.procs_with_client = m.procs;
      await ctx.close();
      await sleep(15000);
      r.mem_after_client_left = pssMB(child.pid).mb;
    } catch (e) {
      r.error = String(e);
      r.log = log.slice(-2000);
    }
    process.kill(-child.pid, "SIGTERM");
    await sleep(1500);
    try {
      process.kill(-child.pid, "SIGKILL");
    } catch {}
    results.push(r);
    console.error(JSON.stringify({ ...r, procs_with_client: undefined }));
  }
}
await browser.close();
console.log(JSON.stringify(results, null, 1));
