// This browser's device keys: X25519 (its Noise static key) and Ed25519
// (what it signs approvals with), both non-extractable, kept in IndexedDB.
// The page can use them; nothing can read them out. Also kept there: this
// device's certificate once approved, and the account root it pinned.

import { deviceKey, publicRaw } from "./noise.ts";
import { deviceId, hex, type Cert } from "./cert.ts";

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

export async function generateKeys(): Promise<DeviceKeys> {
  const noise = await deviceKey();
  const sign = (await subtle.generateKey({ name: "Ed25519" }, false, ["sign", "verify"])) as CryptoKeyPair;
  const n = await publicRaw(noise.publicKey);
  const s = await publicRaw(sign.publicKey);
  return { noise, sign, id: await deviceId(n, s), noisePub: hex(n), signPub: hex(s) };
}

/** This browser's keys, made on first use. */
export async function loadKeys(): Promise<DeviceKeys> {
  const have = await kv<DeviceKeys>("readonly", (s) => s.get("keys"));
  if (have) return have;
  const keys = await generateKeys();
  await kv("readwrite", (s) => void s.put(keys, "keys"));
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
