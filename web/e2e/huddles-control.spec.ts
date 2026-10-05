// M63: huddles through control, where every device has a key. Alice's
// laptop and her second browser see each other as verified (her own
// devices), Erin (another account she shared the session with) as signed,
// and a description whose fingerprints don't match its signature is
// refused: what a daemon in the middle would have to send. The daemon gets
// TURN credentials from control, which gets them from Cloudflare (a fake
// one here).

import { createServer, type Server } from "node:http";
import { expect, test, type Browser, type Page } from "@playwright/test";
import { listen } from "./ports";
import { call, show, TeamControl } from "./team-fixture";

const t = new TeamControl("huddles");
let cf: Server;
const asked: { path: string; auth: string; body: string }[] = [];

test.describe.configure({ mode: "serial" });
test.use({
  launchOptions: { args: ["--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream", "--autoplay-policy=no-user-gesture-required"] },
});

test.beforeAll(async () => {
  cf = createServer((req, res) => {
    let body = "";
    req.on("data", (d) => (body += d));
    req.on("end", () => {
      asked.push({ path: req.url!, auth: req.headers.authorization ?? "", body });
      res.writeHead(201, { "content-type": "application/json" }).end(
        JSON.stringify({
          iceServers: [
            { urls: ["stun:stun.cloudflare.com:3478"] },
            { urls: ["turn:127.0.0.1:9?transport=udp"], username: "fake-user", credential: "fake-pass" },
          ],
        }),
      );
    });
  });
  const api = `http://127.0.0.1:${await listen(cf)}`;
  await t.start(["--turn-key-id", "key-1", "--turn-api-token", "token-1", "--turn-api", api]);
});

test.afterAll(async () => {
  for (const p of [alice, second, erin]) await p?.context().close();
  t.stop();
  cf?.closeAllConnections();
  cf?.close();
});

let alice: Page;
let second: Page;
let erin: Page;
let session = 0;

const ctxOptions = { permissions: ["microphone"] };

/** How this page's huddle sees each other member: [name, state, trust]. */
const peers = (page: Page) =>
  page.evaluate(() => {
    const h = window.__illogical.huddle;
    const me = window.__illogical.client.clientId;
    return (h?.call()?.members ?? []).filter((m) => m.client !== me).map((m) => [m.name, h!.peer(m.client)?.state, h!.peer(m.client)?.trust]);
  });

async function join(page: Page) {
  await page.evaluate((s) => {
    const b = document.querySelector<HTMLButtonElement>(`header.bar .huddle-button[data-huddle="${s}"]`);
    b!.click();
  }, session);
  await expect.poll(() => page.evaluate(() => window.__illogical.huddle?.status.kind)).toBe("live");
}

/** Alice's second browser: signed in as her, approved from her laptop. */
async function secondDevice(browser: Browser): Promise<Page> {
  const ctx = await browser.newContext({ ...ctxOptions, baseURL: t.base });
  await ctx.addCookies([{ name: "as", value: "alice", url: t.github }]);
  const page = await ctx.newPage();
  await page.goto("/");
  await page.locator("[data-signin=github]").click();
  const fp = await page.locator("[data-fingerprint]").getAttribute("data-fingerprint");
  await alice.goto("/");
  await expect(alice.locator(`[data-pending="${fp}"]`)).toBeVisible({ timeout: 20_000 });
  await alice.locator("[data-approve]").click();
  await page.waitForFunction(() => window.__illogical?.control?.phase === "ready", null, { timeout: 30_000 });
  return page;
}

test("Alice's own devices see each other as verified, over control's TURN", async ({ browser }) => {
  test.setTimeout(120_000);
  alice = await t.person(browser, "alice", ctxOptions);
  await t.addMachine(alice, "box");
  await show(alice, "box");
  session = await alice.evaluate(() => window.__illogical.client.state!.sessions[0].id);

  // The daemon asks control, which asks Cloudflare with the key.
  const ice = await call(alice, "GET", "/api/turn");
  expect(ice.body?.turn).toBe(true);
  expect(JSON.stringify(ice.body?.ice_servers)).toContain("fake-user");
  expect(asked[0].path).toBe("/v1/turn/keys/key-1/credentials/generate-ice-servers");
  expect(asked[0].auth).toBe("Bearer token-1");
  // Cached: a second ask doesn't go to Cloudflare again.
  await call(alice, "GET", "/api/turn");
  expect(asked).toHaveLength(1);

  second = await secondDevice(browser);
  await show(second, "box");
  await join(alice);
  await join(second);
  await expect.poll(() => peers(alice), { timeout: 20_000 }).toEqual([["alice", "connected", "verified"]]);
  await expect.poll(() => peers(second), { timeout: 20_000 }).toEqual([["alice", "connected", "verified"]]);
  // The member list says they came with a device key.
  expect(await alice.evaluate(() => window.__illogical.huddle!.call()!.members.every((m) => !!m.device))).toBe(true);
});

test("someone from another account is signed, not verified", async ({ browser }) => {
  test.setTimeout(120_000);
  erin = await t.person(browser, "erin", ctxOptions);
  const who = await alice.evaluate(() => window.__illogical.control!.person("erin"));
  const shared = await alice.evaluate(
    async ([c, s]) => {
      const cl = window.__illogical.client;
      return (await cl.request("POST", "/api/acl", { session: s, principal: `account:${c.account}`, role: "viewer", root: c.root, name: "erin" })).ok;
    },
    [who, session] as const,
  );
  expect(shared).toBe(true);
  await expect
    .poll(async () => (await erin.evaluate(() => window.__illogical.control!.refresh()), erin.locator("[data-share-accept]").count()), { timeout: 30_000 })
    .toBe(1);
  await erin.locator("[data-share-accept]").click();
  await show(erin, "box");
  await join(erin);
  await expect
    .poll(() => peers(erin), { timeout: 20_000 })
    .toEqual([
      ["alice", "connected", "signed"],
      ["alice", "connected", "signed"],
    ]);
  await expect.poll(() => peers(alice), { timeout: 20_000 }).toContainEqual(["erin", "connected", "signed"]);
});

test("a description whose fingerprints were swapped is refused", async () => {
  // Erin leaves, and her page swaps the fingerprint in what it sends next,
  // as a daemon in the middle would; her signature still covers the real
  // one.
  await erin.locator("[data-huddle-leave]").click();
  await erin.evaluate(() => {
    const cl = window.__illogical.client;
    const send = cl.send.bind(cl);
    cl.send = (m) => {
      if (m.type === "call_signal") m.signal.sdp = m.signal.sdp.replace(/a=fingerprint:sha-256 [0-9A-F:]+/g, "a=fingerprint:sha-256 " + "AB:".repeat(31) + "AB");
      send(m);
    };
  });
  await join(erin);
  await expect.poll(() => peers(alice), { timeout: 20_000 }).toContainEqual(["erin", "closed", "refused"]);
  await expect(alice.locator("#status")).toContainText("Refused erin's audio");
  // Alice and her second browser still hear each other.
  await expect.poll(() => peers(alice)).toContainEqual(["alice", "connected", "verified"]);
});
