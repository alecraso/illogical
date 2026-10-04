// S17: what a VS Code extension sees while someone types, and whether a workspace extension on
// a server reaches a unix socket on that server's machine.
//
//   node spikes/s17-editors/vscode_events.mjs <openvscode|code-server> <server-dir> [cps] [seconds]
//
// Packs probe-ext (pack_vsix.py), installs it into a throwaway extensions dir, starts listen.py on
// work/probe.sock, starts the server on a loopback port with ILLOGICAL_PROBE_SOCK pointing at it,
// opens work/vsproj in headless Chrome, and types TypeScript into app.ts at <cps> characters a
// second (jittered) with arrow-key motion, a shift-selection, page scrolling and a save every
// ~20 s. The extension's raw events land in work/vscode-<server>.jsonl; replay.py turns them into
// rates per policy (printed as JSON).

import { execFileSync, spawn } from "node:child_process";
import { mkdirSync, readFileSync, rmSync, writeFileSync, existsSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire("/home/me/dev/jhgaylor/illogical/web/package.json");
const { chromium } = require("@playwright/test");
const [which, dir, cpsArg, secsArg] = process.argv.slice(2);
const CPS = Number(cpsArg ?? 8);
const SECS = Number(secsArg ?? 60);
const work = path.join(here, "work");
const py = path.join(work, "venv/bin/python");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let seed = 17;
const rand = () => ((seed = (seed * 1103515245 + 12345) % 2 ** 31) / 2 ** 31);

const proj = path.join(work, "vsproj");
rmSync(proj, { recursive: true, force: true });
mkdirSync(proj, { recursive: true });
// One deliberate error, so the run starts only once the TypeScript server is answering.
writeFileSync(path.join(proj, "app.ts"), 'const broken: number = "x";\n' + Array.from({ length: 120 }, (_, i) => `// line ${i + 1}`).join("\n") + "\n");
writeFileSync(path.join(proj, "tsconfig.json"), JSON.stringify({ compilerOptions: { strict: true, target: "es2022", noEmit: true } }));

const vsix = execFileSync(py, [path.join(here, "pack_vsix.py")]).toString().trim();
const sdir = path.join(work, `ev-${which}`);
rmSync(sdir, { recursive: true, force: true });
mkdirSync(sdir, { recursive: true });
const env = { ...process.env, HOME: path.join(sdir, "home"), XDG_CONFIG_HOME: path.join(sdir, "xdg"), XDG_DATA_HOME: path.join(sdir, "xdgd"), ILLOGICAL_PROBE_SOCK: "/tmp/s17-sock/probe.sock" /* unix socket paths max out at 108 bytes */ };
const settings = JSON.stringify({
  "security.workspace.trust.enabled": false,
  "editor.autoClosingBrackets": "never",
  "editor.autoClosingQuotes": "never",
  "editor.autoIndent": "none",
  "editor.quickSuggestions": { other: false, comments: false, strings: false },
  "editor.suggestOnTriggerCharacters": false,
  "editor.parameterHints.enabled": false,
  "workbench.startupEditor": "none",
});
const port = which === "openvscode" ? 7831 : 7832;
let bin, args, installArgs;
if (which === "openvscode") {
  bin = path.join(dir, "bin/openvscode-server");
  const common = ["--server-data-dir", path.join(sdir, "server"), "--user-data-dir", path.join(sdir, "user"), "--extensions-dir", path.join(sdir, "ext")];
  installArgs = [...common, "--install-extension", vsix];
  args = ["--host", "127.0.0.1", "--port", String(port), "--without-connection-token", "--telemetry-level", "off", "--accept-server-license-terms", ...common];
  for (const f of ["server/data/User/settings.json", "user/User/settings.json"]) {
    mkdirSync(path.dirname(path.join(sdir, f)), { recursive: true });
    writeFileSync(path.join(sdir, f), settings);
  }
} else {
  bin = path.join(dir, "bin/code-server");
  const common = ["--user-data-dir", path.join(sdir, "user"), "--extensions-dir", path.join(sdir, "ext"), "--config", path.join(sdir, "config.yaml")];
  installArgs = [...common, "--install-extension", vsix];
  args = ["--bind-addr", `127.0.0.1:${port}`, "--auth", "none", "--disable-telemetry", "--disable-update-check", "--disable-workspace-trust", ...common];
  mkdirSync(path.join(sdir, "user/User"), { recursive: true });
  writeFileSync(path.join(sdir, "user/User/settings.json"), settings);
}
console.error(execFileSync(bin, installArgs, { env }).toString().trim().split("\n").slice(-1)[0]);

const out = path.join(work, `vscode-${which}.jsonl`);
rmSync(out, { force: true });
const listener = spawn(py, [path.join(here, "listen.py"), env.ILLOGICAL_PROBE_SOCK, out], { stdio: "inherit" });
await sleep(500);
const server = spawn(bin, args, { env, stdio: "ignore", detached: true });
for (let i = 0; i < 200; i++) {
  try {
    await fetch(`http://127.0.0.1:${port}/`);
    break;
  } catch {
    await sleep(50);
  }
}

const browser = await chromium.launch({ channel: "chrome" });
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
await page.goto(`http://127.0.0.1:${port}/?folder=${encodeURIComponent(proj)}`);
await page.locator(".explorer-folders-view .monaco-list-row", { hasText: "app.ts" }).first().waitFor({ timeout: 60000 });
await sleep(500);
const dialog = page.locator(".monaco-dialog-box");
if (await dialog.count()) await dialog.locator(".monaco-button", { hasText: "Yes, I trust" }).first().click();
await page.locator(".explorer-folders-view .monaco-list-row", { hasText: "app.ts" }).first().click();
await page.locator(".monaco-editor .view-lines", { hasText: "line 1" }).first().waitFor();
await page.locator(".monaco-editor .view-lines").first().click();
const lines = () => readFileSync(out, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l));
// Wait for the TypeScript server's first diagnostics (the deliberate error), up to 2 minutes.
const tsWait = Date.now();
while (Date.now() - tsWait < 120000 && !(existsSync(out) && lines().some((e) => e.ev === "diagnostics" && e.e > 0))) await sleep(500);
console.error(`first diagnostics after ${Date.now() - tsWait} ms`);
const startCount = existsSync(out) ? lines().length : 0;

const CODE = `
interface Item { key: string; value: number }

export function parse(text: string): Item | null {
  const [head, rest] = text.split(":");
  if (!rest) return null;
  return { key: head.trim(), value: Number(rest) };
}

export class Store {
  private items = new Map<string, number>();
  load(lines: string[]): number {
    for (const line of lines) {
      const kv = parse(line);
      if (kv) this.items.set(kv.key, kv.value);
    }
    return this.items.size;
  }
  get(key: string): number | undefined {
    return this.items.get(key);
  }
}
`;
await page.keyboard.press("Control+End");
const t0 = Date.now();
let typed = 0;
let lastSave = t0;
let pos = 0;
while (Date.now() - t0 < SECS * 1000) {
  const ch = CODE[pos++ % CODE.length];
  await page.keyboard.press(ch === "\n" ? "Enter" : ch === " " ? "Space" : ch);
  typed++;
  await sleep((0.4 + 1.2 * rand()) * (1000 / CPS));
  if (ch === "\n" && rand() < 0.12) {
    for (const k of ["ArrowUp", "ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", "Control+ArrowLeft"].sort(() => rand() - 0.5).slice(0, 6)) {
      await page.keyboard.press(k);
      await sleep(150 + 250 * rand());
    }
    for (let i = 0; i < 3; i++) {
      await page.keyboard.press("Shift+ArrowDown");
      await sleep(200);
    }
    await page.keyboard.press("Control+End");
  }
  if (Date.now() - lastSave > 20000) {
    await page.keyboard.press("Control+s");
    lastSave = Date.now();
  }
}
const elapsed = (Date.now() - t0) / 1000;
await sleep(2000);
await browser.close();
process.kill(-server.pid, "SIGTERM");
listener.kill();

const all = lines();
const hello = all.find((e) => e.ev === "hello");
const evs = all.slice(startCount).filter((e) => !["hello", "ready", "bye"].includes(e.ev));
const t = evs[0]?.t ?? 0;
writeFileSync(path.join(work, `vscode-events-${which}.json`), JSON.stringify(evs));
const res = execFileSync(py, ["-c", `
import json, sys
sys.path.insert(0, ${JSON.stringify(here)})
from replay import all_policies, rates
evs = json.load(open(${JSON.stringify(path.join(work, `vscode-events-${which}.json`))}))
el = ${elapsed}
print(json.dumps({"events_per_s": round(len(evs) / el, 2), "by_event_per_s": rates(evs, el), "policies": all_policies(evs, el)}))
`]).toString();
console.log(JSON.stringify({ server: which, hello, cps_target: CPS, typed, seconds: elapsed, cps_actual: Math.round((typed / elapsed) * 100) / 100, events: evs.length, ...JSON.parse(res) }, null, 1));
