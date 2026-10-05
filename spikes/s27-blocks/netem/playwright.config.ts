// The latency spec from the client container of netem.sh: Chromium, with
// the test names resolved to the containers (S27_RESOLVE, host resolver
// rules) instead of the harness's proxy, and the stack already running
// (S27_STACK).
import { defineConfig } from "@playwright/test";
import { readFileSync } from "node:fs";

const spki = readFileSync(".run/cert/spki.txt", "utf8").trim();

export default defineConfig({
  testDir: "../tests",
  testMatch: "latency.spec.ts",
  timeout: 300_000,
  workers: 1,
  reporter: [["list"]],
  projects: [
    {
      name: "chromium",
      use: {
        browserName: "chromium",
        launchOptions: { args: [`--ignore-certificate-errors-spki-list=${spki}`, `--host-resolver-rules=${process.env.S27_RESOLVE}`] },
      },
    },
  ],
});
