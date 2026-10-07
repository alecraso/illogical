// #505: a browser that used illogical keeps its hosts, settings and
// sign-in state after the rename. The page copies each old storage key to
// its new name when the new one is absent, before anything reads storage,
// and leaves the old key for a daemon still serving the older page.

import { expect, test } from "@playwright/test";

const LOCAL: Record<string, string> = {
  "illogical.hosts": JSON.stringify({ this: "old-home", hosts: [] }),
  "illogical.host": "null",
  "illogical.control.host": "null",
  "illogical.control.pins": "{}",
  "illogical.control.directory": "[]",
  "illogical.fleet": "{}",
  "illogical-hand": "off",
  "illogical.palette.recent": JSON.stringify(["new-tab"]),
  "illogical.update.dismissed": "0.25.0",
  "illogical.install-hint": "dismissed",
  "illogical.agents-nudge": JSON.stringify(["claude"]),
  "illogical.getting-started": "seen",
  "illogical.agent": JSON.stringify({ kind: "claude" }),
  "illogical.swarm.by": "host",
  "illogical.swarm.theme": "city",
  "illogical.control-dropped.opened": "123",
};
const SESSION: Record<string, string> = {
  "illogical:presigned-invite": "#invite=abc",
  "illogical.control.recover": "1",
  "illogical.control-dropped.hidden": "123",
};
const renamed = (k: string) => k.replace(/^illogical/, "arugula");

test("storage under the old names carries over, and stays", async ({ page }) => {
  // Seeded once, before the page's first script; the snapshot is taken
  // as the page's module starts, before it fetches and writes anything.
  await page.addInitScript(
    ([local, session, preset]) => {
      if (!sessionStorage.getItem("rename-spec")) {
        sessionStorage.setItem("rename-spec", "1");
        localStorage.clear();
        for (const [k, v] of Object.entries(local)) localStorage.setItem(k, v);
        for (const [k, v] of Object.entries(session)) sessionStorage.setItem(k, v);
        // A new key already set wins over its old one.
        localStorage.setItem("illogical.swarm.theme", "hive");
        localStorage.setItem(preset, "timeline");
      }
      addEventListener("DOMContentLoaded", () => {
        const snap: Record<string, string | null> = {};
        for (const s of [localStorage, sessionStorage]) {
          for (let i = 0; i < s.length; i++) snap[s.key(i)!] = s.getItem(s.key(i)!);
        }
        (window as unknown as { renameSnap: typeof snap }).renameSnap = snap;
      });
    },
    [LOCAL, SESSION, "arugula.swarm.theme"] as const,
  );
  await page.goto("/");
  await page.waitForFunction(() => "renameSnap" in window);
  const snap = await page.evaluate(() => (window as unknown as { renameSnap: Record<string, string | null> }).renameSnap);

  for (const [k, v] of Object.entries({ ...LOCAL, ...SESSION })) {
    if (k === "illogical.swarm.theme") continue;
    expect(snap[renamed(k)], renamed(k)).toBe(v);
    expect(snap[k], `${k} stays`).toBe(v);
  }
  expect(snap["arugula.swarm.theme"]).toBe("timeline");
  expect(snap["illogical.swarm.theme"]).toBe("hive");
});
