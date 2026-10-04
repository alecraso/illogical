import { createRequire } from "node:module";
const require = createRequire(new URL("../../../web/package.json", import.meta.url));
const { chromium } = require("@playwright/test");
const b = await chromium.launch({ channel: "chrome" });
for (const grant of [false, true]) {
  const ctx = await b.newContext();
  if (grant) { try { await ctx.grantPermissions(["local-network-access"], { origin: "https://illogical-s15-relay.fly.dev" }); } catch (e) { console.log("grant failed:", e.message.split("\n")[0]); } }
  const p = await ctx.newPage();
  p.on("console", (m) => m.type() !== "log" && console.log(`  [${m.type()}]`, m.text().slice(0, 300)));
  await p.goto("https://illogical-s15-relay.fly.dev/info");
  const r = await p.evaluate(() => new Promise((res) => {
    const ws = new WebSocket("wss://geek.tail1234.ts.net:10000/");
    ws.onopen = () => res("open"); ws.onerror = () => res("error");
  }));
  console.log(`granted=${grant}: ${r}`);
  await ctx.close();
}
console.log(await (await b.newPage()).evaluate(() => navigator.userAgent));
await b.close();
