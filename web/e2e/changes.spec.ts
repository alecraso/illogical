// M11's "done when", from a phone: on a tab where an agent (here, a stand-in
// writing files) changed things, Changes opens a diff block listing them
// with +/−; tapping a hunk's line opens a live file block at that line; both
// follow the edits that come after, and stop watching once nothing draws
// them; a shared session's viewer sees both and can't change them; a build
// that fails shows as Failed, and Rerun runs it again. First on this host,
// then the same on a VM tab (with `illogical diff` and `view` there), which
// needs wispd and its token; elsewhere that part skips.

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { appendFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { devices, expect, test, type Page } from "@playwright/test";
import { open, text, closeContexts } from "./helpers";
import type { PaneId } from "../src/proto";
import { ANY, daemonPort } from "./ports";
import { deliver, FakePush, tap } from "./phones";

test.afterAll(closeContexts);

let PORT = 0;
let VM_PORT = 0;
const OWNER = "me@example.com";
const FRIEND = "friend@example.com";
const WISP = process.env.ILLOGICAL_WISP_URL ?? "http://127.0.0.1:7788";
const token = (() => {
  try {
    return readFileSync(process.env.ILLOGICAL_WISP_TOKEN_FILE ?? `${homedir()}/.local/share/wisp/token`, "utf8").trim();
  } catch {
    return "";
  }
})();
const phone = (() => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  return { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch };
})();

// Made in beforeAll: this module is loaded more than once (#62).
let dir = "";
let repo = "";
let vmState = "";
let daemons: ChildProcess[] = [];

/** Start a daemon; its port. */
async function start(state: string, args: string[], env: Record<string, string> = {}) {
  const d = spawn(
    "../target/debug/illogicald",
    ["--listen", ANY, "--state-dir", state, "--shell", "bash --norc --noprofile", "--no-manager-env", ...args],
    { stdio: "ignore", env: { ...process.env, ...env } },
  );
  daemons.push(d);
  const port = await daemonPort(state, d);
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${port}/api/host`)).ok) return port;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("daemon did not start");
}

const git = (...args: string[]) => execFileSync("git", ["-C", repo, "-c", "user.email=t@example.com", "-c", "user.name=t", ...args]);
const APP = ["export function hello() {", '  return "hello";', "}", "", "export const answer = 41;", ""].join("\n");

test.beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "ilg-e2e-changes-"));
  repo = join(dir, "repo");
  vmState = join(dir, "vm-state");
  mkdirSync(join(repo, "src"), { recursive: true });
  git("init", "-q", "-b", "main");
  writeFileSync(join(repo, "src/app.ts"), APP);
  writeFileSync(join(repo, "README.md"), "# demo\n");
  git("add", ".");
  git("commit", "-qm", "init");
  PORT = await start(join(dir, "state"), ["--owner", OWNER, "--tailscale-socket", "/nonexistent/sock", "--wisp-token-file", "/nonexistent"]);
});

test.afterAll(() => {
  for (const d of daemons) d.kill("SIGKILL");
  daemons = [];
  if (dir) rmSync(dir, { recursive: true, force: true });
});

const base = (port: number) => `http://127.0.0.1:${port}`;
const post = (port: number, path: string, body: unknown, headers: Record<string, string> = {}) =>
  fetch(base(port) + path, { method: "POST", headers: { "content-type": "application/json", ...headers }, body: JSON.stringify(body) });
const panesOf = (page: Page) => page.evaluate(() => window.__illogical.client.state!.panes);
const blockState = <T,>(page: Page, id: PaneId) => page.evaluate((b) => window.__illogical.client.blocks.get(b)?.state ?? null, id) as Promise<T | null>;
const activePane = (page: Page) => page.evaluate(() => window.__illogical.client.active() ?? null);
const newPane = async (page: Page, type: string, before: PaneId[]) => {
  await expect.poll(async () => (await panesOf(page)).find((p) => p.type === type && !before.includes(p.id))?.id ?? null, { timeout: 30_000 }).not.toBeNull();
  return (await panesOf(page)).find((p) => p.type === type && !before.includes(p.id))!.id;
};

type Diff = { watching: boolean; add: number; files: { path: string; add: number; del: number; open?: boolean }[] };
type File = { watching: boolean; text: string; line: number | null; rev: number };

/** Changes from the sheet, a file's hunks, a line: the diff block and the
 * file block it opened, both shown on the phone. */
async function changesToFile(page: Page, term: PaneId, path: string, lineText: string) {
  const before = (await panesOf(page)).map((p) => p.id);
  await page.locator(".sheet-button").click();
  await page.locator("[data-changes]").click();
  const diff = await newPane(page, "diff", before);
  await expect.poll(() => activePane(page)).toBe(diff);
  const row = page.locator(`[data-diff="${diff}"] .diff-file[data-file="${path}"]`);
  await expect(row).toBeVisible({ timeout: 30_000 });
  await row.locator(".diff-file-head").tap();
  const line = row.locator(".dl.add", { hasText: lineText });
  await expect(line).toBeVisible();
  const at = Number(await line.getAttribute("data-line"));
  await line.tap();
  const file = await newPane(page, "file", [...before, diff]);
  await expect.poll(() => activePane(page)).toBe(file);
  await expect(page.locator(`[data-file-block="${file}"] .cm-mark-line`)).toContainText(lineText, { timeout: 30_000 });
  expect((await blockState<File>(page, file))!.line).toBe(at);
  expect(term).not.toBe(file);
  return { diff, file };
}

test.describe("on this host", () => {
  test.describe.configure({ mode: "serial" });
  test.use({ baseURL: async ({}, use) => use(base(PORT)), ...phone });

  let term: PaneId = 0;
  let diff: PaneId = 0;
  let file: PaneId = 0;

  test("Changes lists what the agent changed; a hunk's line opens a live file block there", async ({ page }) => {
    await open(page);
    term = (await panesOf(page))[0].id;
    await post(PORT, `/api/panes/${term}/send`, { text: `cd ${repo}`, enter: true });
    await expect.poll(async () => (await panesOf(page)).find((p) => p.id === term)?.cwd).toBe(repo);
    // The agent's edits: one changed file, one new.
    writeFileSync(join(repo, "src/app.ts"), APP.replace("41", "42").replace('"hello"', '"hi"'));
    writeFileSync(join(repo, "NOTES.md"), "# notes\none\ntwo\n");

    ({ diff, file } = await changesToFile(page, term, "src/app.ts", "answer = 42"));
    const d = (await blockState<Diff>(page, diff))!;
    expect(d.files.map((f) => [f.path, f.add, f.del])).toEqual([
      ["src/app.ts", 2, 2],
      ["NOTES.md", 3, 0],
    ]);
    // Drawn on the phone, and live: the agent edits on.
    expect((await blockState<File>(page, file))!.watching).toBe(true);
    const shown = page.locator(`[data-file-block="${file}"]`);
    await expect(shown.locator(".review-live")).toHaveText("live");
    writeFileSync(join(repo, "src/app.ts"), "// added by the agent\n// and another line\n" + APP.replace("41", "42"));
    await expect(shown.locator(".cm-content")).toContainText("added by the agent", { timeout: 10_000 });
    // The mark stays on its text, two lines down.
    await expect(shown.locator(".cm-mark-line")).toContainText("answer = 42");
    expect((await blockState<File>(page, file))!.line).toBe(7);
  });

  test("the diff follows too, and both stop watching once nothing draws them", async ({ page }) => {
    await open(page);
    await page.evaluate((d) => window.__illogical.client.setActive(d), diff);
    await expect(page.locator(`[data-diff="${diff}"] .review-live`)).toHaveText("live");
    await expect.poll(async () => (await blockState<Diff>(page, diff))!.files.find((f) => f.path === "src/app.ts")?.add, { timeout: 10_000 }).toBe(3);
    // The open file's hunks are drawn, highlighted.
    await expect(page.locator(`[data-diff="${diff}"] .diff-file.open .dl.add .hl-c`).first()).toContainText("added by the agent");
    // Back to the terminal: nothing draws them, so they stop.
    await page.evaluate((t) => window.__illogical.client.setActive(t), term);
    await expect.poll(async () => [(await blockState<Diff>(page, diff))!.watching, (await blockState<File>(page, file))!.watching]).toEqual([false, false]);
    const rev = (await blockState<File>(page, file))!.rev;
    appendFileSync(join(repo, "src/app.ts"), "// while nobody looked\n");
    await page.waitForTimeout(2500);
    expect((await blockState<File>(page, file))!.rev).toBe(rev);
    // Looked at again: it catches up.
    await page.evaluate((f) => window.__illogical.client.setActive(f), file);
    await expect(page.locator(`[data-file-block="${file}"] .cm-content`)).toContainText("while nobody looked", { timeout: 10_000 });
  });

  test("a viewer of the shared session sees both and can't change them", async ({ browser, page }) => {
    await open(page);
    const session = await page.evaluate((d) => {
      const c = window.__illogical.client;
      return c.sessionOfTab(c.tabOfPane(d)!.id);
    }, diff);
    expect((await post(PORT, "/api/acl", { session, principal: `tailnet:${FRIEND}`, role: "viewer" })).ok).toBe(true);
    const friend = await (await browser.newContext({ ...phone, baseURL: base(PORT), extraHTTPHeaders: { "tailscale-user-login": FRIEND } })).newPage();
    await friend.goto("/");
    await expect.poll(() => friend.evaluate(() => window.__illogical?.client.role())).toBe("viewer");
    // A viewer sees what the one driving the tab shows (M12): the owner's
    // phone is on the diff.
    const show = async (b: PaneId) => {
      await page.evaluate((x) => window.__illogical.client.setActive(x), b);
      await friend.evaluate((x) => window.__illogical.client.setActive(x), b);
    };
    await show(diff);
    const v = friend.locator(`[data-diff="${diff}"]`);
    await expect(v.locator(".diff-file.open .dl.add").first()).toBeVisible();
    await expect(v.locator(".diff-file-head").first()).toBeDisabled();
    await expect(v.locator(".review-bar button")).toHaveCount(0);
    const count = (await panesOf(page)).length;
    await v.locator(".diff-file.open .dl.add").first().tap();
    await friend.waitForTimeout(500);
    expect((await panesOf(page)).length).toBe(count);
    await show(file);
    await expect(friend.locator(`[data-file-block="${file}"] .cm-mark-line`)).toContainText("answer = 42");
    // Its calls are refused, as are the owner's files.
    const as = { "tailscale-user-login": FRIEND };
    expect((await post(PORT, `/api/blocks/${file}/call/goto`, { line: 1 }, as)).status).toBe(403);
    expect((await post(PORT, `/api/blocks/${diff}/call/file`, { path: "NOTES.md" }, as)).status).toBe(403);
    expect((await fetch(`${base(PORT)}/api/fs/read?path=${encodeURIComponent(join(repo, "README.md"))}`, { headers: as })).status).toBe(403);
    await friend.close();
  });

  test("a build that fails shows as Failed on the phone, and Rerun runs it again", async ({ page }) => {
    await open(page);
    const runs = join(dir, "runs");
    await post(PORT, `/api/panes/${term}/send`, { text: `build() { echo x >> ${runs}; sleep 3.2; echo BUILD-FAILED-$((40+2)); return 2; }`, enter: true });
    await post(PORT, `/api/panes/${term}/send`, { text: "build --release", enter: true });
    // Looking elsewhere (the diff) while it runs.
    await page.evaluate((d) => window.__illogical.client.setActive(d), diff);
    await expect.poll(async () => (await panesOf(page)).find((p) => p.id === term)?.reason?.kind ?? null, { timeout: 15_000 }).toBe("failed");
    const r = (await panesOf(page)).find((p) => p.id === term)!.reason!;
    expect(r.actions).toEqual(["rerun", "dismiss"]);
    await page.locator(".sheet-button").click();
    await page.locator(`[data-wants="${term}"] [data-rerun]`).tap();
    await expect.poll(() => readFileSync(runs, "utf8").split("\n").filter(Boolean).length, { timeout: 10_000 }).toBe(2);
    await expect.poll(async () => (await panesOf(page)).find((p) => p.id === term)?.reason?.kind ?? null, { timeout: 15_000 }).toBe("failed");
    await page.evaluate((t) => window.__illogical.client.setActive(t), term);
    await expect.poll(async () => (await text(page, term)).match(/BUILD-FAILED-42/g)?.length ?? 0).toBe(2);
  });

  // #214: the push itself. The phone turns on *Notify this device*; the
  // daemon's push (encrypted, VAPID-signed) reaches a fake push service,
  // which decrypts it and hands it to the phone's service worker; Rerun on
  // the notification types the build again.
  test("a failure's push reaches the phone, and Rerun on the notification runs it again", async ({ page, context }) => {
    const push = await FakePush.start();
    try {
      await push.stub(page, "pixel");
      await context.grantPermissions(["notifications"]);
      await open(page);
      await page.locator(".sheet-button").click();
      await page.getByRole("button", { name: "Notify this device" }).click();
      await expect(page.getByRole("button", { name: "Notify this device" })).toHaveAttribute("aria-pressed", "true");
      expect((await push.next("pixel", (p) => p.pane === 0)).body).toBe("Notifications work.");
      await page.keyboard.press("Escape");

      const runs = join(dir, "push-runs");
      await post(PORT, `/api/panes/${term}/send`, { text: `build() { echo x >> ${runs}; sleep 3.2; echo PUSHED-BUILD-$((40+2)); return 2; }`, enter: true });
      // The phone shows the diff, so nobody looks at the terminal.
      await page.evaluate((d) => window.__illogical.client.setActive(d), diff);
      await post(PORT, `/api/panes/${term}/send`, { text: "build --push", enter: true });
      const payload = await push.next("pixel", (p) => p.pane === term && p.reason?.kind === "failed", 20_000);
      expect(payload.title).toBe("Failed");
      expect(payload.body).toMatch(/^build --push failed \(exit 2\)/);
      expect(payload.reason!.actions).toEqual(["rerun", "dismiss"]);

      // On the phone: a notification with Rerun and Dismiss; Rerun.
      expect(await deliver(context, page, payload)).toEqual(["rerun", "dismiss"]);
      await tap(context, payload.tag, "rerun");
      await expect.poll(() => readFileSync(runs, "utf8").split("\n").filter(Boolean).length, { timeout: 10_000 }).toBe(2);
      await page.evaluate((t) => window.__illogical.client.setActive(t), term);
      await expect.poll(async () => (await text(page, term)).match(/PUSHED-BUILD-42/g)?.length ?? 0, { timeout: 15_000 }).toBe(2);
      expect(push.refused).toEqual([]);
    } finally {
      push.close();
    }
  });
});

// ---------------------------------------------------------------- a VM tab

const wisp = (method: string, path: string, body?: string) =>
  fetch(`${WISP}/v1/sprites${path}`, { method, headers: { Authorization: `Bearer ${token}` }, body });
/** Write a file in the VM, as an agent there would. */
async function put(sprite: string, path: string, content: string) {
  const r = await wisp("PUT", `/${sprite}/fs/write?path=${encodeURIComponent(path)}&mkdirParents=true`, content);
  if (!r.ok) throw new Error(`write ${path}: ${r.status} ${await r.text()}`);
}
const cli = (...args: string[]) => execFileSync("../target/debug/illogical", ["--socket", join(vmState, "sock"), ...args], { encoding: "utf8" });

test.describe("on a VM tab", () => {
  test.describe.configure({ mode: "serial" });
  test.use({ baseURL: async ({}, use) => use(base(VM_PORT)), ...phone });

  let sprite = "";
  let term: PaneId = 0;
  let diff: PaneId = 0;
  let file: PaneId = 0;

  test.beforeAll(async () => {
    if (!token) return;
    VM_PORT = await start(vmState, ["--tailscale-socket", "/nonexistent/sock"], { ILLOGICAL_WISP_URL: WISP });
  });
  test.afterAll(async () => {
    if (!token) return;
    let id: string | null = null;
    try {
      id = readFileSync(join(vmState, "daemon-id"), "utf8").trim();
    } catch {
      // never started
    }
    // Close what has machines, then anything a failed test left.
    for (const p of [term, diff, file]) if (p) await post(VM_PORT, `/api/panes/${p}/close`, {}).catch(() => null);
    await new Promise((r) => setTimeout(r, 3000));
    if (id) {
      const list = await (await wisp("GET", `?prefix=illogical-eph-${id}-`)).json();
      for (const s of list.sprites ?? []) await wisp("DELETE", `/${s.name}`);
    }
  });

  test("Changes on a VM tab, a hunk's line to a live file block, following the agent", async ({ page }) => {
    test.skip(!token, "no wisp token on this host");
    test.setTimeout(180_000);
    await open(page);
    await page.evaluate(() => window.__illogical.client.newVm({ session: window.__illogical.client.session!, tab: true }));
    await expect.poll(async () => (await page.evaluate(() => window.__illogical.client.state!.machines)).length, { timeout: 60_000 }).toBe(1);
    const [m] = await page.evaluate(() => window.__illogical.client.state!.machines);
    sprite = m.sprite;
    await expect.poll(async () => (await panesOf(page)).find((p) => p.host === m.id)?.cwd ?? null, { timeout: 60_000 }).toBe("/home/sprite");
    term = (await panesOf(page)).find((p) => p.host === m.id)!.id;
    // A repository in the VM, then the agent's edits to it.
    const setup = "mkdir -p ~/proj/src && cd ~/proj && git init -q -b main && printf 'one\\ntwo\\nthree\\n' > src/main.py && git add . && git -c user.email=t@x -c user.name=t commit -qm init && echo SETUP-DONE";
    await post(VM_PORT, `/api/panes/${term}/send`, { text: setup, enter: true });
    await expect.poll(async () => (await panesOf(page)).find((p) => p.id === term)?.cwd, { timeout: 30_000 }).toBe("/home/sprite/proj");
    await put(sprite, "/home/sprite/proj/src/main.py", "one\nTWO = 2\nthree\nfour = 4\n");
    await put(sprite, "/home/sprite/proj/new.txt", "made by the agent\n");

    ({ diff, file } = await changesToFile(page, term, "src/main.py", "four = 4"));
    const d = (await blockState<Diff>(page, diff))!;
    expect(d.files.map((f) => [f.path, f.add, f.del])).toEqual([
      ["src/main.py", 2, 1],
      ["new.txt", 1, 0],
    ]);
    for (const b of [diff, file]) expect((await panesOf(page)).find((p) => p.id === b)!.host).toBe(m.id);
    // The agent keeps editing: the file block follows (every 3s on a VM).
    await put(sprite, "/home/sprite/proj/src/main.py", "zero\none\nTWO = 2\nthree\nfour = 4\n");
    const shown = page.locator(`[data-file-block="${file}"]`);
    await expect(shown.locator(".cm-content")).toContainText("zero", { timeout: 15_000 });
    await expect(shown.locator(".cm-mark-line")).toContainText("four = 4");
    // ...and so does the diff, once it's drawn again.
    await page.evaluate((x) => window.__illogical.client.setActive(x), diff);
    await expect.poll(async () => (await blockState<Diff>(page, diff))!.files.find((f) => f.path === "src/main.py")?.add, { timeout: 15_000 }).toBe(3);
    // Nothing draws them: they stop, and the VM may sleep.
    await page.evaluate((t) => window.__illogical.client.setActive(t), term);
    await expect.poll(async () => [(await blockState<Diff>(page, diff))!.watching, (await blockState<File>(page, file))!.watching]).toEqual([false, false]);
  });

  test("illogical diff, view, capture and describe on the VM", async () => {
    test.skip(!token || !diff, "no VM tab (no wisp token, or the test before failed)");
    test.setTimeout(60_000);
    const out = cli("diff", `%${term}`);
    expect(out).toMatch(/^%\d+\n/);
    expect(out).toContain("src/main.py  +3 -1");
    expect(out).toContain("2 files changed, +4 -1 (the working tree against HEAD)");
    const block = Number(out.split("\n")[0].slice(1));
    expect(cli("capture", `%${block}`)).toContain("+four = 4");
    const view = Number(cli("view", `%${term}:src/main.py:2`).trim().slice(1));
    await expect.poll(() => cli("capture", `%${view}`), { timeout: 15_000 }).toBe("zero\none\nTWO = 2\nthree\nfour = 4\n");
    const desc = JSON.parse(cli("--json", "describe", `%${view}`));
    expect([desc.info.type, desc.state.line, desc.state.real]).toEqual(["file", 2, "/home/sprite/proj/src/main.py"]);
    for (const b of [block, view]) cli("close", `%${b}`);
  });

  test("a build that fails in the VM tab shows as Failed on the phone; Rerun runs it again", async ({ page }) => {
    test.skip(!token || !term, "no VM tab (no wisp token, or the first test failed)");
    test.setTimeout(90_000);
    await open(page);
    await post(VM_PORT, `/api/panes/${term}/send`, { text: "build() { sleep 3.2; echo VM-BUILD-$((40+2)); return 1; }", enter: true });
    await post(VM_PORT, `/api/panes/${term}/send`, { text: "build", enter: true });
    await page.evaluate((d) => window.__illogical.client.setActive(d), diff);
    await expect.poll(async () => (await panesOf(page)).find((p) => p.id === term)?.reason?.kind ?? null, { timeout: 20_000 }).toBe("failed");
    await page.locator(".sheet-button").click();
    await page.locator(`[data-wants="${term}"] [data-rerun]`).tap();
    await page.evaluate((t) => window.__illogical.client.setActive(t), term);
    await expect.poll(async () => (await text(page, term)).match(/VM-BUILD-42/g)?.length ?? 0, { timeout: 20_000 }).toBe(2);
  });
});
