// M63: huddles. The owner starts one on a session from the bar; a friend
// who drives and a watcher see it and join. Audio goes peer to peer
// (Chrome's fake microphone beeps), each person sees who's talking and who
// muted, someone whose share is revoked drops out within a second, and a
// daemon that goes away ends the huddle for everyone in it.
//
// These people come over the tailnet, with no device keys: they're marked
// unverified. Signed fingerprints are checked in huddles-control.spec.ts.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Browser, type Page } from "@playwright/test";
import { ANY, daemonPort } from "./ports";
import { labs } from "./labs";

let base = "";
const OWNER = "me@example.com";
const FRIEND = "friend@example.com";
const WATCHER = "watcher@example.com";
let daemon: ChildProcess;
let dir: string;

test.describe.configure({ mode: "serial" });
test.use({
  baseURL: async ({}, use) => use(base),
  launchOptions: { args: ["--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream", "--autoplay-policy=no-user-gesture-required"] },
});

test.beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "arugula-e2e-huddles-"));
  daemon = spawn(
    "../target/debug/arugulad",
    [
      ...["--listen", ANY, "--state-dir", labs(dir), "--owner", OWNER],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/sock"],
    ],
    { stdio: "ignore" },
  );
  base = `http://127.0.0.1:${await daemonPort(dir, daemon)}`;
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${base}/api/host`)).ok) return;
    } catch {
      // not yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
});

test.afterAll(async () => {
  // Pages left open would keep reconnecting to this daemon's port.
  for (const p of [owner, friend, watcher]) await p?.context().close();
  daemon?.kill("SIGKILL");
  rmSync(dir, { recursive: true, force: true });
});

const api = (path: string, body?: unknown) =>
  fetch(base + path, {
    method: body === undefined ? "GET" : "POST",
    headers: body === undefined ? {} : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });

async function person(browser: Browser, who?: string): Promise<Page> {
  const ctx = await browser.newContext({
    permissions: ["microphone"],
    ...(who ? { extraHTTPHeaders: { "tailscale-user-login": who } } : {}),
  });
  const page = await ctx.newPage();
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__arugula?.client.connected)).toBe(true);
  await expect.poll(() => page.evaluate(() => window.__arugula.client.hasCalls())).toBe(true);
  return page;
}

/** Client ids in the huddle, as this page has it. */
const members = (page: Page, session: number) =>
  page.evaluate((s) => window.__arugula.client.call(s)?.members.map((m) => m.client) ?? [], session);

/** How this page's connection to each other member is doing. */
const peers = (page: Page) =>
  page.evaluate(() => {
    const h = window.__arugula.huddle;
    const me = window.__arugula.client.clientId;
    return (h?.call()?.members ?? []).filter((m) => m.client !== me).map((m) => [h!.peer(m.client)?.state, h!.peer(m.client)?.trust]);
  });

let owner: Page;
let friend: Page;
let watcher: Page;
let session = 0;

test("the owner starts a huddle; a friend sees it and joins, and they hear each other", async ({ browser }) => {
  owner = await person(browser);
  session = await owner.evaluate(() => window.__arugula.client.state!.sessions[0].id);
  await api("/api/acl", { session, principal: `tailnet:${FRIEND}`, role: "editor" });
  await api("/api/acl", { session, principal: `tailnet:${WATCHER}`, role: "viewer" });
  friend = await person(browser, FRIEND);
  watcher = await person(browser, WATCHER);

  await owner.locator("header.bar .huddle-button").click();
  await expect(owner.locator(".huddle-bar .huddle-member")).toHaveCount(1);
  // The others see a huddle going on.
  await expect(friend.locator("header.bar .huddle-button.live .huddle-count")).toHaveText("1");
  await expect(watcher.locator("header.bar .huddle-button.live")).toBeVisible();

  await friend.locator("header.bar .huddle-button").click();
  await expect(owner.locator(".huddle-bar .huddle-member")).toHaveCount(2);
  await expect(friend.locator(".huddle-bar .huddle-member")).toHaveCount(2);
  // Connected, and over the tailnet with no device keys: unverified.
  await expect.poll(() => peers(owner), { timeout: 15_000 }).toEqual([["connected", "unverified"]]);
  await expect.poll(() => peers(friend), { timeout: 15_000 }).toEqual([["connected", "unverified"]]);

  // Chrome's fake microphone beeps: each sees the other talking.
  const friendId = await friend.evaluate(() => window.__arugula.client.clientId);
  const ownerId = await owner.evaluate(() => window.__arugula.client.clientId);
  await expect(owner.locator(`.huddle-member[data-member="${friendId}"][data-speaking]`)).toBeVisible({ timeout: 10_000 });
  await expect(friend.locator(`.huddle-member[data-member="${ownerId}"][data-speaking]`)).toBeVisible({ timeout: 10_000 });
  // Audio really arrives: bytes on the inbound RTP stream.
  const bytes = () =>
    owner.evaluate(async (id) => {
      const pc = (window.__arugula.huddle as unknown as { peers: Map<number, { pc: RTCPeerConnection }> }).peers.get(id)!.pc;
      let n = 0;
      (await pc.getStats()).forEach((s) => {
        if (s.type === "inbound-rtp" && s.kind === "audio") n = s.bytesReceived;
      });
      return n;
    }, friendId);
  await expect.poll(bytes, { timeout: 10_000 }).toBeGreaterThan(1000);
});

test("a watcher joins too, from the chat view's channel, and a mute shows for everyone", async () => {
  // The chat view shows the huddle on the session's channel, and its
  // header joins it.
  await watcher.evaluate(() => (location.hash = "#chat"));
  // Its channel is under Huddles at the top of the sidebar (M73), too.
  const row = watcher.locator(`[data-chat-huddles] .chat-row[data-chat-thread="session-${session}"]`);
  await expect(row.locator(".huddle-chip .avatar")).toHaveCount(2);
  await row.click();
  await watcher.locator(".chat-head .huddle-button", { hasText: "Join huddle" }).click();
  await expect(watcher.locator(".chat-head .huddle-button.here")).toContainText("In huddle");
  await watcher.keyboard.press("Escape");
  for (const p of [owner, friend, watcher]) await expect(p.locator(".huddle-bar .huddle-member")).toHaveCount(3);
  await expect.poll(() => peers(watcher), { timeout: 15_000 }).toEqual([
    ["connected", "unverified"],
    ["connected", "unverified"],
  ]);

  await friend.locator("[data-huddle-mute]").click();
  const friendId = await friend.evaluate(() => window.__arugula.client.clientId);
  await expect(owner.locator(`.huddle-member[data-member="${friendId}"] .huddle-muted`)).toBeVisible();
  await expect(watcher.locator(`.huddle-member[data-member="${friendId}"] .huddle-muted`)).toBeVisible();
  // Muted: the friend's own mic track is off.
  expect(await friend.evaluate(() => window.__arugula.huddle!.muted)).toBe(true);
  await friend.keyboard.press("Control+Shift+Space");
  await expect(owner.locator(`.huddle-member[data-member="${friendId}"] .huddle-muted`)).toHaveCount(0);
});

test("a watcher whose share is revoked drops out within a second", async () => {
  const at = Date.now();
  await api("/api/acl", { session, principal: `tailnet:${WATCHER}`, role: null });
  await expect.poll(() => members(owner, session), { timeout: 1000 }).toHaveLength(2);
  expect(Date.now() - at).toBeLessThan(1000);
  // The watcher hangs up on their side too.
  await expect(watcher.locator(".huddle-bar.ended")).toBeVisible();
  expect(await watcher.evaluate(() => window.__arugula.huddle?.live())).toBe(false);
  await expect.poll(() => peers(owner)).toEqual([["connected", "unverified"]]);
});

test("leaving, and the last one out ends it", async () => {
  await friend.locator("[data-huddle-leave]").click();
  await expect(friend.locator(".huddle-bar.ended")).toContainText("You left");
  await expect(owner.locator(".huddle-bar .huddle-member")).toHaveCount(1);
  // The session menu offers it again.
  await friend.locator(".huddle-bar.ended button", { hasText: "Join again" }).click();
  await expect(owner.locator(".huddle-bar .huddle-member")).toHaveCount(2);
  await expect.poll(() => peers(owner), { timeout: 15_000 }).toEqual([["connected", "unverified"]]);
});

test("the Linux app's page runs the call through the app's commands", async ({ browser }) => {
  // The Linux app has no RTCPeerConnection: its page hands descriptions to
  // the app (crates/desktop/src/calls.rs). This stands in for the app with
  // the browser's own WebRTC behind the same commands.
  const ctx = await browser.newContext({ permissions: ["microphone"] });
  await ctx.addInitScript(() => {
    const RTC = window.RTCPeerConnection;
    delete (window as { RTCPeerConnection?: unknown }).RTCPeerConnection;
    (window as unknown as { __arugulaApp: unknown }).__arugulaApp = { name: "test", platform: "linux", nativeCalls: true };
    const peers = new Map<number, RTCPeerConnection>();
    const calls: string[] = [];
    let stream: MediaStream | undefined;
    const gather = (pc: RTCPeerConnection) =>
      new Promise<void>((done) => {
        if (pc.iceGatheringState === "complete") return done();
        pc.addEventListener("icegatheringstatechange", () => pc.iceGatheringState === "complete" && done());
        setTimeout(done, 3000);
      });
    const describe = async (pc: RTCPeerConnection, d: RTCSessionDescriptionInit) => {
      await pc.setLocalDescription(d);
      await gather(pc);
      return pc.localDescription!.sdp;
    };
    type A = { id: number; ice: RTCIceServer[]; offer: boolean; kind: RTCSdpType; sdp: string; muted: boolean };
    const invoke = async (cmd: string, a: A) => {
      calls.push(cmd);
      switch (cmd) {
        case "call_native_start":
          stream = await navigator.mediaDevices.getUserMedia({ audio: true });
          return;
        case "call_native_peer": {
          const pc = new RTC({ iceServers: a.ice });
          for (const t of stream!.getTracks()) pc.addTrack(t, stream!);
          peers.get(a.id)?.close();
          peers.set(a.id, pc);
          return a.offer ? describe(pc, await pc.createOffer()) : null;
        }
        case "call_native_remote": {
          const pc = peers.get(a.id)!;
          await pc.setRemoteDescription({ type: a.kind, sdp: a.sdp });
          return a.kind === "offer" ? describe(pc, await pc.createAnswer()) : null;
        }
        case "call_native_drop":
          peers.get(a.id)?.close();
          peers.delete(a.id);
          return;
        case "call_native_mute":
          for (const t of stream!.getAudioTracks()) t.enabled = !a.muted;
          return;
        case "call_native_stop":
          for (const pc of peers.values()) pc.close();
          peers.clear();
          for (const t of stream?.getTracks() ?? []) t.stop();
          return;
        case "call_native_status":
          return { me: 0.1, peers: [...peers].map(([id, pc]) => ({ id, state: pc.connectionState, level: 0.1 })) };
      }
    };
    Object.assign(window, { __TAURI__: { core: { invoke } }, __nativeCalls: calls });
  });
  const app = await ctx.newPage();
  await app.goto("/");
  await expect.poll(() => app.evaluate(() => window.__arugula?.client.connected)).toBe(true);
  await expect.poll(() => app.evaluate(() => window.__arugula.client.hasCalls())).toBe(true);
  expect(await app.evaluate(() => typeof RTCPeerConnection)).toBe("undefined");

  // The owner and the friend are still in the huddle; the app joins, and
  // offers to both.
  await app.locator("header.bar .huddle-button").click();
  await expect.poll(() => peers(app), { timeout: 15_000 }).toEqual([
    ["connected", "unverified"],
    ["connected", "unverified"],
  ]);
  await expect.poll(() => peers(owner), { timeout: 15_000 }).toContainEqual(["connected", "unverified"]);
  const calls = () => app.evaluate(() => (window as unknown as { __nativeCalls: string[] }).__nativeCalls);
  expect(await calls()).toEqual(expect.arrayContaining(["call_native_start", "call_native_peer", "call_native_remote", "call_native_status"]));
  // Levels from the app light the rings.
  await expect(app.locator(".huddle-member[data-speaking]").first()).toBeVisible();

  await app.locator("[data-huddle-mute]").click();
  await expect.poll(calls).toContain("call_native_mute");
  await app.locator("[data-huddle-leave]").click();
  await expect.poll(calls).toContain("call_native_stop");
  await ctx.close();
});

test("the machine going away ends the huddle for everyone in it", async () => {
  daemon.kill("SIGKILL");
  await expect(owner.locator(".huddle-bar.ended")).toContainText("dropped", { timeout: 10_000 });
  await expect(friend.locator(".huddle-bar.ended")).toContainText("dropped", { timeout: 10_000 });
});
