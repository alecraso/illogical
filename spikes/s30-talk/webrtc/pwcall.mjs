// Drives call.html in headless Chrome for N seconds (the far end of a
// call with the native peer) and prints its log and last stats.
import { createRequire } from "node:module";
const require = createRequire(new URL("../../../web/package.json", import.meta.url));
const { chromium } = require("@playwright/test");
const [url, secs = "15"] = process.argv.slice(2);
const b = await chromium.launch({ channel: "chrome", headless: true, args: ["--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream", "--autoplay-policy=no-user-gesture-required"] });
const p = await (await b.newContext({ permissions: ["microphone"] })).newPage();
await p.goto(url);
await p.waitForTimeout(Number(secs) * 1000);
console.log(await p.textContent("#status"));
console.log(await p.textContent("#stats"));
await b.close();
