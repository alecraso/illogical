// M28 with the real thing: VS Code (downloaded by @vscode/test-electron,
// driven through Playwright's Electron support) opens a folder on a testnet
// box over Microsoft's Remote-SSH, with illogical's extension installed on
// the box's VS Code server, as a person's laptop would. The box runs its
// own illogicald; a phone (a Pixel-sized page on that daemon) follows the
// editor's cursor, gets the debugger's breakpoint as a card it continues
// from, and accepts the stand-in Claude Code's edit from its rail.
//
// editor-swarm.spec.ts covers the same with code-server standing in for
// VS Code; this one is what it stood in for. Runs only with
// ILLOGICAL_TESTNET_EDITORS=1 (`just testnet-editors`): it needs Docker,
// the static build, and the network to download VS Code, its server for
// the box, and Remote-SSH from the Marketplace.

import { execFile } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { _electron as electron, devices, expect, test, type ElectronApplication, type Page } from "@playwright/test";
import { downloadAndUnzipVSCode, resolveCliArgsFromVSCodeExecutablePath } from "@vscode/test-electron";
import { closeContexts } from "./helpers";

test.afterAll(closeContexts);

const enabled = process.env.ILLOGICAL_TESTNET_EDITORS === "1";
const port = Number(process.env.ILLOGICAL_TESTNET_EDITORS_DAEMON_PORT) || 17752;
const APP = `http://127.0.0.1:${port}`;
const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));
const boxSh = here("../../testnet/editors/box.sh");
const sshConfig = here("../../testnet/editors/.state/ssh_config");
const run = promisify(execFile);
/** A command on the box, as illo. */
const onBox = async (cmd: string) => (await run("ssh", ["-F", sshConfig, "m28-box", cmd], { maxBuffer: 1 << 24 })).stdout;
const PROJ = "/home/illo/shop";

test.skip(!enabled, "ILLOGICAL_TESTNET_EDITORS=1 runs it (just testnet-editors)");
test.use({ baseURL: APP });
test.describe.configure({ mode: "serial" });

let dir = "";
let app: ElectronApplication;
let vs: Page;
let phone: Page;

const editorsNow = async () =>
  JSON.parse(await onBox(`/opt/illogical/illogical --json editors`)) as { pane: number; editor: { app: string; remote?: string; followers: number; debug?: unknown }; file: string | null }[];
const tileKey = (page: Page) => page.evaluate(() => window.__illogical.fleet.panes.find((p) => p.info.type === "editor" && p.session === null)?.key ?? null);

/** A VS Code command, from its palette. */
async function command(name: string) {
  await vs.keyboard.press("F1");
  await vs.locator(".quick-input-widget input").fill(`>${name}`);
  await vs.locator(".quick-input-list .monaco-list-row", { hasText: name }).first().click();
}

async function openFile(rel: string, line: number) {
  await vs.keyboard.press(process.platform === "darwin" ? "Meta+p" : "Control+p");
  await vs.locator(".quick-input-widget input").fill(`${rel}:${line}`);
  await expect(vs.locator(".quick-input-list .monaco-list-row", { hasText: rel.split("/").pop()! }).first()).toBeVisible({ timeout: 15_000 });
  await vs.keyboard.press("Enter");
  await expect(vs.locator(".tab.active", { hasText: rel.split("/").pop()! })).toBeVisible({ timeout: 15_000 });
}

test.beforeAll(async () => {
  test.setTimeout(900_000);
  await run(boxSh, ["up"], { env: process.env });
  // The project, on the box.
  const prices = Array.from({ length: 60 }, (_, i) => `const price${i} = ${i * 3}; // line ${i + 1}`).join("\\n");
  await onBox(
    [
      `rm -rf ${PROJ} && mkdir -p ${PROJ}/src ${PROJ}/.vscode ${PROJ}/.git`,
      `printf '${prices}\\n' > ${PROJ}/src/prices.js`,
      `printf 'function total(items) {\\n  return items.reduce((a, b) => a + b, 0);\\n}\\nmodule.exports = { total };\\n' > ${PROJ}/src/cart.js`,
      `printf "let n = 0;\\nfor (let i = 0; i < 3; i++) n += i;\\ndebugger;\\nconsole.log('done', n);\\n" > ${PROJ}/app.js`,
      `printf '{"version":"0.2.0","configurations":[{"type":"node","request":"launch","name":"app","program":"\${workspaceFolder}/app.js"}]}' > ${PROJ}/.vscode/launch.json`,
      `/opt/illogical/illogical editors vsix -o /tmp/illogical.vsix`,
    ].join(" && "),
  );

  // VS Code and Remote-SSH, in directories of their own: nothing of the
  // person's VS Code is read or written.
  const cache = join(homedir(), ".cache/illogical/vscode-test");
  const exe = await downloadAndUnzipVSCode({ cachePath: cache, version: process.env.ILLOGICAL_VSCODE_VERSION || "stable" });
  dir = mkdtempSync(join(tmpdir(), "ilg-e2e-m28-ssh-"));
  const own = [`--user-data-dir=${join(dir, "user")}`, `--extensions-dir=${join(dir, "ext")}`];
  const [cli, ...cliArgs] = resolveCliArgsFromVSCodeExecutablePath(exe);
  await run(cli, [...cliArgs.filter((a) => !a.startsWith("--extensions-dir") && !a.startsWith("--user-data-dir")), ...own, "--install-extension", "ms-vscode-remote.remote-ssh"], {
    env: { ...process.env, ELECTRON_RUN_AS_NODE: "1" },
  });
  mkdirSync(join(dir, "user/User"), { recursive: true });
  writeFileSync(
    join(dir, "user/User/settings.json"),
    JSON.stringify({
      "remote.SSH.configFile": sshConfig,
      "remote.SSH.remotePlatform": { "m28-box": "linux" },
      // The client fetches the server and copies it over, as for a box
      // with no way out.
      "remote.SSH.localServerDownload": "always",
      "remote.SSH.showLoginTerminal": false,
      "remote.SSH.useExecServer": true,
      "security.workspace.trust.enabled": false,
      "workbench.startupEditor": "none",
      "telemetry.telemetryLevel": "off",
      "update.mode": "none",
      "extensions.autoUpdate": false,
      "extensions.autoCheckUpdates": false,
      "chat.disableAIFeatures": true,
      "workbench.tips.enabled": false,
    }),
  );

  const launch = () =>
    electron.launch({
      executablePath: exe,
      args: [...own, `--folder-uri=vscode-remote://ssh-remote+m28-box${PROJ}`, "--skip-welcome", "--skip-release-notes", "--disable-telemetry", "--disable-workspace-trust", "--new-window"],
      env: { ...process.env, VSCODE_SKIP_PRELAUNCH: "1" },
      timeout: 60_000,
    });
  app = await launch();
  vs = await app.firstWindow();
  await expect(vs.locator(".monaco-workbench")).toBeVisible({ timeout: 60_000 });
  // Remote-SSH's indicator, bottom left: "SSH: m28-box".
  await expect(vs.locator(".statusbar")).toContainText("m28-box", { timeout: 240_000 });

  // The extension, into the box's VS Code server (a workspace extension
  // runs where the files are), then the window again.
  await expect.poll(async () => (await onBox("ls -d ~/.vscode-server/cli/servers/*/server/bin/code-server ~/.vscode-server/bin/*/bin/code-server 2>/dev/null || true")).trim(), { timeout: 240_000 }).not.toBe("");
  const server = (await onBox("ls -d ~/.vscode-server/cli/servers/*/server/bin/code-server ~/.vscode-server/bin/*/bin/code-server 2>/dev/null | head -1")).trim();
  await onBox(`${server} --install-extension /tmp/illogical.vsix`);
  await command("Developer: Reload Window");
  await expect(vs.locator(".monaco-workbench")).toBeVisible({ timeout: 60_000 });
});

test.afterAll(async () => {
  await app?.close().catch(() => {});
  if (dir) rmSync(dir, { recursive: true, force: true });
  if (enabled && !process.env.ILLOGICAL_TESTNET_KEEP) await run(boxSh, ["down"], { env: process.env }).catch(() => {});
});

test("VS Code over Remote-SSH joins the box's swarm when asked", async ({ browser }) => {
  test.setTimeout(120_000);
  const item = vs.locator(".statusbar-item", { hasText: "illogical" }).first();
  await expect(item).toBeVisible({ timeout: 60_000 });
  expect(await editorsNow()).toEqual([]);
  await command("illogical: Show this workspace in the swarm");
  await expect.poll(async () => (await editorsNow()).length, { timeout: 20_000 }).toBe(1);
  await openFile("src/prices.js", 20);
  await expect.poll(async () => (await editorsNow())[0]?.file).toBe("src/prices.js");
  const e = (await editorsNow())[0].editor;
  expect(e.app).toBe("vscode");
  expect(e.remote).toBe("ssh-remote");

  const ctx = await browser.newContext({ ...devices["Pixel 7"], baseURL: APP });
  phone = await ctx.newPage();
  await phone.goto("/#swarm");
  await expect(phone.locator(".swarm")).toBeVisible();
  await expect.poll(() => tileKey(phone), { timeout: 20_000 }).not.toBeNull();
});

test("the phone follows the cursor", async () => {
  test.setTimeout(60_000);
  await phone.waitForTimeout(1500);
  const key = (await tileKey(phone))!;
  const pos = (await phone.evaluate((k) => (window.__illogical.swarm as { screenOf(k: string): { x: number; y: number } }).screenOf(k), key))!;
  await phone.mouse.click(pos.x, pos.y);
  await expect(phone.locator(".follow")).toBeVisible();
  await expect(phone.locator(".follow-file")).toContainText("src/prices.js", { timeout: 15_000 });
  await expect(vs.locator(".statusbar-item", { hasText: "1 following" })).toBeVisible({ timeout: 15_000 });
  const cursorLine = () =>
    phone.evaluate(() => {
      const f = (window as unknown as { __follow?: { view: { state: { doc: { lineAt(n: number): { number: number } }; selection: { main: { head: number } } } } } }).__follow;
      return f ? f.view.state.doc.lineAt(f.view.state.selection.main.head).number : null;
    });
  await expect.poll(cursorLine).toBe(20);
  await vs.keyboard.press("Control+g");
  await vs.keyboard.type("45");
  const t0 = Date.now();
  await vs.keyboard.press("Enter");
  await expect.poll(cursorLine, { intervals: [20] }).toBe(45);
  console.log(`over Remote-SSH, the cursor followed in ${Date.now() - t0} ms`);
  await vs.keyboard.press("End");
  await vs.keyboard.type(" // edited live");
  await expect(phone.locator(".follow .cm-content")).toContainText("// line 45 // edited live", { timeout: 5000 });
  await phone.locator("[data-follow-close]").click();
  await expect(vs.locator(".statusbar-item", { hasText: "following" })).toHaveCount(0, { timeout: 10_000 });
  await command("File: Revert File");
});

test("a breakpoint on the box is a card on the phone's rail, and Continue runs on", async () => {
  test.setTimeout(120_000);
  await openFile("app.js", 1);
  await vs.keyboard.press("F5");
  const card = phone.locator('.swarm-card[data-kind="paused"]');
  await expect(card).toBeVisible({ timeout: 60_000 });
  await expect(card).toContainText("app.js:3");
  await card.locator("[data-continue]").click();
  await expect(card).toHaveCount(0, { timeout: 20_000 });
  await expect(vs.locator(".repl")).toContainText("done 3", { timeout: 20_000 });
});

test("an edit from Claude Code in a pane on the box is accepted from the phone's rail", async () => {
  test.setTimeout(90_000);
  await onBox("mkdir -p ~/fake");
  // The stand-in Claude Code, copied to the box and run in a pane there.
  const fake = here("../../crates/daemon/tests/fake_claude.py");
  await run("scp", ["-F", sshConfig, fake, "m28-box:fake/fake_claude.py"]);
  const pane = Number((await onBox(`cd ${PROJ} && /opt/illogical/illogical run --cwd ${PROJ} -- python3 ~/fake/fake_claude.py`)).trim().replace("%", ""));
  await expect.poll(async () => onBox(`/opt/illogical/illogical tail %${pane} --text`), { timeout: 20_000 }).toContain("connected");
  await fetch(`${APP}/api/panes/${pane}/send`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ text: `edit ${PROJ}/src/cart.js function total(items) {\\n  return items.reduce((a, b) => a + b.price, 0);\\n}\\nmodule.exports = { total };\\n`, enter: true }),
  });
  const card = phone.locator('.swarm-card[data-kind="diff"]');
  await expect(card).toBeVisible({ timeout: 20_000 });
  await card.locator("[data-accept]").click();
  await expect.poll(() => onBox(`cat ${PROJ}/src/cart.js`), { timeout: 15_000 }).toContain("a + b.price, 0");
  await expect(card).toHaveCount(0);
});
