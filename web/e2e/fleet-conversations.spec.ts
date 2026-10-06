// #78: every host's Claude Code conversations in one picker (M33 over
// M25's fleet). Three daemons stand in for geek (the home daemon the page
// comes from), jake-mini and a laptop, each with a Claude directory of its
// own. The picker lists them grouped by host, opens one of jake-mini's on
// jake-mini and continues it there, and a host that stops answering
// doesn't hold up the others.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { devices, expect, test, type Page } from "@playwright/test";
import { menu, paneEl, panes, reset, seedConversation } from "./helpers";
import { ANY, daemonPort } from "./ports";
import { labs } from "./labs";

let homeUrl = "";
const dirs: string[] = [];
const daemons = new Map<string, ChildProcess>();
const portOf = new Map<string, number>();
/** Each host's conversation. */
const conv = new Map<string, { id: string; cwd: string }>();

test.use({ baseURL: async ({}, use) => use(homeUrl) });
test.describe.configure({ mode: "serial" });

const tmp = (prefix: string) => {
  const d = mkdtempSync(join(tmpdir(), prefix));
  dirs.push(d);
  return d;
};

/** A daemon with a Claude directory of its own, holding one conversation
 * ("remember WORD"). */
async function startDaemon(name: string, word: string, extra: string[] = []) {
  const state = tmp(`ilg-e2e-fconv-${name}-`);
  const claude = tmp(`ilg-e2e-fconv-claude-${name}-`);
  mkdirSync(join(claude, "sessions"));
  const c = seedConversation(claude, word, `About ${word}`);
  dirs.push(c.cwd);
  conv.set(name, c);
  const d = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", ANY, "--name", name, "--state-dir", labs(state)],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/tailscaled.sock"],
      ...extra,
    ],
    { stdio: "ignore", env: { ...process.env, CLAUDE_CONFIG_DIR: claude } },
  );
  daemons.set(name, d);
  const port = await daemonPort(state, d);
  portOf.set(name, port);
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${port}/api/host`)).ok) return port;
    } catch {
      // not yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`daemon ${name} did not start`);
}

test.beforeAll(async () => {
  homeUrl = `http://127.0.0.1:${await startDaemon("geek", "geese")}`;
  for (const [name, word] of [
    ["jake-mini", "minnow"],
    ["laptop", "lapwing"],
  ]) {
    await startDaemon(name, word, ["--allow-origin", homeUrl]);
    const res = await fetch(`${homeUrl}/api/hosts`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name, urls: [`http://127.0.0.1:${portOf.get(name)}`] }),
    });
    expect(res.ok).toBe(true);
  }
});

test.afterAll(() => {
  for (const d of daemons.values()) d.kill("SIGCONT"), d.kill("SIGKILL");
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

const hostStates = (page: Page) =>
  page.evaluate(() => Object.fromEntries((window.__illogical?.fleet?.list ?? []).map((h) => [h.name, h.state])));

const picker = (page: Page) => page.getByRole("dialog", { name: "Claude Code conversations" });

/** On geek, with every host connected, the picker from a pane's menu. */
async function openPicker(page: Page) {
  await reset(page);
  await expect
    .poll(() => hostStates(page), { timeout: 15_000 })
    .toEqual({ geek: "connected", "jake-mini": "connected", laptop: "connected" });
  const [term] = await panes(page);
  await menu(page, paneEl(page, term), "Claude Code conversations…");
  await expect(picker(page)).toBeVisible();
  return picker(page);
}

/** The agent block on a daemon, if there's one. */
const agentOn = async (url: string) =>
  ((await (await fetch(`${url}/api/panes`)).json()) as { id: number; type?: string }[]).find((p) => p.type === "agent")?.id ?? null;

const row = (page: Page, host: string) => picker(page).locator(`.conv-row[data-host="${host}"][data-conversation="${conv.get(host)!.id}"]`);

test("every host's conversations in one picker, each opened and continued on its own host", async ({ page }) => {
  const dialog = await openPicker(page);
  // Grouped by host, this one first; each host's under its heading.
  await expect(dialog.locator(".conv-host .conv-host-name")).toHaveText(["geek", "jake-mini", "laptop"]);
  for (const host of ["geek", "jake-mini", "laptop"]) {
    await expect(dialog.locator(`.conv-host[data-host="${host}"] .conv-host-state`)).toHaveText("1 conversation");
    await expect(row(page, host)).toBeVisible();
  }
  const order = await dialog.locator(".conv-host, .conv-row").evaluateAll((els) =>
    els.map((e) => (e.classList.contains("conv-host") ? `#${e.getAttribute("data-host")}` : e.getAttribute("data-host"))),
  );
  expect(order).toEqual(["#geek", "geek", "#jake-mini", "jake-mini", "#laptop", "laptop"]);
  await expect(row(page, "jake-mini")).toContainText("About minnow");
  // Search runs across hosts.
  await dialog.locator(".picker-filter").fill("minnow");
  await expect(row(page, "geek")).toBeHidden();
  await expect(row(page, "jake-mini")).toBeVisible();

  // jake-mini's opens on jake-mini, and the page shows it there.
  await row(page, "jake-mini").click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => page.evaluate(() => window.__illogical.hosts.current)).toBe("jake-mini");
  const mini = `http://127.0.0.1:${portOf.get("jake-mini")}`;
  await expect.poll(() => agentOn(mini)).not.toBeNull();
  const block = (await agentOn(mini))!;
  expect(await agentOn(homeUrl)).toBeNull();
  await expect.poll(() => page.evaluate(() => window.__illogical.client.active())).toBe(block);
  const el = paneEl(page, block);
  await expect(el.locator(".agent-user").first()).toHaveText("remember minnow");

  // Continued there: jake-mini's adapter reads jake-mini's transcript.
  await el.locator(".agent-composer textarea").fill("recall");
  await el.locator(".agent-composer textarea").press("Enter");
  await expect(el.locator(".agent-msg").last()).toHaveText("You said minnow.");

  // Back on geek, picking it again goes to that block on jake-mini.
  await page.evaluate(() => window.__illogical.hosts.select("geek"));
  await expect.poll(() => page.evaluate(() => window.__illogical.client.connected)).toBe(true);
  const [term] = await panes(page);
  await menu(page, paneEl(page, term), "Claude Code conversations…");
  await expect(row(page, "jake-mini").locator(".host-tag")).toHaveText(`%${block}`);
  await row(page, "jake-mini").click();
  await expect.poll(() => page.evaluate(() => window.__illogical.hosts.current)).toBe("jake-mini");
  await expect.poll(() => page.evaluate(() => window.__illogical.client.active())).toBe(block);
  expect(await agentOn(homeUrl)).toBeNull();
  await page.evaluate(() => window.__illogical.hosts.select("geek"));
});

test("a host that stops answering is shown as such and holds nothing up", async ({ page }) => {
  await reset(page);
  await expect.poll(() => hostStates(page), { timeout: 15_000 }).toEqual({ geek: "connected", "jake-mini": "connected", laptop: "connected" });
  // Unplugged: its socket stays open but nothing answers.
  const laptop = daemons.get("laptop")!;
  laptop.kill("SIGSTOP");
  try {
    const t0 = Date.now();
    const [term] = await panes(page);
    await menu(page, paneEl(page, term), "Claude Code conversations…");
    // The others are there at once, the laptop still being asked.
    await expect(row(page, "jake-mini")).toBeVisible();
    await expect(row(page, "geek")).toBeVisible();
    const others = Date.now() - t0;
    expect(others).toBeLessThan(2000);
    await expect(picker(page).locator('.conv-host[data-host="laptop"]')).toHaveClass(/reading/);
    // Then it says it isn't answering, within the per-host timeout.
    await expect(picker(page).locator('.conv-host[data-host="laptop"]')).toHaveClass(/away/, { timeout: 7000 });
    await expect(picker(page).locator('.conv-host[data-host="laptop"] .conv-host-state')).toContainText("not answering");
    const away = Date.now() - t0;
    console.log(`fleet conversations: others in ${others} ms, the stopped host given up on at ${away} ms`);
    expect(away).toBeLessThan(7000);
    // What's there still opens.
    await expect(row(page, "jake-mini")).toBeVisible();
  } finally {
    laptop.kill("SIGCONT");
  }
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  test("the sheet lists every host's and opens one on its host", async ({ page }) => {
    await reset(page);
    await expect.poll(() => hostStates(page), { timeout: 15_000 }).toEqual({ geek: "connected", "jake-mini": "connected", laptop: "connected" });
    await page.locator(".sheet-button").click();
    await page.locator("[data-conversations]").click();
    await expect(picker(page).locator(".picker.phone")).toBeVisible();
    await expect(picker(page).locator(".conv-host .conv-host-name")).toHaveText(["geek", "jake-mini", "laptop"]);
    await row(page, "laptop").click();
    await expect(picker(page)).toBeHidden();
    await expect.poll(() => page.evaluate(() => window.__illogical.hosts.current)).toBe("laptop");
    const there = `http://127.0.0.1:${portOf.get("laptop")}`;
    await expect.poll(() => agentOn(there)).not.toBeNull();
    const block = (await agentOn(there))!;
    await expect.poll(() => page.evaluate(() => window.__illogical.client.active())).toBe(block);
    await expect(paneEl(page, block).locator(".agent-user").first()).toHaveText("remember lapwing");
    await page.evaluate(() => window.__illogical.hosts.select("geek"));
  });
});
