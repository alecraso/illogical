// This browser's device keys: X25519 (its Noise static key) and Ed25519
// (what it signs approvals with), both non-extractable, kept in IndexedDB.
// The page can use them; nothing can read them out. Also kept there: this
// device's certificate once approved, and the account root it pinned.
//
// #94: a browser that can't read those keys back (WebKit returns null for
// a record holding an X25519 CryptoKey) keeps them wrapped instead: PKCS#8,
// encrypted with a non-extractable AES-GCM key that it can read back. Keys
// are checked after they're stored, read back as a later page load would,
// so a browser never enrolls keys it loses on the next reload.
//
// What wrapping gives up: script running in this origin (an XSS, a
// malicious build) could unwrap the keys as extractable and copy them out,
// then use them anywhere, for as long as the device stays approved. With
// plain non-extractable keys it can only use them while it runs here.
// Both forms are equally readable on disk to someone who has the browser
// profile, and the keys are non-extractable in memory either way.

import { deviceKey, publicRaw } from "./noise.ts";
import { deviceId, hex, unhex, type Cert } from "./cert.ts";

const subtle = globalThis.crypto.subtle;

export interface DeviceKeys {
  noise: CryptoKeyPair;
  sign: CryptoKeyPair;
  id: string;
  noisePub: string;
  signPub: string;
}

/** What this browser remembers about its place in an account. */
export interface Enrollment {
  /** Control's origin, so one browser could hold more than one. */
  control: string;
  account: string;
  root: string;
  cert: Cert;
}

// Frozen (#504): renaming it would lose every browser's device key.
const DB = "illogical-device";

function open(): Promise<IDBDatabase> {
  return new Promise((res, rej) => {
    const r = indexedDB.open(DB, 1);
    r.onupgradeneeded = () => r.result.createObjectStore("kv");
    r.onsuccess = () => res(r.result);
    r.onerror = () => rej(r.error);
  });
}

async function kv<T>(mode: IDBTransactionMode, fn: (s: IDBObjectStore) => IDBRequest | void): Promise<T | undefined> {
  const db = await open();
  return new Promise((res, rej) => {
    const tx = db.transaction("kv", mode);
    const req = fn(tx.objectStore("kv"));
    tx.oncomplete = () => res(req ? (req.result as T) : undefined);
    tx.onerror = () => rej(tx.error);
  });
}

export async function generateKeys(extractable = false): Promise<DeviceKeys> {
  const noise = extractable ? ((await subtle.generateKey({ name: "X25519" }, true, ["deriveBits"])) as CryptoKeyPair) : await deviceKey();
  const sign = (await subtle.generateKey({ name: "Ed25519" }, extractable, ["sign", "verify"])) as CryptoKeyPair;
  const n = await publicRaw(noise.publicKey);
  const s = await publicRaw(sign.publicKey);
  return { noise, sign, id: await deviceId(n, s), noisePub: hex(n), signPub: hex(s) };
}

/** Why `keys` can't be used as this device's, or null if they can: they
 * sign what their public key verifies, and agree on a secret over X25519. */
export async function checkKeys(keys: DeviceKeys): Promise<string | null> {
  try {
    if ((await deviceId(unhex(keys.noisePub), unhex(keys.signPub))) !== keys.id) return "its id doesn't match its keys";
    const msg = new TextEncoder().encode("arugula key check");
    const signPub = await subtle.importKey("raw", unhex(keys.signPub), { name: "Ed25519" }, false, ["verify"]);
    const sig = await subtle.sign("Ed25519", keys.sign.privateKey, msg);
    if (!(await subtle.verify("Ed25519", signPub, sig, msg))) return "its Ed25519 key signs what its public key doesn't verify";
    const other = (await subtle.generateKey({ name: "X25519" }, false, ["deriveBits"])) as CryptoKeyPair;
    const noisePub = await subtle.importKey("raw", unhex(keys.noisePub), { name: "X25519" }, true, []);
    const a = await subtle.deriveBits({ name: "X25519", public: other.publicKey }, keys.noise.privateKey, 256);
    const b = await subtle.deriveBits({ name: "X25519", public: noisePub }, other.privateKey, 256);
    if (hex(new Uint8Array(a)) !== hex(new Uint8Array(b))) return "its X25519 key doesn't agree with its public key";
    return null;
  } catch (e) {
    return `its keys don't work (${(e as Error).message ?? e})`;
  }
}

/** The keys as IndexedDB holds them when CryptoKeys don't survive there. */
interface Wrapped {
  wrap: CryptoKey;
  noise: { iv: Uint8Array<ArrayBuffer>; key: ArrayBuffer };
  sign: { iv: Uint8Array<ArrayBuffer>; key: ArrayBuffer };
  id: string;
  noisePub: string;
  signPub: string;
}

async function wrapKeys(keys: DeviceKeys): Promise<Wrapped> {
  const wrap = await subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["wrapKey", "unwrapKey"]);
  const seal = async (k: CryptoKey) => {
    const iv = crypto.getRandomValues(new Uint8Array(12));
    return { iv, key: await subtle.wrapKey("pkcs8", k, wrap, { name: "AES-GCM", iv }) };
  };
  return { wrap, noise: await seal(keys.noise.privateKey), sign: await seal(keys.sign.privateKey), id: keys.id, noisePub: keys.noisePub, signPub: keys.signPub };
}

/** Unwrapped into non-extractable keys, inside WebCrypto. */
async function unwrapKeys(w: Wrapped): Promise<DeviceKeys> {
  const open = (s: Wrapped["noise"], alg: string, use: KeyUsage[]) =>
    subtle.unwrapKey("pkcs8", s.key, w.wrap, { name: "AES-GCM", iv: s.iv }, { name: alg }, false, use);
  const pub = (hexKey: string, alg: string, use: KeyUsage[]) => subtle.importKey("raw", unhex(hexKey), { name: alg }, true, use);
  return {
    noise: { privateKey: await open(w.noise, "X25519", ["deriveBits"]), publicKey: await pub(w.noisePub, "X25519", []) },
    sign: { privateKey: await open(w.sign, "Ed25519", ["sign"]), publicKey: await pub(w.signPub, "Ed25519", ["verify"]) },
    id: w.id,
    noisePub: w.noisePub,
    signPub: w.signPub,
  };
}

/** The keys IndexedDB holds, in whichever form. Undefined if there are none,
 * or none this browser can read back (WebKit's null, a failed unwrap). */
async function storedKeys(): Promise<DeviceKeys | undefined> {
  const have = await kv<DeviceKeys | Wrapped | null>("readonly", (s) => s.get("keys"));
  if (!have) return undefined;
  if (!("wrap" in have)) return have;
  return unwrapKeys(have).catch(() => undefined);
}

/** Store keys, then read them back from IndexedDB as a later page load
 * would, and check what comes back. */
async function storeChecked(record: DeviceKeys | Wrapped): Promise<DeviceKeys | string> {
  await kv("readwrite", (s) => void s.put(record, "keys"));
  const back = await storedKeys();
  if (!back) return "they don't come back from IndexedDB";
  return (await checkKeys(back)) ?? back;
}

/** This browser's keys, made on first use, or again if the ones it kept
 * no longer work (they were never usable after a reload, so nothing
 * trusted them that this browser can still answer for). */
export async function loadKeys(): Promise<DeviceKeys> {
  const have = await storedKeys();
  if (have && !(await checkKeys(have))) return have;
  let keys = await storeChecked(await generateKeys());
  // #94: non-extractable keys don't survive IndexedDB here: wrap them.
  if (typeof keys === "string") keys = await storeChecked(await wrapKeys(await generateKeys(true)));
  if (typeof keys === "string") {
    await kv("readwrite", (s) => void s.delete("keys"));
    throw new Error(`this browser can't keep a device key between page loads (${keys}); use another browser, or update this one`);
  }
  // Ask the browser not to evict this (it may still, on Safari tabs).
  await navigator.storage?.persist?.().catch(() => false);
  return keys;
}

export async function signText(keys: DeviceKeys, text: string): Promise<string> {
  return hex(new Uint8Array(await subtle.sign("Ed25519", keys.sign.privateKey, new TextEncoder().encode(text))));
}

export async function loadEnrollment(control: string): Promise<Enrollment | undefined> {
  return kv<Enrollment>("readonly", (s) => s.get(`enrollment:${control}`));
}

export async function saveEnrollment(e: Enrollment): Promise<void> {
  await kv("readwrite", (s) => void s.put(e, `enrollment:${e.control}`));
}

export async function forget(control: string): Promise<void> {
  await kv("readwrite", (s) => {
    s.delete(`enrollment:${control}`);
    s.delete("keys");
  });
}

/** A daemon the service worker may answer a notification to (M29): what
 * the page checked (its Noise key from a verified certificate) and how to
 * reach it. The worker can't read `localStorage`, where the page keeps
 * its directory, so the page copies the checked part here. */
export interface WorkerDaemon {
  id: string;
  noise: string;
  direct: string[];
  relay: string;
}

export async function saveWorkerDirectory(daemons: WorkerDaemon[]): Promise<void> {
  await kv("readwrite", (s) => void s.put(daemons, "worker-directory"));
}

export async function loadWorkerDirectory(): Promise<WorkerDaemon[]> {
  return (await kv<WorkerDaemon[]>("readonly", (s) => s.get("worker-directory"))) ?? [];
}

/** The device keys, if this browser has made them (never makes them: for
 * the service worker). */
export async function existingKeys(): Promise<DeviceKeys | undefined> {
  return storedKeys();
}
