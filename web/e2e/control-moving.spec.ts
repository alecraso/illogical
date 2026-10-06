// Control at two URLs while it moves (#507): the old one's page says where
// control is now; the new one's doesn't.
import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { ANY, controlPort } from "./ports";

let proc: ChildProcess;
let dir: string;
let port: number;

test.beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "control-moving-"));
  const db = join(dir, "control.db");
  proc = spawn(
    "../target/debug/illogical-control",
    [
      "--listen",
      ANY,
      "--public-url",
      "http://127.0.0.1:0",
      "--also-url",
      "http://localhost:0",
      "--db",
      db,
      "--static-dir",
      "dist",
    ],
    { stdio: "ignore" },
  );
  port = await controlPort(db, proc);
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${port}/control.json`)).ok) break;
    } catch {
      // not yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
});

test.afterAll(() => {
  proc?.kill("SIGKILL");
  rmSync(dir, { recursive: true, force: true });
});

test("the old address says where control is now, with the same page there", async ({
  page,
}) => {
  await page.goto(`http://localhost:${port}/#join=ABCDE-FGHIJ`);
  const bar = page.locator("[data-control-moving]");
  await expect(bar).toContainText(`Control is moving to 127.0.0.1:${port}`);
  await expect(bar.getByRole("link")).toHaveAttribute(
    "href",
    `http://127.0.0.1:${port}/#join=ABCDE-FGHIJ`,
  );
});

test("the new address doesn't", async ({ page }) => {
  await page.goto(`http://127.0.0.1:${port}/`);
  await expect(page.locator("body")).not.toContainText("Control is moving");
  await expect(page.locator("[data-control-moving]")).toHaveCount(0);
});
