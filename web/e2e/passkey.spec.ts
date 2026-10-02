// M17: passkeys as a first-class sign-in. With no GitHub at all, a
// stranger makes an account with a passkey, the browser becomes its first
// device, and after signing out the passkey signs them back in (Chrome's
// virtual authenticator stands in for Touch ID).

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { ANY, controlPort } from "./ports";

// WebAuthn needs a domain name; localhost counts as secure.
let base = "";
let control: ChildProcess;
let dir: string;

test.use({ baseURL: async ({}, use) => use(base) });

test.beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "illogical-e2e-passkey-"));
  control = spawn(
    "../target/debug/illogical-control",
    ["--listen", ANY, "--public-url", "http://localhost:0", "--db", join(dir, "control.db"), "--static-dir", "dist"],
    { stdio: "ignore", env: { ...process.env, GITHUB_CLIENT_ID: "", GITHUB_CLIENT_SECRET: "" } },
  );
  base = `http://localhost:${await controlPort(join(dir, "control.db"), control)}`;
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${base}/control.json`)).ok) return;
    } catch {
      // not yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
});

test.afterAll(() => {
  control?.kill("SIGKILL");
  rmSync(dir, { recursive: true, force: true });
});

test("make an account with a passkey, sign out, sign back in", async ({ page }) => {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("WebAuthn.enable");
  await cdp.send("WebAuthn.addVirtualAuthenticator", {
    options: { protocol: "ctap2", transport: "internal", hasResidentKey: true, hasUserVerification: true, isUserVerified: true },
  });
  await page.goto("/");
  await expect(page.locator("[data-signin=github]")).toHaveCount(0);
  await page.locator("[data-signup=passkey]").click();
  await expect(page.getByRole("heading", { name: "Add a machine" })).toBeVisible();
  const first = await page.evaluate(() => ({ account: window.__illogical.control!.account, root: window.__illogical.control!.enrollment!.root }));
  expect(first.root).toBe(await page.evaluate(() => window.__illogical.control!.keys.id));

  // Sign out (keeping this browser's device), then back in with the passkey.
  await page.evaluate(() => fetch("/auth/logout", { method: "POST" }));
  await page.goto("/");
  await expect(page.locator("[data-signin=passkey]")).toBeVisible();
  await page.locator("[data-signin=passkey]").click();
  await expect(page.getByRole("heading", { name: "Add a machine" })).toBeVisible();
  expect(await page.evaluate(() => window.__illogical.control!.account)).toBe(first.account);
});
