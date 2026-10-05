import { defineConfig } from "@playwright/test";

// Real Safari through safaridriver (safari/): `testnet/macos/test.sh
// safari` runs it against Safari in a tart VM, or set SAFARIDRIVER_URL to a
// local `safaridriver --port N`. Chrome plays anyone who isn't Safari.
export default defineConfig({
  testDir: "safari",
  timeout: 90_000,
  workers: 1,
  use: { channel: "chrome" },
});
