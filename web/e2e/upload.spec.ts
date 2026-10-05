// M70: a file pasted, dropped or picked in a pane goes to the pane's host
// with the same bytes, and its path is pasted in, bracketed. The stand-in
// for Claude Code is a script named `claude` (so the daemon takes it for
// an agent) that asks for bracketed paste and shows what it's sent.

import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { active, paneEl, ready, reset, text, type } from "./helpers";

let bin = "";

test.beforeAll(() => {
  bin = mkdtempSync(join(tmpdir(), "ilg-e2e-upload-"));
  writeFileSync(join(bin, "claude"), `#!/bin/sh\nprintf 'stand-in ready\\n\\033[?2004h'\ncat -v\n`);
  chmodSync(join(bin, "claude"), 0o755);
});

test.afterAll(() => rmSync(bin, { recursive: true, force: true }));

/** Some bytes of every value, as a "PNG". */
const png = Buffer.from(Array.from({ length: 70_000 }, (_, i) => (i * 7) % 256));

async function standIn(page: Page) {
  await reset(page);
  const pane = await active(page);
  await type(page, pane, `PATH=${bin}:$PATH claude\n`);
  await expect.poll(() => text(page, pane)).toContain("stand-in ready");
  return pane;
}

/** The path the stand-in was pasted, bracketed: what `cat -v` shows. */
async function pasted(page: Page, pane: number): Promise<string> {
  let path = "";
  await expect
    .poll(async () => {
      const all = (await text(page, pane)).replace(/\n/g, "");
      path = /\^\[\[200~(\S+?)\^\[\[201~/.exec(all)?.[1] ?? "";
      return path;
    })
    .not.toBe("");
  return path;
}

test("a pasted image lands on the host and its path is pasted bracketed", async ({ page }) => {
  const pane = await standIn(page);
  await ready(page, pane);
  await paneEl(page, pane)
    .locator(".term-host")
    .evaluate((host, bytes) => {
      const dt = new DataTransfer();
      dt.items.add(new File([new Uint8Array(bytes)], "shot.png", { type: "image/png" }));
      host.dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }));
    }, [...png]);
  const path = await pasted(page, pane);
  expect(path).toMatch(/illogical-uploads\/\d+\/[0-9a-f]{16}\.png$/);
  // Not an image the browser can decode, so it went as it was.
  expect(readFileSync(path)).toEqual(png);
  await expect(paneEl(page, pane).locator("[data-chip]")).toContainText("Pasted the file");
});

test("Attach file… picks files and pastes their paths together", async ({ page }) => {
  const pane = await standIn(page);
  await paneEl(page, pane).click({ button: "right", position: { x: 60, y: 60 } });
  const chooser = page.waitForEvent("filechooser");
  await page.getByRole("menuitem", { name: "Attach file…" }).click();
  await (await chooser).setFiles([
    { name: "a.txt", mimeType: "text/plain", buffer: Buffer.from("first") },
    { name: "b.txt", mimeType: "text/plain", buffer: Buffer.from("second") },
  ]);
  await expect.poll(async () => (await text(page, pane)).replace(/\n/g, "")).toMatch(/\^\[\[200~\S+\.txt \S+\.txt\^\[\[201~/);
  const [a, b] = /\^\[\[200~(\S+) (\S+)\^\[\[201~/.exec((await text(page, pane)).replace(/\n/g, ""))!.slice(1);
  expect([readFileSync(a, "utf8"), readFileSync(b, "utf8")]).toEqual(["first", "second"]);
});

test("into what isn't a shell or an agent, it asks first", async ({ page }) => {
  await reset(page);
  const pane = await active(page);
  await type(page, pane, `printf 'cat ready\\n\\033[?2004h'; cat -v\n`);
  await expect.poll(() => text(page, pane)).toContain("cat ready");
  await paneEl(page, pane)
    .locator(".term-host")
    .evaluate((host) => {
      const dt = new DataTransfer();
      dt.items.add(new File(["hello"], "note.txt", { type: "text/plain" }));
      host.dispatchEvent(new DragEvent("drop", { dataTransfer: dt, bubbles: true, cancelable: true }));
    });
  const chip = paneEl(page, pane).locator("[data-chip]");
  await expect(chip).toContainText("cat is in front");
  expect(await text(page, pane)).not.toContain("200~");
  await chip.getByRole("button", { name: "Paste anyway" }).click();
  const path = await pasted(page, pane);
  expect(readFileSync(path, "utf8")).toBe("hello");
});
