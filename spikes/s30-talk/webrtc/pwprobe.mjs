// Runs probe.html in Playwright's Chromium, Firefox and WebKit, and in
// installed Chrome, with fake media devices, and prints the results.
import { createRequire } from "node:module";
const require = createRequire(new URL("../../../web/package.json", import.meta.url));
const pw = require("@playwright/test");
const base = process.argv[2] || "http://127.0.0.1:8799/probe.html";
// TURN_IP: where coturn listens (Firefox refuses TURN on loopback).
const turn = `&turn=turn:${process.env.TURN_IP || "127.0.0.1"}:3478&user=s30&pass=s30secret`;
const runs = [
  ["chrome", pw.chromium, { channel: "chrome", args: ["--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream"] }],
  ["pw-firefox", pw.firefox, { firefoxUserPrefs: { "media.navigator.streams.fake": true, "media.navigator.permission.disabled": true } }],
  ["pw-webkit", pw.webkit, {}],
];
for (const [name, type, opts] of runs) {
  let b;
  try {
    b = await type.launch({ headless: true, ...opts });
    const ctx = await b.newContext(name === "chrome" ? { permissions: ["microphone"] } : {});
    const p = await ctx.newPage();
    await p.goto(`${base}?client=${name}&report=/report${turn}`);
    await p.waitForFunction(() => document.getElementById("out").textContent.includes('"done": true'), null, { timeout: 90000 });
    console.log("=====", name, "\n" + (await p.textContent("#out")));
  } catch (e) { console.log("=====", name, "FAILED", e.message.split("\n")[0]); }
  await b?.close();
}
