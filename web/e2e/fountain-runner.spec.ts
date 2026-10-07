// M45b: the Fountain runner view, against a fake Fountain served here
// (synthetic runners and sandboxes), the unit file this spec
// writes, and the config's stand-in `systemctl` and `sudo` (which runs bash
// as this user: no test runs sudo). Nothing here reaches a real Fountain.
//
// Opened from a pane's menu ("Fountain runner…"), the block shows this
// host's runner online, and its sandboxes; *Changes* opens a diff of the
// checkout in one (its git run through the sudo form), and *Shell* opens
// a terminal in its directory.

import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type Server, type ServerResponse } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { menu, paneEl, panes, reset } from "./helpers";
import { listen } from "./ports";

const KEY = "e2e-fountain-runner-key";
const RUNNER = "edbc518d-70b9-4f45-8db2-73f57b1de3f0";
const fixtureText = (f: string) => readFileSync(new URL(`../../crates/daemon/tests/fixtures/fountain/${f}`, import.meta.url), "utf8");

let server: Server;
let root = "";

function json(res: ServerResponse, status: number, v: unknown) {
  res.writeHead(status, { "content-type": "application/json" }).end(JSON.stringify(v));
}

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  root = mkdtempSync(join(tmpdir(), "arugula-e2e-sandboxes-"));
  const runners = JSON.parse(fixtureText("runner-view-runners.json"));
  runners.data[0].root = root;
  const sandboxes = JSON.parse(fixtureText("runner-view-sandboxes.json").replaceAll("/srv/fountain/sandboxes", root));
  const agents = JSON.parse(fixtureText("agents.json"));
  for (const s of sandboxes.data) if (s.runner?.path) mkdirSync(s.runner.path, { recursive: true });
  // The agent's checkout in the R1 sandbox: one commit, all of it its edits.
  const sb = sandboxes.data.find((s: { sprite_name: string }) => s.sprite_name.endsWith("-2972e1a2"));
  const repo = join(sb.runner.path, "r1-check");
  mkdirSync(repo);
  writeFileSync(join(repo, "hello.txt"), "hello\n");
  const git = (...a: string[]) => execFileSync("git", ["-c", "user.name=t", "-c", "user.email=t@example.com", ...a], { cwd: repo });
  git("init", "-q");
  git("add", ".");
  git("commit", "-qm", "first");
  server = createServer((req, res) => {
    const u = new URL(req.url!, "http://fountain");
    if (req.headers.authorization !== `Bearer ${KEY}`) return json(res, 401, { error: { message: "invalid api key" } });
    if (u.pathname === "/api/runners") return json(res, 200, runners);
    if (u.pathname === "/api/sandboxes") return json(res, 200, sandboxes);
    const m = u.pathname.match(/^\/api\/agents\/(.+)$/);
    const a = m && agents.data.find((x: { id: string }) => x.id === m[1]);
    if (a) return json(res, 200, { data: a });
    json(res, 404, { error: "not here" });
  });
  const origin = `http://127.0.0.1:${await listen(server)}`;
  writeFileSync(process.env.ARUGULA_FOUNTAIN_CREDENTIALS!, `[default]\napi_key = "${KEY}"\nbase_url = "${origin}"\n`);
  writeFileSync(
    process.env.ARUGULA_FOUNTAIN_UNIT_FILE!,
    `[Service]\nUser=fountain\nExecStart=/usr/local/bin/fountain runner --name runner-1 --root ${root}\n`,
  );
});

test.afterAll(() => {
  server.close();
  rmSync(process.env.ARUGULA_FOUNTAIN_CREDENTIALS!, { force: true });
  rmSync(process.env.ARUGULA_FOUNTAIN_UNIT_FILE!, { force: true });
  rmSync(root, { recursive: true, force: true });
});

test("the runner view shows this host's runner; Changes and Shell open beside it", async ({ page }) => {
  await reset(page);
  const term = (await panes(page))[0];
  await menu(page, paneEl(page, term), "Fountain runner…");
  await expect.poll(async () => (await panes(page)).length).toBe(2);
  const block = await page.evaluate(() => window.__arugula.client.state!.panes.find((p) => p.type === "fountain")!.id);
  const el = page.locator(`[data-fountain-block="${block}"][data-fountain-view="runner"]`);
  // The first read starts the user's shell, systemctl, sudo and fountain
  // --version, and asks the fake Fountain: on a busy machine, seconds.
  await expect(el.locator('[data-runner-this="runner-1"]')).toBeVisible({ timeout: 20_000 });
  await expect(el.locator("[data-runner-online]")).toHaveText("online");
  await expect(el.locator('[data-unit-active="true"]')).toBeVisible();
  const cards = el.locator(".fountain-sandboxes .fountain-card");
  await expect(cards).toHaveCount(2);
  const r1 = el.locator(`.fountain-card[data-sandbox="runner-${RUNNER.replaceAll("-", "")}-2972e1a2"]`);
  await expect(r1.locator(".fountain-name")).toHaveText("hud-playground");
  await expect(el.locator("[data-runner-note]")).toContainText("parking the sandbox doesn't stop it");

  // Changes: a diff of the checkout, beside it.
  await r1.locator("[data-changes]").click();
  await expect.poll(async () => (await panes(page)).length, { timeout: 15_000 }).toBe(3);
  const diff = await page.evaluate(() => window.__arugula.client.state!.panes.find((p) => p.type === "diff")!.id);
  await expect(paneEl(page, diff)).toContainText("hello.txt");
  // Its files are fountain's: no Open file from its lines.
  await expect(paneEl(page, diff).locator('[data-run-as="fountain"]')).toBeVisible();
  await paneEl(page, diff).locator('.diff-file[data-file="hello.txt"] .diff-file-head').click();
  await expect(paneEl(page, diff).locator(".dl.add").first()).toBeVisible();
  await expect(paneEl(page, diff).locator(".dl.go")).toHaveCount(0);

  // Shell: a terminal in the sandbox's directory.
  await r1.locator("[data-shell]").click();
  await expect.poll(async () => (await panes(page)).length).toBe(4);
  const shell = await page.evaluate(() => window.__arugula.client.state!.panes.filter((p) => p.type === "terminal").map((p) => p.id).pop()!);
  const text = () =>
    page.evaluate((p) => window.__arugula.client.request("GET", `/api/panes/${p}/capture?format=text`).then((r) => r.text()), shell);
  await expect.poll(text).not.toBe("");
  await page.evaluate((p) => window.__arugula.client.request("POST", `/api/panes/${p}/send`, { text: "echo at=$(pwd)", enter: true }), shell);
  // pwd's path is the real one (macOS's temp dir is behind a symlink).
  await expect.poll(async () => (await text()).replaceAll("\n", "")).toContain(`at=${realpathSync(root)}/runner-${RUNNER.replaceAll("-", "")}-2972e1a2`);
});
