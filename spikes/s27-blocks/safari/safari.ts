// S27 in real Safari (macOS, or iOS in the Simulator) through safaridriver,
// for a machine where that's set up (Track E's macOS VM). The same checks
// as the Playwright specs that matter for a go/no-go: the worker registers
// in control's cross-site frame, the page and its requests come through it,
// a WebSocket goes through the shim, control sees no plaintext, and a Vite
// save reloads the block.
//
//   node safari/safari.ts --print-setup      what the machine needs (sudo)
//   node safari/safari.ts                    Safari on this Mac
//   node safari/safari.ts --ios              Safari in the booted iOS Simulator
//   node safari/safari.ts --driver playwright-webkit
//                                            the same script in Playwright's
//                                            WebKit, to check the script here
//
// It prints one JSON verdict and exits non-zero if any check failed. Build
// first: `cargo build --release && node build.mjs && target/release/s27
// cert .run/cert` (or run the Playwright suite once).

import { writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { vite } from "../tests/devservers.ts";
import { admin, controlStats, startStack } from "../tests/servers.ts";

const args = process.argv.slice(2);
const flag = (f: string) => args.includes(f);
const PORT = Number(process.env.S27_SAFARI_PORT ?? 7753);
const KEYS = ["s27safari1", "s27safari2"];
const CERT = resolve(".run/cert/cert.pem");

if (flag("--print-setup")) {
  console.log(`# Once per machine (the VM's provisioning does this):
sudo safaridriver --enable
sudo security add-trusted-cert -d -r trustAsRoot -k /Library/Keychains/System.keychain ${CERT}
echo "127.0.0.1 control.test ${KEYS.map((k) => `b-${k}.blocks.test`).join(" ")}" | sudo tee -a /etc/hosts
# For --ios, also: xcrun simctl boot <device>; xcrun simctl keychain booted add-root-cert ${CERT}
# (the Simulator uses this Mac's resolver, so /etc/hosts covers it).`);
  process.exit(0);
}

/** What the checks need of a browser. Scripts are function bodies whose
 * last argument is a callback, as WebDriver's async execute has them. */
interface Driver {
  goto(url: string): Promise<void>;
  exec<T>(body: string, ...a: unknown[]): Promise<T>;
  /** Into the frame whose src starts with `origin`, or back to the top. */
  frame(origin: string | null): Promise<void>;
  close(): Promise<void>;
}

const ELEMENT = "element-6066-11e4-a52e-4f735466cecf";

async function webdriver(ios: boolean): Promise<Driver> {
  const { spawn } = await import("node:child_process");
  const port = 4444 + Math.floor(Math.random() * 1000);
  const proc = spawn("safaridriver", ["-p", String(port)], { stdio: "inherit" });
  const base = `http://127.0.0.1:${port}`;
  const call = async (method: string, path: string, body?: unknown) => {
    const r = await fetch(base + path, { method, headers: { "content-type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body) });
    const j = (await r.json()) as { value: any };
    if (!r.ok) throw new Error(`${method} ${path}: ${JSON.stringify(j.value)}`);
    return j.value;
  };
  for (let i = 0; ; i++) {
    try {
      await fetch(`${base}/status`);
      break;
    } catch {
      if (i > 50) throw new Error("safaridriver didn't start");
      await new Promise((r) => setTimeout(r, 100));
    }
  }
  const caps = ios ? { browserName: "Safari", platformName: "iOS", "safari:useSimulator": true } : { browserName: "safari" };
  const { sessionId } = await call("POST", "/session", { capabilities: { alwaysMatch: caps } });
  const s = `/session/${sessionId}`;
  await call("POST", `${s}/timeouts`, { script: 60_000 });
  return {
    goto: (url) => call("POST", `${s}/url`, { url }),
    exec: (body, ...a) => call("POST", `${s}/execute/async`, { script: body, args: a }),
    async frame(origin) {
      if (origin === null) return call("POST", `${s}/frame/parent`, {});
      const el = await call("POST", `${s}/element`, { using: "css selector", value: `iframe[src^="${origin}"]` });
      await call("POST", `${s}/frame`, { id: { [ELEMENT]: el[ELEMENT] } });
    },
    async close() {
      await call("DELETE", s).catch(() => {});
      proc.kill();
    },
  };
}

async function playwrightWebkit(): Promise<Driver> {
  const { webkit } = await import("@playwright/test");
  const setup = (await import("../tests/global-setup.ts")).default;
  const stopProxy = await setup();
  const browser = await webkit.launch({ proxy: { server: `http://127.0.0.1:${process.env.S27_PROXY_PORT ?? 7753}` } });
  const page = await (await browser.newContext({ ignoreHTTPSErrors: true })).newPage();
  let at: import("@playwright/test").Frame = page.mainFrame();
  return {
    goto: async (url) => void (await page.goto(url)),
    exec: (body, ...a) =>
      at.evaluate(([body, a]) => new Promise((res) => new Function(body).apply(null, [...(a as unknown[]), res])), [body, a] as const) as Promise<never>,
    async frame(origin) {
      if (origin === null) at = page.mainFrame();
      else {
        for (let i = 0; !page.frames().some((f) => f.url().startsWith(origin)); i++) {
          if (i > 300) throw new Error(`no frame at ${origin}`);
          await new Promise((r) => setTimeout(r, 100));
        }
        at = page.frames().find((f) => f.url().startsWith(origin))!;
      }
    },
    async close() {
      await browser.close();
      await stopProxy();
    },
  };
}

/** A body that runs `src` (an async function's body) and calls back with
 * its value, or {error}. */
const run = (src: string) => `const done = arguments[arguments.length - 1]; const a = Array.from(arguments).slice(0, -1);
(async () => { ${src} })().then(done, (e) => done({ error: String(e) }));`;

async function until<T>(d: Driver, src: string, ok: (v: T) => boolean, ms = 30_000): Promise<T> {
  const end = Date.now() + ms;
  let v: T;
  for (;;) {
    try {
      v = await d.exec<T>(run(src));
      if (ok(v)) return v;
    } catch {}
    if (Date.now() > end) throw new Error(`timed out; last: ${JSON.stringify(v!)}`);
    await new Promise((r) => setTimeout(r, 250));
  }
}

const pw = args.includes("--driver") && args[args.indexOf("--driver") + 1] === "playwright-webkit";
const stack = await startStack(pw ? "127.0.0.1:0" : `127.0.0.1:${PORT}`);
const v = await vite();
const verdict: Record<string, unknown> = { browser: pw ? "playwright-webkit" : flag("--ios") ? "safari-ios-simulator" : "safari-macos", at: new Date().toISOString() };
const checks: Record<string, boolean | string> = {};
const d = pw ? await playwrightWebkit() : await webdriver(flag("--ios"));
try {
  if (!pw) verdict.userAgent = await d.exec<string>(run("return navigator.userAgent"));
  await d.goto(stack.origin + "/");
  await until(d, "return !!window.s27?.ready", (x) => x === true);
  await admin(stack, "/trust", { sign: await d.exec<string>(run("return window.s27.signPub")) });

  // A plain block: the worker, a request, a WebSocket, no plaintext at control.
  const open = (key: string, block: string) =>
    d.exec<string>(run("return window.s27.openBlock(a[0]).origin"), { daemon: stack.daemon, block, key });
  await admin(stack, "/block", { id: "safari-plain", port: stack.site });
  const plain = await open(KEYS[0], "safari-plain");
  await d.frame(plain);
  try {
    await until<string>(d, "return document.querySelector('h1')?.textContent", (t) => t === "block home");
    checks.loads = true;
    checks.workerControls = await d.exec<boolean>(run("return !!navigator.serviceWorker.controller"));
    const echo = await d.exec<any>(run("const r = await fetch('/echo', { method: 'POST', body: 'hi' }); return (await r.json()).body"));
    checks.fetch = echo === "hi" || JSON.stringify(echo);
    checks.websocket =
      (await d.exec<string>(
        run(`return await new Promise((res) => { const w = new WebSocket('wss://' + location.host + '/ws');
          w.onopen = () => w.send('ping'); w.onmessage = (e) => res(e.data); w.onerror = () => res('error'); setTimeout(() => res('timeout'), 10000); })`),
      )) === "ping";
  } catch (e) {
    checks.loads = String(e);
  }
  await d.frame(null);
  checks.controlSawNoPlaintext = (await controlStats(stack)).marker_hits === 0;

  // Vite: a save reloads the block.
  await admin(stack, "/block", { id: "safari-vite", port: v.port });
  const vb = await open(KEYS[1], "safari-vite");
  await d.frame(vb);
  try {
    await until<string>(d, "return document.querySelector('#msg')?.textContent", (t) => t === "message one", 60_000);
    await new Promise((r) => setTimeout(r, 1000));
    const t = Date.now();
    writeFileSync(`${v.dir}/main.js`, `document.querySelector("#msg").textContent = "message saved";\n`);
    await until<string>(d, "return document.querySelector('#msg')?.textContent", (x) => x === "message saved");
    checks.viteReload = true;
    // From the save to the reloaded page's DOMContentLoaded, by the page's
    // own clock (not when this script happened to look).
    const loaded = await d.exec<number>(
      run("const n = performance.getEntriesByType('navigation')[0]; return performance.timeOrigin + n.domContentLoadedEventEnd"),
    );
    verdict.viteReloadMs = Math.round(loaded - t);
    verdict.viteReloadSeenMs = Date.now() - t;
  } catch (e) {
    checks.viteReload = String(e);
  }
} catch (e) {
  checks.setup = String(e);
} finally {
  await d.close();
  v.stop();
  stack.stop();
}
verdict.checks = checks;
verdict.go = Object.values(checks).every((c) => c === true);
console.log(JSON.stringify(verdict, null, 2));
writeFileSync(`.run/verdict-${verdict.browser}.json`, JSON.stringify(verdict, null, 2));
process.exit(verdict.go ? 0 : 1);
