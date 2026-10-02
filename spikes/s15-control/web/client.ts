// The WebCrypto Noise client against the Rust (snow) daemon, directly or
// through the relay: interop, then the same measurements as `s15 client`.
//   node --experimental-strip-types web/client.ts URL DAEMON_PUB_HEX

import { deviceKey, Initiator } from "./noise.ts";

const [url, peerHex] = process.argv.slice(2);
const peer = Uint8Array.from(peerHex.match(/../g)!.map((h) => parseInt(h, 16)));

const ws = new WebSocket(url);
ws.binaryType = "arraybuffer";
const inbox: Uint8Array[] = [];
let waiting: ((m: Uint8Array) => void) | undefined;
ws.onmessage = (ev) => {
  const m = new Uint8Array(ev.data as ArrayBuffer);
  if (waiting) {
    const w = waiting;
    waiting = undefined;
    w(m);
  } else inbox.push(m);
};
const next = () => (inbox.length ? Promise.resolve(inbox.shift()!) : new Promise<Uint8Array>((r) => (waiting = r)));
await new Promise((r, j) => {
  ws.onopen = r;
  ws.onerror = j;
});

const key = await deviceKey();
const t0 = performance.now();
const ik = new Initiator(key, peer);
ws.send(await ik.write(new TextEncoder().encode("attach")));
const { payload, channel } = await ik.read(await next());
console.log(`handshake ${(performance.now() - t0).toFixed(2)} ms, daemon said "${new TextDecoder().decode(payload)}"`);

const rtt: number[] = [];
for (let i = 0; i < 200; i++) {
  const t = performance.now();
  ws.send(await channel.send.seal(new TextEncoder().encode("ea")));
  const back = await channel.recv.open(await next());
  if (new TextDecoder().decode(back) !== "ea") throw new Error("bad echo");
  rtt.push(performance.now() - t);
}
rtt.sort((a, b) => a - b);
console.log(`keystroke round trip p50 ${rtt[100].toFixed(3)} ms p99 ${rtt[198].toFixed(3)} ms`);

const req = new Uint8Array(5);
req[0] = 0x62; // b
new DataView(req.buffer).setUint32(1, 1 << 20);
const t = performance.now();
ws.send(await channel.send.seal(req));
let got = 0;
for (;;) {
  const m = await channel.recv.open(await next());
  if (m.length === 1 && m[0] === 0x64) break;
  got += m.length;
}
const ms = performance.now() - t;
console.log(`1 MB burst: ${got} bytes in ${ms.toFixed(1)} ms (${(got / ms / 1000).toFixed(1)} MB/s)`);
ws.close();
