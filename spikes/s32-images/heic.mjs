import { createRequire } from "node:module"; import { readFileSync } from "node:fs";
const require = createRequire(new URL("../../web/package.json", import.meta.url));
const pw = require("@playwright/test");
const heic = readFileSync("/tmp/illogical-s32/img.heic").toString("base64");
for (const [name, type, opts] of [["chrome", pw.chromium, { channel: "chrome" }], ["webkit", pw.webkit, {}], ["firefox", pw.firefox, {}]]) {
  const b = await type.launch(opts); const p = await b.newPage();
  const r = await p.evaluate(async (b64) => {
    const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
    try {
      const bmp = await createImageBitmap(new Blob([bytes], { type: "image/heic" }));
      const c = new OffscreenCanvas(bmp.width, bmp.height); c.getContext("2d").drawImage(bmp, 0, 0);
      const jpeg = await c.convertToBlob({ type: "image/jpeg", quality: 0.9 });
      return `decodes ${bmp.width}x${bmp.height}, re-encoded as JPEG (${jpeg.size} bytes)`;
    } catch (e) { return `can't decode: ${e.name}`; }
  }, heic);
  console.log(name.padEnd(8), r); await b.close();
}
