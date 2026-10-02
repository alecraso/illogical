// Run the deployed rtt.html in Chrome from here, as the phone would.
import { createRequire } from "node:module";
const require = createRequire(new URL("../../../web/package.json", import.meta.url));
const { chromium } = require("@playwright/test");
const b = await chromium.launch({ channel: "chrome" });
const p = await b.newPage();
await p.goto(process.argv[2]);
await p.click("button[data-net=wifi]");
await p.waitForFunction(() => /done|error/.test(document.getElementById("out").textContent), null, { timeout: 60000 });
console.log(await p.textContent("#out"));
await b.close();
