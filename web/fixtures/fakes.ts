// The outside services arugula-control talks to, faked: GitHub sign-in,
// Stripe and a Web Push endpoint. `web/control-smoke.ts` starts them in its
// own process; the test stack's `control` profile (testnet/) runs them in a
// container with `fakes-serve.ts`. Each records what it was sent, for the
// test to check.

import { createServer, type IncomingMessage, type Server } from "node:http";

/** A request a fake got: its path, headers, and body. */
export interface Call {
  path: string;
  headers: IncomingMessage["headers"];
  body: Buffer;
}

function body(req: IncomingMessage): Promise<Buffer> {
  return new Promise((ok) => {
    const chunks: Buffer[] = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => ok(Buffer.concat(chunks)));
  });
}

/** GitHub's numeric user id for a login: stable, so the same login is the
 * same account on every sign-in, and below 2^31 however long the login. */
export const githubId = (login: string) => [...login].reduce((h, c) => (h * 31 + c.charCodeAt(0)) % 2147483647, 7);

/** A fake GitHub for OAuth sign-in. Authorize redirects straight back with
 * a code naming who signed in: the `login` query parameter the client adds
 * to the authorize URL, else `user()` (control-smoke switches it). No state
 * on the server, so any number of people can sign in at once. */
export function fakeGithub(port: number, host = "127.0.0.1", user: () => string = () => "stranger"): Server {
  return createServer(async (req, res) => {
    const u = new URL(req.url!, `http://${host}:${port}`);
    const json = (v: unknown) => res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(v));
    if (u.pathname === "/login/oauth/authorize") {
      const back = new URL(u.searchParams.get("redirect_uri")!);
      back.searchParams.set("code", `c0de.${u.searchParams.get("login") ?? user()}`);
      back.searchParams.set("state", u.searchParams.get("state")!);
      res.writeHead(302, { location: back.href }).end();
    } else if (u.pathname === "/login/oauth/access_token") {
      const code = new URLSearchParams((await body(req)).toString()).get("code") ?? "";
      json({ access_token: `gho_${code.replace(/^c0de\./, "")}` });
    } else if (u.pathname === "/user") {
      const login = String(req.headers.authorization ?? "").replace(/^(Bearer|token) gho_/, "");
      json({ id: githubId(login), login });
    } else res.writeHead(404).end();
  }).listen(port, host);
}

/** A fake Stripe: customers, checkout, a subscription with a seat item
 * and a metered item. `calls` holds what control asked for, bodies as
 * forms. */
export function fakeStripe(port: number, host = "127.0.0.1") {
  const calls: { path: string; form: URLSearchParams }[] = [];
  const server = createServer(async (req, res) => {
    const path = req.url!.split("?")[0];
    calls.push({ path, form: new URLSearchParams((await body(req)).toString()) });
    const reply = (v: unknown) => res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(v));
    if (path === "/v1/customers") reply({ id: "cus_1" });
    else if (path === "/v1/checkout/sessions") reply({ id: "cs_1", url: "https://checkout.stripe.test/cs_1" });
    else if (path === "/v1/subscriptions/sub_1")
      reply({ id: "sub_1", items: { data: [{ id: "si_seat", price: { id: "price_seat" } }, { id: "si_min", price: { id: "price_min" } }] } });
    else reply({});
  }).listen(port, host);
  return { server, calls };
}

/** A Web Push endpoint (any path) that takes every notification. */
export function fakePush(port: number, host = "127.0.0.1") {
  const pushed: Call[] = [];
  const server = createServer(async (req, res) => {
    pushed.push({ path: req.url!, headers: req.headers, body: await body(req) });
    res.writeHead(201).end();
  }).listen(port, host);
  return { server, pushed };
}
