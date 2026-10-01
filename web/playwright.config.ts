import { defineConfig } from "@playwright/test";

// By default runs against a throwaway debug daemon on 7683 (which serves
// web/dist from disk), driving the system Chrome. Set E2E_BASE_URL to test a
// daemon that is already running, e.g. through `tailscale serve`.
const port = 7683;
const external = process.env.E2E_BASE_URL || undefined;

export default defineConfig({
  testDir: "e2e",
  timeout: 30_000,
  fullyParallel: false,
  workers: 1,
  use: {
    baseURL: external ?? `http://127.0.0.1:${port}`,
    channel: "chrome",
    viewport: { width: 1000, height: 640 },
  },
  webServer: external
    ? undefined
    : {
        command: `RUST_LOG=illogicald=debug ../target/debug/illogicald --listen 127.0.0.1:${port} --shell "bash --norc --noprofile"`,
        url: `http://127.0.0.1:${port}/`,
        reuseExistingServer: false,
        stdout: "ignore",
        stderr: process.env.E2E_DAEMON_LOG ? "pipe" : "ignore",
      },
});
