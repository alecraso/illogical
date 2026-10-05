// A stand-in for control's page: it holds the device key (Ed25519, not
// extractable) and, for each block it shows, makes a one-off X25519 key,
// signs a grant for it, and hands both to that block's frame. It answers
// only the frame it made, at the origin it gave it, and only ever for that
// frame's block: what a frame asks for doesn't change what it gets.

import { type Creds, type Grant, hex } from "./chan.ts";

const subtle = crypto.subtle;

interface Block {
  id: string;
  origin: string;
  frame: HTMLIFrameElement;
  mints: number;
}

interface Open {
  daemon: { id: string; noise: string };
  block: string;
  /** Grant lifetime. */
  ttlMs?: number;
  /** "relay", or a direct `wss://…/e2e` URL. */
  route?: string;
  path?: string;
  /** The block's origin key, if it has one already (a block brought back
   * after a reload keeps its origin, and so its worker and storage). */
  key?: string;
}

const device = (await subtle.generateKey({ name: "Ed25519" }, false, ["sign", "verify"])) as CryptoKeyPair;
const signPub = hex(new Uint8Array(await subtle.exportKey("raw", device.publicKey)));
const blocks: Block[] = [];
const opts = new Map<Block, Open>();

async function mint(b: Block, o: Open): Promise<Creds> {
  const kp = (await subtle.generateKey({ name: "X25519" }, true, ["deriveBits"])) as CryptoKeyPair;
  const pkcs8 = await subtle.exportKey("pkcs8", kp.privateKey);
  const pub = hex(new Uint8Array(await subtle.exportKey("raw", kp.publicKey)));
  const g: Grant = { daemon: o.daemon.id, block: o.block, key: pub, expires: Date.now() + (o.ttlMs ?? 3600_000), by: signPub, sig: "" };
  const body = `illogical block grant 1\ndaemon ${g.daemon}\nblock ${g.block}\nkey ${g.key}\nexpires ${g.expires}\n`;
  g.sig = hex(new Uint8Array(await subtle.sign("Ed25519", device.privateKey, new TextEncoder().encode(body))));
  b.mints++;
  return { daemon: o.daemon, pkcs8, pub, grant: g, route: o.route ?? "relay", control: location.origin };
}

addEventListener("message", async (e) => {
  const b = blocks.find((b) => e.source === b.frame.contentWindow && e.origin === b.origin);
  if (!b || (e.data?.s27 !== "hello" && e.data?.s27 !== "renew")) return;
  const creds = await mint(b, opts.get(b)!);
  b.frame.contentWindow?.postMessage({ s27: "creds", creds }, b.origin);
});

function openBlock(o: Open): Block {
  const key = o.key ?? hex(crypto.getRandomValues(new Uint8Array(10)));
  const origin = `https://b-${key}.blocks.test${location.port ? `:${location.port}` : ""}`;
  const frame = document.createElement("iframe");
  frame.src = origin + (o.path ?? "/");
  frame.style.cssText = "width:900px;height:600px;border:1px solid #888";
  const b: Block = { id: o.block, origin, frame, mints: 0 };
  blocks.push(b);
  opts.set(b, o);
  document.body.append(frame);
  return b;
}

/** Today's path, for comparison: the daemon's own block site, no worker. */
function openDirect(block: string, port: number, path = "/"): HTMLIFrameElement {
  const frame = document.createElement("iframe");
  frame.src = `https://b-${block}.direct.test:${port}${path}`;
  frame.style.cssText = "width:900px;height:600px;border:1px solid #888";
  document.body.append(frame);
  return frame;
}

Object.assign(window, { s27: { signPub, blocks, openBlock, openDirect, ready: true } });
