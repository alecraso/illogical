// Loopback callers show the daemon's local token (crates/daemon/src/
// localauth.rs). Test daemons all read one file, $ILLOGICAL_LOCAL_TOKEN_FILE
// (made here, in a directory of its own removed at exit, and inherited by
// every daemon and worker); browsers carry it as a cookie (any
// name: the value is what counts); and importing this makes Node's fetch
// send that cookie to loopback, where only daemons read it.

import { randomBytes } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

if (!process.env.ILLOGICAL_LOCAL_TOKEN_FILE) {
  const dir = mkdtempSync(join(tmpdir(), "illogical-e2e-token-"));
  process.env.ILLOGICAL_LOCAL_TOKEN_FILE = join(dir, "local-token");
  process.on("exit", () => rmSync(dir, { recursive: true, force: true }));
}
const file = process.env.ILLOGICAL_LOCAL_TOKEN_FILE!;
if (!existsSync(file)) writeFileSync(file, `ilt_${randomBytes(32).toString("hex")}`, { mode: 0o600 });

export const localToken = readFileSync(file, "utf8").trim();

/** The cookie, for a browser context's `storageState`. */
export const tokenCookies = ["127.0.0.1", "localhost"].map((domain) => ({
  name: "illogical_e2e",
  value: localToken,
  domain,
  path: "/",
  expires: -1,
  httpOnly: true,
  secure: false,
  sameSite: "Lax" as const,
}));

/** A daemon's sign-in link (for a browser without the cookie). */
export const signIn = (origin: string, next = "/") => `${origin}/auth?token=${localToken}&next=${encodeURIComponent(next)}`;

const loopback = ["127.0.0.1", "localhost", "[::1]"];
const plain = globalThis.fetch as typeof fetch & { illogical?: true };
if (!plain.illogical) {
  const withToken = (input: Parameters<typeof fetch>[0], init?: RequestInit) => {
    const url = new URL(input instanceof Request ? input.url : String(input));
    if (!loopback.includes(url.hostname)) return plain(input, init);
    const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
    // A request with cookies of its own is for something else (wisp's UI).
    if (headers.has("cookie")) return plain(input, init);
    headers.set("cookie", `illogical_e2e=${localToken}`);
    return plain(input, { ...init, headers });
  };
  globalThis.fetch = Object.assign(withToken, { illogical: true as const });
}
