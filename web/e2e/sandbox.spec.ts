// M4a's "done when", for real: a throwaway wisp sprite installs the static
// daemon with `illogicald install --tailnet`, joins the tailnet and a home
// daemon's host list (with an invite), and then, from a phone, its own URL
// gives a working vim; the home daemon's page switches to it too.
//
// Needs a tailnet auth key (ephemeral, tag:sandbox) in a file, wispd and
// its token, this machine on the tailnet, and `just static`:
//
//   ILLOGICAL_E2E_TAILNET_AUTHKEY_FILE=~/.config/illogical/tailnet-authkey just e2e-sandbox
//
// A single-use key is spent by one run. ILLOGICAL_E2E_SANDBOX=NAME instead
// reuses a sprite that already joined (and keeps it): install runs again,
// which needs no key once tailscaled is logged in.
//
// Skips without them. The key goes to the sprite in a request body (never
// on a command line) and is deleted there after use.

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { devices, expect, test, type Page } from "@playwright/test";
import { active, ready, run } from "./helpers";
import { daemonPort } from "./ports";
import { labs } from "./labs";

const WISP = process.env.ILLOGICAL_WISP_URL ?? "http://127.0.0.1:7788";
const STATIC = "../target/x86_64-unknown-linux-musl/release";
const keyFile = process.env.ILLOGICAL_E2E_TAILNET_AUTHKEY_FILE ?? "";
const reuse = process.env.ILLOGICAL_E2E_SANDBOX ?? "";
const read = (path: string) => {
  try {
    return readFileSync(path);
  } catch {
    return null;
  }
};
const wispToken = read(process.env.ILLOGICAL_WISP_TOKEN_FILE ?? `${homedir()}/.local/share/wisp/token`)?.toString().trim() ?? "";
const authkey = keyFile ? read(keyFile.replace(/^~/, homedir())) : null;
const tailnetIp = (() => {
  try {
    return execFileSync("tailscale", ["ip", "-4"], { encoding: "utf8" }).trim().split("\n")[0];
  } catch {
    return "";
  }
})();
const missing = !authkey && !reuse
  ? "neither ILLOGICAL_E2E_TAILNET_AUTHKEY_FILE nor ILLOGICAL_E2E_SANDBOX is set"
  : !wispToken
    ? "no wisp token"
    : !tailnetIp
      ? "not on a tailnet"
      : !existsSync(`${STATIC}/illogicald`)
        ? "no static build (just static)"
        : "";

const sprite = reuse || `illogical-m4a-e2e-${Date.now().toString(36)}`;
let homeUrl = "";
const auth = { Authorization: `Bearer ${wispToken}` };
let home: ChildProcess | undefined;
let homeState = "";
let awake: WebSocket | undefined;
let sandboxUrl = "";

async function wisp(method: string, path: string, body?: BodyInit, query = "") {
  const headers = typeof body === "string" ? { ...auth, "Content-Type": "application/json" } : auth;
  const r = await fetch(`${WISP}/v1/sprites${path}${query}`, { method, headers, body });
  if (!r.ok) throw new Error(`${method} ${path}: ${r.status} ${await r.text()}`);
  return r;
}

/** A command in the sprite over an exec TTY; its output, once it exits. */
function exec(shell: string, keepOpen = false): Promise<{ out: string; ws: WebSocket }> {
  const q = ["bash", "-lc", shell].map((c) => `cmd=${encodeURIComponent(c)}`).join("&");
  const url = `${WISP.replace(/^http/, "ws")}/v1/sprites/${sprite}/exec?tty=true&${q}&max_run_after_disconnect=30s`;
  // Node's WebSocket takes headers (browsers' doesn't).
  const ws = new (WebSocket as unknown as new (u: string, o: object) => WebSocket)(url, { headers: auth });
  ws.binaryType = "arraybuffer";
  let out = "";
  return new Promise((resolve, reject) => {
    ws.onmessage = (m) => {
      if (typeof m.data === "string") {
        if (keepOpen && JSON.parse(m.data).type === "session_info") resolve({ out, ws });
      } else out += new TextDecoder().decode(m.data as ArrayBuffer);
    };
    ws.onclose = () => resolve({ out, ws });
    ws.onerror = () => reject(new Error(`exec failed: ${out}`));
  });
}

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  if (missing) return;
  test.setTimeout(300_000);
  // The home daemon, on this machine's tailnet address so the sandbox can
  // reach it (it asks tailscaled who connects).
  homeState = mkdtempSync(join(tmpdir(), "illogical-e2e-home-"));
  home = spawn(
    "../target/debug/illogicald",
    ["--listen", `${tailnetIp}:0`, "--name", "home", "--state-dir", labs(homeState), "--no-manager-env"],
    { stdio: "ignore" },
  );
  homeUrl = `http://${tailnetIp}:${await daemonPort(homeState, home)}`;
  await expect.poll(() => fetch(`${homeUrl}/api/host`).then((r) => r.ok, () => false), { timeout: 15_000 }).toBe(true);
  const invite = (await (await fetch(`${homeUrl}/api/hosts/invite`, { method: "POST" })).json()).token as string;

  if (!reuse) await wisp("POST", "", JSON.stringify({ name: sprite }));
  const put = (data: Buffer | string, path: string, mode: string) =>
    wisp("PUT", `/${sprite}/fs/write`, data, `?path=${encodeURIComponent(path)}&mode=${mode}&mkdir=true`);
  await put(readFileSync(`${STATIC}/illogicald`), "/home/sprite/illogicald", "0755");
  await put(readFileSync(`${STATIC}/illogical`), "/home/sprite/illogical", "0755");
  // Reused: already logged in, so the key isn't read.
  await put(authkey ?? "unused", "/home/sprite/.tskey", "0600");
  await put(invite, "/home/sprite/.invite", "0600");
  const { out } = await exec(
    `~/illogical install --tailnet file:$HOME/.tskey --hostname ${sprite} --home ${homeUrl} --join file:$HOME/.invite; ` +
      "rm -f ~/.tskey ~/.invite",
  );
  console.log(out);
  // install's own list ("  https://…"), not tailscale serve's output.
  sandboxUrl = /^ {2}(https:\/\/[^\s/]+)/m.exec(out)?.[1] ?? "";
  expect(sandboxUrl, out).toMatch(/^https:\/\//);
  // Sprites pause when idle, and tailnet packets don't wake them (S4):
  // keep it busy while the tests run.
  awake = (await exec("while true; do echo -n .; sleep 5; done", true)).ws;
  await expect.poll(() => fetch(`${sandboxUrl}/api/host`).then((r) => r.ok, () => false), { timeout: 60_000 }).toBe(true);
});

test.afterAll(async () => {
  awake?.close();
  if (!missing && !reuse) await wisp("DELETE", `/${sprite}`).catch(() => {});
  home?.kill("SIGKILL");
  if (homeState) rmSync(homeState, { recursive: true, force: true });
});

const connected = (page: Page) =>
  page.evaluate(() => !!window.__illogical?.client.connected && window.__illogical.client.state !== null);

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  test("the sandbox's own URL gives a working vim", async ({ page }) => {
    test.skip(!!missing, missing);
    test.setTimeout(90_000);
    await page.goto(sandboxUrl);
    await expect.poll(() => connected(page), { timeout: 30_000 }).toBe(true);
    const pane = await active(page);
    await ready(page, pane);
    await run(page, pane, "echo on-$(hostname)", `on-${sprite}`);

    await page.keyboard.type("vi /tmp/m4a.txt\n");
    await expect.poll(() => page.evaluate((p) => window.__illogical.screen(p), pane)).toContain("m4a.txt");
    await page.keyboard.type("ihello from the phone");
    await page.keyboard.press("Escape");
    await page.keyboard.type(":wq\n");
    await run(page, pane, "cat /tmp/m4a.txt; echo cat-$((2+3))", "cat-5");
    await expect.poll(() => page.evaluate((p) => window.__illogical.screen(p), pane)).toContain("hello from the phone");
  });
});

test("a new sandbox is a tagged node, not one of yours", async () => {
  // An untagged key makes the sandbox a device of yours: it could reach
  // your other machines as you. (A reused sprite is whatever it joined as.)
  test.skip(!!missing || !!reuse, missing || "reusing a sandbox");
  const status = JSON.parse(execFileSync("tailscale", ["status", "--json"], { encoding: "utf8" }));
  const peer = Object.values(status.Peer as Record<string, { HostName: string; Tags?: string[] }>).find(
    (p) => p.HostName === sprite,
  );
  expect(peer, "the sandbox is a peer").toBeDefined();
  console.log(`${sprite} tags: ${JSON.stringify(peer!.Tags ?? [])}`);
  expect(peer!.Tags ?? []).toContain("tag:sandbox");
});

test("the sandbox is on the home daemon's list, and its page switches to it", async ({ page }) => {
  test.skip(!!missing, missing);
  test.setTimeout(90_000);
  const list = await (await fetch(`${homeUrl}/api/hosts`)).json();
  const entry = list.hosts.find((h: { name: string }) => h.name === sprite);
  expect(entry.urls[0]).toBe(sandboxUrl);
  await expect
    .poll(async () => (await (await fetch(`${homeUrl}/api/hosts`)).json()).hosts[0].last_seen_ms, { timeout: 70_000 })
    .toBeGreaterThan(0);

  await page.goto(homeUrl);
  await expect.poll(() => connected(page), { timeout: 30_000 }).toBe(true);
  await page.evaluate((s) => window.__illogical.hosts.select(s), sprite);
  await expect.poll(() => page.evaluate(() => window.__illogical.client.base)).toBe(sandboxUrl);
  await expect.poll(() => connected(page), { timeout: 30_000 }).toBe(true);
  const pane = await active(page);
  await ready(page, pane);
  await run(page, pane, "echo via-home-$((7*6))", "via-home-42");
});
