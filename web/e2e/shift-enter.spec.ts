// Shift+Enter in a pane sends ESC CR (Alt+Enter, a new line in Claude Code),
// where xterm.js alone sends CR. A program in raw mode logs each byte it
// reads, so the test sees exactly what reached the pane: no CR after the
// ESC CR, from the keypress that follows the keydown.

import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { active, paneEl, ready, reset, run } from "./helpers";

test("Shift+Enter sends ESC CR and nothing else; Enter still sends CR", async ({ page }) => {
  await reset(page);
  const pane = await active(page);
  const dir = mkdtempSync(join(tmpdir(), "shift-enter-"));
  const log = join(dir, "bytes");
  const script = join(dir, "raw.py");
  writeFileSync(
    script,
    [
      "import os, sys, tty",
      "tty.setraw(0)",
      "print('raw-ready\\r', flush=True)",
      "while True:",
      "    b = os.read(0, 1)",
      "    with open(sys.argv[1], 'a') as f:",
      "        f.write('%02x\\n' % b[0])",
      "    if b == b'q':",
      "        break",
      "",
    ].join("\n"),
  );
  writeFileSync(log, "");
  await run(page, pane, `python3 ${script} ${log}`, "raw-ready");
  await ready(page, pane);
  const bytes = () => readFileSync(log, "utf8").trim().split("\n").filter(Boolean);

  await paneEl(page, pane).click({ position: { x: 40, y: 40 } });
  await page.keyboard.press("Shift+Enter");
  await page.keyboard.press("x");
  await expect.poll(bytes).toEqual(["1b", "0d", "78"]);
  await page.keyboard.press("Enter");
  await page.keyboard.press("x");
  await expect.poll(bytes).toEqual(["1b", "0d", "78", "0d", "78"]);
  await page.keyboard.press("q");
  await expect.poll(bytes).toEqual(["1b", "0d", "78", "0d", "78", "71"]);
});
