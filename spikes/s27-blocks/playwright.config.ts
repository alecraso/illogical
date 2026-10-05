import { defineConfig } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";

// The spike's stack: build the binary and control's static files, and make
// the test certificate, once per run (workers load this file again).
const CERT = ".run/cert";
if (!process.env.S27_BUILT) {
  execFileSync("mise", ["exec", "--", "cargo", "build", "--release"], { stdio: "inherit" });
  execFileSync("node", ["build.mjs"], { stdio: "inherit" });
  if (!existsSync(`${CERT}/spki.txt`)) execFileSync("target/release/s27", ["cert", CERT], { stdio: "inherit" });
  process.env.S27_BUILT = "1";
}
const spki = readFileSync(`${CERT}/spki.txt`, "utf8").trim();
// The harness's CONNECT proxy (global-setup.ts) sends every *.test name to
// 127.0.0.1, so both browsers reach control's block domain by name.
export const PROXY_PORT = Number(process.env.S27_PROXY_PORT ?? 7753);

export default defineConfig({
  testDir: "tests",
  globalSetup: "./tests/global-setup.ts",
  timeout: 120_000,
  workers: 1,
  reporter: [["list"]],
  use: {
    proxy: { server: `http://127.0.0.1:${PROXY_PORT}` },
  },
  projects: [
    {
      name: "chromium",
      use: {
        browserName: "chromium",
        launchOptions: { args: [`--ignore-certificate-errors-spki-list=${spki}`] },
      },
    },
    {
      name: "webkit",
      use: { browserName: "webkit", ignoreHTTPSErrors: true },
    },
  ],
});
