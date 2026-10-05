// A block through control, end to end: the worker registers in a
// cross-site frame, the page and its requests come from the daemon over
// Noise, and control relays only ciphertext.

import { admin, controlStats, expect, MARKER, openBlock, test, workerStats } from "./stack.ts";

test("a block loads through the worker, and control can't read it", async ({ parent, stack }) => {
  const before = await controlStats(stack);
  const b = await openBlock(parent, stack);
  await expect(b.frame.locator("h1")).toHaveText("block home");
  expect(await b.frame.evaluate(() => !!navigator.serviceWorker.controller)).toBe(true);

  // The frame is a third party: its first load was a cross-site navigation.
  const after = await controlStats(stack);
  const first = after.log.find((l: any) => b.origin.includes(l.host));
  expect(first).toMatchObject({ mode: "navigate", dest: "iframe", site: "cross-site" });

  // The page's own requests: a body, a POST, a redirect, a big download.
  const r = await b.frame.evaluate(async () => {
    const echo = await (await fetch("/echo", { method: "POST", body: "hello" })).json();
    const red = await fetch("/redirect");
    const big = await (await fetch("/big?n=3000000")).arrayBuffer();
    return { echo, redirected: red.url, redStatus: red.status, big: big.byteLength };
  });
  expect(r.echo).toMatchObject({ method: "POST", body: "hello" });
  expect(r.echo.host).toMatch(/^localhost:\d+$/);
  expect(r.redirected).toBe(`${b.origin}/landed`);
  expect(r.big).toBe(3_000_000);

  // Control relayed it all and saw no plaintext: the page is full of the
  // marker, and control looks for it in every byte it relays.
  expect(after.relayed_bytes).toBeGreaterThan(before.relayed_bytes);
  const end = await controlStats(stack);
  expect(end.relayed_bytes - before.relayed_bytes).toBeGreaterThan(3_000_000);
  expect(end.marker_hits).toBe(0);
  expect(await b.frame.content()).toContain(MARKER);
});

test("a WebSocket in the block's page goes through the shim", async ({ parent, stack }) => {
  const b = await openBlock(parent, stack);
  const echoed = await b.frame.evaluate(
    () =>
      new Promise<string[]>((res, rej) => {
        const ws = new WebSocket(`wss://${location.host}/ws`);
        ws.binaryType = "arraybuffer";
        const got: string[] = [];
        ws.onopen = () => {
          ws.send("one");
          ws.send(new Uint8Array([1, 2, 3]));
        };
        ws.onmessage = (e) => {
          got.push(typeof e.data === "string" ? e.data : `bytes:${new Uint8Array(e.data).join(",")}`);
          if (got.length === 2) {
            ws.close();
            res(got);
          }
        };
        ws.onerror = () => rej(new Error("ws error"));
      }),
  );
  expect(echoed).toEqual(["one", "bytes:1,2,3"]);
  expect((await b.frame.evaluate(() => (window as any).s27shim)).sockets).toBe(1);
  expect((await admin(stack, "/stats")).sockets).toBeGreaterThan(0);
});

test("cookies: the worker keeps the jar, the page can't see it", async ({ parent, stack }) => {
  const b = await openBlock(parent, stack);
  const r = await b.frame.evaluate(async () => {
    await fetch("/cookie/set");
    const sent = await (await fetch("/cookie/get")).text();
    return { sent, visible: document.cookie };
  });
  // The server's cookie comes back to it (from the worker's jar), and, as
  // HttpOnly would have it, the page doesn't see it.
  expect(r.sent).toBe("s27=yes");
  expect(r.visible).toBe("");
});

test("grants: an unknown block, or a device the daemon doesn't trust, is refused", async ({ parent, stack, browser }) => {
  const reasons = async () => ((await admin(stack, "/stats")).refusals as string[]).join("\n");
  // A block the daemon doesn't have.
  const origin = await parent.evaluate((a) => (window as any).s27.openBlock(a).origin, { daemon: stack.daemon, block: "nope" });
  await expect.poll(reasons, { timeout: 20_000 }).toContain("no such block");
  const f = parent.frames().find((f) => f.url().startsWith(origin))!;
  await expect(f.locator("body")).toContainText("s27:");

  // Another browser's control page, whose device key nobody approved.
  const ctx = await browser.newContext();
  const stranger = await ctx.newPage();
  await stranger.goto(stack.origin + "/");
  await stranger.waitForFunction(() => (window as any).s27?.ready);
  await admin(stack, "/block", { id: "theirs", port: stack.site });
  await stranger.evaluate((a) => (window as any).s27.openBlock(a), { daemon: stack.daemon, block: "theirs" });
  await expect.poll(reasons, { timeout: 20_000 }).toContain("doesn't trust");
  await ctx.close();
  // And the trusted page still opens blocks.
  const b = await openBlock(parent, stack);
  expect((await workerStats(b.frame)).connects).toBeGreaterThan(0);
});
