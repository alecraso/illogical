// Device certificates and deciding which devices an account trusts: the
// browser's copy of `illogical_e2e::cert`, kept byte-for-byte compatible
// (the signed body is a fixed line format, not JSON).
//
// The browser checks daemons the way daemons check browsers: a daemon's
// key from the directory counts only if its certificate chains back to the
// account root this browser pinned when it was enrolled. Otherwise control
// could hand out a key of its own and sit in the middle.

import { cat, sha256 } from "./noise.ts";

const subtle = globalThis.crypto.subtle;

export type Kind = "browser" | "cli" | "daemon" | "recovery";

export interface Cert {
  v: number;
  account: string;
  device: string;
  kind: Kind;
  name: string;
  noise: string;
  sign: string;
  created: number;
  approver: string;
  sig: string;
}

export interface Revocation {
  v: number;
  account: string;
  device: string;
  at: number;
  by: string;
  sig: string;
}

export const approves = (k: Kind) => k === "browser" || k === "cli" || k === "recovery";

export function hex(b: Uint8Array): string {
  return Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
}

export function unhex(s: string): Uint8Array<ArrayBuffer> {
  if (!/^([0-9a-f]{2})*$/.test(s)) throw new Error("bad hex");
  return Uint8Array.from(s.match(/../g) ?? [], (h) => parseInt(h, 16));
}

const enc = new TextEncoder();

export function certBody(c: Cert): string {
  return (
    `illogical device v1\naccount ${c.account}\ndevice ${c.device}\nkind ${c.kind}\nname ${c.name}\n` +
    `noise ${c.noise}\nsign ${c.sign}\ncreated ${c.created}\napprover ${c.approver}\n`
  );
}

export function revocationBody(r: Revocation): string {
  return `illogical revoke v1\naccount ${r.account}\ndevice ${r.device}\nat ${r.at}\nby ${r.by}\n`;
}

export async function deviceId(noise: Uint8Array, sign: Uint8Array): Promise<string> {
  return hex((await sha256(cat(enc.encode("illogical device id"), noise, sign))).subarray(0, 8));
}

export const fingerprint = (id: string) => id.match(/.{1,4}/g)?.join("-") ?? id;

export async function verify(signHex: string, msg: string, sigHex: string): Promise<boolean> {
  try {
    const key = await subtle.importKey("raw", unhex(signHex), { name: "Ed25519" }, false, ["verify"]);
    return await subtle.verify("Ed25519", key, unhex(sigHex), enc.encode(msg));
  } catch {
    return false;
  }
}

const token = (s: string) => /^[A-Za-z0-9_-]{1,64}$/.test(s);
// eslint-disable-next-line no-control-regex
const plain = (s: string) => s.length > 0 && [...s].length <= 64 && !/[\u0000-\u001f\u007f-\u009f]/.test(s);

export async function checkForm(c: Cert): Promise<boolean> {
  if (c.v !== 1 || !token(c.account) || !token(c.approver) || !plain(c.name)) return false;
  if (!/^[0-9a-f]{64}$/.test(c.noise) || !/^[0-9a-f]{64}$/.test(c.sign)) return false;
  return (await deviceId(unhex(c.noise), unhex(c.sign))) === c.device;
}

export async function signedBy(c: Cert, approver: Cert): Promise<boolean> {
  return approver.device === c.approver && (await verify(approver.sign, certBody(c), c.sig));
}

/** Which of `certs` chain back to `root`, net of revocations: the same
 * rules as `Trust::evaluate`. */
export async function evaluate(
  trust: { account: string; root: string },
  certs: Cert[],
  revocations: Revocation[] = [],
): Promise<Map<string, Cert>> {
  const byId = new Map<string, Cert[]>();
  for (const c of certs) {
    if (c.account === trust.account && (await checkForm(c))) byId.set(c.device, [...(byId.get(c.device) ?? []), c]);
  }
  const valid = async (revoked: Map<string, number>, id: string, at: number, depth: number): Promise<Cert | undefined> => {
    const r = revoked.get(id);
    if (depth > 64 || (r !== undefined && r <= at)) return undefined;
    for (const c of byId.get(id) ?? []) {
      if (id === trust.root) {
        if (c.approver === c.device && approves(c.kind) && c.kind !== "recovery" && (await signedBy(c, c))) return c;
        continue;
      }
      if (c.approver === c.device) continue;
      const a = await valid(revoked, c.approver, c.created, depth + 1);
      if (!a || !approves(a.kind) || (a.kind === "recovery" && c.kind === "daemon")) continue;
      if (await signedBy(c, a)) return c;
    }
    return undefined;
  };
  let revoked = new Map<string, number>();
  for (let pass = 0; pass < 2; pass++) {
    const next = new Map<string, number>();
    for (const r of revocations) {
      if (r.v !== 1 || r.account !== trust.account) continue;
      const s = await valid(revoked, r.by, r.at, 0);
      if (s && approves(s.kind) && (await verify(s.sign, revocationBody(r), r.sig))) next.set(r.device, Math.min(next.get(r.device) ?? Infinity, r.at));
    }
    revoked = next;
  }
  const out = new Map<string, Cert>();
  for (const id of byId.keys()) {
    const c = await valid(revoked, id, Number.MAX_SAFE_INTEGER, 0);
    if (c) out.set(id, c);
  }
  return out;
}

/** The code a joining daemon prints, recomputed from its certificate. */
export async function joinCode(c: Cert): Promise<string> {
  const ALPHABET = "ABCDEFGHJKMNPQRSTVWXYZ0123456789";
  const h = await sha256(enc.encode(`illogical join\n${c.noise}\n${c.sign}\n`));
  let bits = new DataView(h.buffer).getBigUint64(0);
  let s = "";
  for (let i = 0; i < 10; i++) {
    if (i === 5) s += "-";
    s += ALPHABET[Number(bits >> 59n)];
    bits = (bits << 5n) & 0xffffffffffffffffn;
  }
  return s;
}

export function normalizeCode(s: string): string | null {
  const c = s.replace(/[^A-Za-z0-9]/g, "").toUpperCase();
  return c.length === 10 ? `${c.slice(0, 5)}-${c.slice(5)}` : null;
}
