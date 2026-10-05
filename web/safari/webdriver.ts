// A small W3C WebDriver client for real Safari (safaridriver), which
// Playwright can't drive: Playwright's WebKit is Safari's engine, not
// Safari. SAFARIDRIVER_URL is a running `safaridriver --port N`, here or in
// a VM (testnet/macos). Only what web/safari needs.

const ELEMENT = "element-6066-11e4-a52e-4f735466cecf";

export const driverUrl = process.env.SAFARIDRIVER_URL ?? "";

async function call<T>(method: string, url: string, body?: unknown): Promise<T> {
  const r = await fetch(url, {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const j = (await r.json().catch(() => ({}))) as { value?: unknown };
  if (!r.ok) {
    const v = (j.value ?? {}) as { error?: string; message?: string };
    throw new Error(`${method} ${url}: ${r.status} ${v.error ?? ""} ${v.message ?? ""}`.trim());
  }
  return j.value as T;
}

export class Safari {
  private constructor(
    private base: string,
    readonly capabilities: Record<string, unknown>,
  ) {}

  /** A new session. Safari allows one at a time per safaridriver. */
  static async start(caps: Record<string, unknown> = {}): Promise<Safari> {
    let last: unknown;
    // A session that just ended can hold Safari for a moment.
    for (let i = 0; i < 10; i++) {
      try {
        const v = await call<{ sessionId: string; capabilities: Record<string, unknown> }>("POST", `${driverUrl}/session`, {
          capabilities: { alwaysMatch: { browserName: "safari", ...caps } },
        });
        return new Safari(`${driverUrl}/session/${v.sessionId}`, v.capabilities);
      } catch (e) {
        last = e;
        await new Promise((r) => setTimeout(r, 1000));
      }
    }
    throw last;
  }

  version = () => String(this.capabilities.browserVersion ?? "");
  quit = () => call("DELETE", this.base).catch(() => undefined);
  goto = (url: string) => call("POST", `${this.base}/url`, { url });
  url = () => call<string>("GET", `${this.base}/url`);
  /** Runs `script` (a function body: `return ...`) in the page. */
  run = <T>(script: string, ...args: unknown[]) => call<T>("POST", `${this.base}/execute/sync`, { script, args });
  /** A cookie for the page's current host (cookies don't care about ports). */
  cookie = (name: string, value: string) => call("POST", `${this.base}/cookie`, { cookie: { name, value, path: "/" } });

  async click(css: string) {
    const el = await call<Record<string, string>>("POST", `${this.base}/element`, { using: "css selector", value: css });
    const id = el?.[ELEMENT];
    if (!id) throw new Error(`Safari: no element ${css} (${JSON.stringify(el)})`);
    await call("POST", `${this.base}/element/${id}/click`, {});
  }

  /** Polls `script` until it returns something truthy; returns that. */
  async until<T>(what: string, script: string, timeout = 15_000): Promise<T> {
    const deadline = Date.now() + timeout;
    let last: unknown;
    for (;;) {
      try {
        const v = await this.run<T>(script);
        if (v) return v;
      } catch (e) {
        // Mid-navigation there's no page to ask.
        last = e;
      }
      if (Date.now() > deadline) throw new Error(`Safari: timed out waiting for ${what}${last ? ` (${last})` : ""}`);
      await new Promise((r) => setTimeout(r, 200));
    }
  }

  /** Waits for `css` to exist (and be shown); returns its text. */
  shown = (css: string, timeout?: number) =>
    this.until<string>(
      css,
      `const e = document.querySelector(${JSON.stringify(css)}); return e && e.getClientRects().length ? e.textContent || " " : null;`,
      timeout,
    );
}
