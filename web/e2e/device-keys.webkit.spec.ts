// #94 in WebKit (Safari's engine): a browser's device keys still work after
// a reload. WebKit reads back a record holding an X25519 CryptoKey from
// IndexedDB as null, so a Safari that enrolled signed approvals with keys
// nobody trusted on every later page load. Here: the key probe page, a
// browser that enrolls and approves a join from a later page load, and one
// that lost its key, says so and enrolls again with a recovery code.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { ANY, controlPort, listen } from "./ports";

let base = "";
const procs: ChildProcess[] = [];
const dirs: string[] = [];
let gh: Server;

test.describe.configure({ mode: "serial" });
// Playwright's WebKit stops loading the page once the service worker is
// registered (real Safari doesn't), so none here.
test.use({ baseURL: async ({}, use) => use(base), serviceWorkers: "block" });

function temp(what: string) {
  const d = mkdtempSync(join(tmpdir(), `illogical-e2e-keys-${what}-`));
  dirs.push(d);
  return d;
}

test.beforeAll(async () => {
  gh = createServer((req, res) => {
    const u = new URL(req.url!, "http://github");
    if (u.pathname === "/login/oauth/authorize") {
      const back = new URL(u.searchParams.get("redirect_uri")!);
      back.searchParams.set("code", "c0de");
      back.searchParams.set("state", u.searchParams.get("state")!);
      res.writeHead(302, { location: back.href }).end();
    } else if (u.pathname === "/login/oauth/access_token") {
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ access_token: "gho_test" }));
    } else if (u.pathname === "/user") {
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ id: 9, login: "safari" }));
    } else res.writeHead(404).end();
  });
  const github = `http://127.0.0.1:${await listen(gh)}`;
  const db = join(temp("db"), "control.db");
  procs.push(
    spawn(
      "../target/debug/illogical-control",
      [
        ...["--listen", ANY, "--public-url", "http://127.0.0.1:0", "--db", db],
        ...["--github-client-id", "id", "--github-client-secret", "s", "--static-dir", "dist"],
        ...["--github-url", github, "--github-api", github],
      ],
      { stdio: "ignore" },
    ),
  );
  base = `http://127.0.0.1:${await controlPort(db, procs.at(-1))}`;
});

test.afterAll(() => {
  for (const p of procs) p.kill("SIGKILL");
  gh?.close();
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

// (Mid-reload, there's no page to ask.)
const phase = (page: Page) => page.evaluate(() => window.__illogical?.control?.phase).catch(() => undefined);

/** How IndexedDB holds this browser's keys: as CryptoKeys, or wrapped. */
const keyForm = (page: Page) =>
  page.evaluate(
    () =>
      new Promise<string>((res) => {
        const r = indexedDB.open("illogical-device");
        r.onsuccess = () => {
          const get = r.result.transaction("kv").objectStore("kv").get("keys");
          get.onsuccess = () => res(get.result == null ? `${get.result}` : "wrap" in get.result ? "wrapped" : "keys");
        };
      }),
  );

/** `illogicald join`, approved from `page` (a fresh page load each time). */
async function approveJoin(page: Page, name: string) {
  const joining = spawn("../target/debug/illogicald", ["join", base, "--name", name, "--state-dir", temp(name)], { stdio: ["pipe", "pipe", "ignore"] });
  procs.push(joining);
  const link = await new Promise<string>((res) => {
    let out = "";
    joining.stdout!.on("data", (d) => {
      out += d;
      const m = out.match(/(http\S+#join=[A-Z0-9-]+)/);
      if (m) res(m[1]);
    });
  });
  const exited = new Promise<number | null>((r) => joining.on("exit", r));
  await page.goto(link);
  await expect(page.locator("[data-join-code]")).toHaveText(link.split("#join=")[1]);
  // The machine asks whether the account is the one this browser shows.
  const account = await page.locator("[data-join-account]").getAttribute("data-join-account");
  await page.locator("[data-approve-join]").click();
  joining.stdin!.end(`${account}\n`);
  expect(await exited).toBe(0);
}

test("the key probe: keys survive a reload here, or the fallback does", async ({ page }) => {
  await page.goto("/key-probe.html");
  const verdict = page.locator("#verdict");
  await expect(verdict).not.toHaveText("Checking…", { timeout: 15_000 });
  const rows = await page.locator("#rows tr").allTextContents();
  test.info().annotations.push({ type: "probe", description: [await verdict.textContent(), ...rows].join("\n") });
  await expect(verdict).not.toContainText("doesn't work either");
  // And for a test driving real Safari (web/safari): the same, as data.
  await expect(verdict).toHaveAttribute("data-verdict", /^(keys|wrapped)$/);
  expect(JSON.parse((await page.locator("#result").textContent())!).verdict).toBe(await verdict.getAttribute("data-verdict"));
  // The fallback, after the reload.
  await expect(page.locator("#rows tr").last().locator("td").last()).toHaveText("works");
  await expect(page.locator("#browser")).toContainText("Safari");
});

let codes: string[] = [];
let page: Page;

test("a browser enrolls, and approves a join after a reload", async ({ browser }) => {
  page = await (await browser.newContext({ baseURL: base, serviceWorkers: "block" })).newPage();
  await page.goto("/");
  await page.locator("[data-signin=github]").click();
  await expect(page.locator("[data-recovery-code]")).toHaveCount(2);
  codes = await page.locator("[data-recovery-code]").allTextContents();
  // Stored in whichever form this WebKit reads back.
  test.info().annotations.push({ type: "stored", description: await keyForm(page) });
  await page.reload();
  await expect.poll(() => phase(page), { timeout: 20_000 }).toBe("ready");
  await approveJoin(page, "box-1");
});

test("a browser that lost its key says so, and enrolls again", async () => {
  await page.goto("/");
  await expect.poll(() => phase(page), { timeout: 20_000 }).toBe("ready");
  // What WebKit did to the old form: the keys come back as nothing.
  await page.evaluate(
    () =>
      new Promise<void>((res) => {
        const r = indexedDB.open("illogical-device");
        r.onsuccess = () => {
          const tx = r.result.transaction("kv", "readwrite");
          tx.objectStore("kv").delete("keys");
          tx.oncomplete = () => res();
        };
      }),
  );
  await page.reload();
  await expect(page.locator("[data-lost-key]")).toContainText("can't read back the key it was approved with");
  await page.locator("[data-enroll-again]").click();
  await expect.poll(() => phase(page), { timeout: 20_000 }).toBe("waiting");
  await page.locator("[data-use-recovery]").click();
  await page.getByLabel("Recovery code").fill(codes[0]);
  await page.getByRole("button", { name: "Use it" }).click();
  await expect.poll(() => phase(page), { timeout: 20_000 }).toBe("ready");
  await approveJoin(page, "box-2");
});
