import { defineConfig } from "@playwright/test";
import { tokenCookies } from "./e2e/local-token.ts";

// `just screenshots`: screenshots/shots.spec.ts starts its own daemon (it
// restarts it for the "restored" shot) and writes to site/img/.
export default defineConfig({
  testDir: "screenshots",
  timeout: 90_000,
  workers: 1,
  use: { channel: "chrome", storageState: { cookies: tokenCookies, origins: [] } },
});
