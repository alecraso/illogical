// M17: passkeys as a first-class sign-in. With no GitHub at all, a
// stranger makes an account with a passkey (and a name, #102), the browser becomes its first
// device, and after signing out the passkey signs them back in (Chrome's
// virtual authenticator stands in for Touch ID).

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { controlPanel, closeContexts } from "./helpers";
import { ANY, controlPort } from "./ports";

test.afterAll(closeContexts);

// WebAuthn needs a domain name; localhost counts as secure.
let base = "";
let control: ChildProcess;
let dir: string;

test.use({ baseURL: async ({}, use) => use(base) });

test.beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "arugula-e2e-passkey-"));
  control = spawn(
    "../target/debug/arugula-control",
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
  // #102: a name first, what teammates see.
  await expect(page.locator("[data-signup-go]")).toBeDisabled();
  await page.locator("[data-signup-name]").fill("  Ada   Lovelace ");
  await page.locator("[data-signup-go]").click();
  await expect(page.getByRole("heading", { name: "Add a machine" })).toBeVisible();
  const first = await page.evaluate(() => ({ account: window.__arugula.control!.account, root: window.__arugula.control!.enrollment!.root }));
  expect(first.root).toBe(await page.evaluate(() => window.__arugula.control!.keys.id));
  expect(await page.evaluate(() => window.__arugula.control!.name)).toBe("Ada Lovelace");
  // Others find them by it, and it changes later.
  const found = await page.evaluate(() => fetch("/api/people?login=ada%20lovelace").then((r) => r.json()));
  expect(found).toMatchObject({ account: first.account, name: "Ada Lovelace", root: first.root });
  await page.locator("[data-stored-codes]").check();
  await page.locator("[data-saved-codes]").click();
  await controlPanel(page, "devices");
  await expect(page.locator("[data-account-name]")).toHaveText("Ada Lovelace");
  await page.locator("[data-edit-name]").click();
  await page.locator("[data-name-input]").fill("Ada");
  await page.locator("[data-save-name]").click();
  await expect(page.locator("[data-account-name]")).toHaveText("Ada");
  expect(await page.evaluate(() => fetch("/api/me").then((r) => r.json()))).toMatchObject({ name: "Ada" });

  // Sign out (keeping this browser's device), then back in with the passkey.
  await page.evaluate(() => fetch("/auth/logout", { method: "POST" }));
  await page.goto("/");
  await expect(page.locator("[data-signin=passkey]")).toBeVisible();
  await page.locator("[data-signin=passkey]").click();
  await expect(page.getByRole("heading", { name: "Add a machine" })).toBeVisible();
  expect(await page.evaluate(() => window.__arugula.control!.account)).toBe(first.account);
  // The account panel names it by the browser that added it, not "Passkey 1" (#208).
  await controlPanel(page, "account");
  await expect(page.locator("[data-passkey-name]")).toHaveText(/^Passkey from Chrome on \w+/);
  await expect(page.locator("[data-passkeys] li .dim")).toContainText("last used");
  // With a second passkey, one can be removed, after a dialog: a
  // double-click on Remove only opens it.
  await page.locator("[data-add-passkey]").click();
  await expect(page.locator("[data-passkeys] li")).toHaveCount(2);
  await page.locator("[data-remove-passkey]").first().dblclick();
  await new Promise((r) => setTimeout(r, 1000));
  expect(((await page.evaluate(() => fetch("/api/me/passkeys").then((r) => r.json()))) as { passkeys: unknown[] }).passkeys).toHaveLength(2);
  const ask = page.locator("[data-confirm-dialog]");
  await expect(ask).toContainText("stops signing you in at once");
  await ask.locator("[data-confirm-remove]").click();
  await expect(page.locator("[data-passkeys] li")).toHaveCount(1);
});
