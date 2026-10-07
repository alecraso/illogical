// Noise_IK_25519_AESGCM_SHA256, initiator only, on WebCrypto alone
// (S15; the Rust side is `arugula_e2e::channel`).
//
// Every primitive is in WebCrypto: X25519 (deriveBits), AES-256-GCM,
// SHA-256 and HMAC. So the device's static private key can be a
// non-extractable CryptoKey kept in IndexedDB: the page can use it but
// never read it, and no crypto library ships in the bundle.
//
// IK: the client knows the daemon's static key (from the directory, signed
// by an approving device), so attach is one round trip:
//   -> e, es, s, ss   (+ payload, encrypted)
//   <- e, ee, se      (+ payload, encrypted)

const subtle = globalThis.crypto.subtle;
const NAME = "Noise_IK_25519_AESGCM_SHA256";
const TAG = 16;

export type Bytes = Uint8Array<ArrayBuffer>;

export function cat(...parts: Uint8Array[]): Bytes {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
}

export async function sha256(b: Uint8Array): Promise<Bytes> {
  return new Uint8Array(await subtle.digest("SHA-256", b as Bytes));
}

async function hmac(key: Uint8Array, data: Uint8Array): Promise<Bytes> {
  const k = await subtle.importKey("raw", key as Bytes, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return new Uint8Array(await subtle.sign("HMAC", k, data as Bytes));
}

/** Noise's HKDF with two outputs. */
async function hkdf2(ck: Uint8Array, ikm: Uint8Array): Promise<[Bytes, Bytes]> {
  const temp = await hmac(ck, ikm);
  const o1 = await hmac(temp, new Uint8Array([1]));
  const o2 = await hmac(temp, cat(o1, new Uint8Array([2])));
  return [o1, o2];
}

function nonce(n: number): Bytes {
  // 4 zero bytes, then the counter as a big-endian u64.
  const iv = new Uint8Array(12);
  new DataView(iv.buffer).setBigUint64(4, BigInt(n));
  return iv;
}

/** One direction of a transport (or the handshake's cipher state). */
export class Cipher {
  private n = 0;
  private key: CryptoKey;
  private constructor(key: CryptoKey) {
    this.key = key;
  }

  static async of(raw: Uint8Array): Promise<Cipher> {
    return new Cipher(await subtle.importKey("raw", raw as Bytes, "AES-GCM", false, ["encrypt", "decrypt"]));
  }

  async seal(plain: Uint8Array, ad: Uint8Array = new Uint8Array()): Promise<Bytes> {
    const iv = nonce(this.n++);
    return new Uint8Array(await subtle.encrypt({ name: "AES-GCM", iv, additionalData: ad as Bytes }, this.key, plain as Bytes));
  }

  async open(ct: Uint8Array, ad: Uint8Array = new Uint8Array()): Promise<Bytes> {
    const iv = nonce(this.n++);
    return new Uint8Array(await subtle.decrypt({ name: "AES-GCM", iv, additionalData: ad as Bytes }, this.key, ct as Bytes));
  }
}

async function dh(priv: CryptoKey, pub: Uint8Array): Promise<Bytes> {
  const k = await subtle.importKey("raw", pub as Bytes, { name: "X25519" }, true, []);
  return new Uint8Array(await subtle.deriveBits({ name: "X25519", public: k }, priv, 256));
}

export async function publicRaw(k: CryptoKey): Promise<Bytes> {
  return new Uint8Array(await subtle.exportKey("raw", k));
}

/** A device key pair: the private half can't be exported. */
export async function deviceKey(): Promise<CryptoKeyPair> {
  return (await subtle.generateKey({ name: "X25519" }, false, ["deriveBits"])) as CryptoKeyPair;
}

class Symmetric {
  ck!: Bytes;
  h!: Bytes;
  k: Cipher | undefined;

  static async init(prologue: Uint8Array): Promise<Symmetric> {
    const s = new Symmetric();
    const name = new TextEncoder().encode(NAME);
    s.h = name.length <= 32 ? cat(name, new Uint8Array(32 - name.length)) : await sha256(name);
    s.ck = s.h;
    await s.mixHash(prologue);
    return s;
  }

  async mixHash(d: Uint8Array) {
    this.h = await sha256(cat(this.h, d));
  }

  async mixKey(ikm: Uint8Array) {
    const [ck, k] = await hkdf2(this.ck, ikm);
    this.ck = ck;
    this.k = await Cipher.of(k);
  }

  async encryptAndHash(p: Uint8Array): Promise<Bytes> {
    const c = this.k ? await this.k.seal(p, this.h) : (p as Bytes);
    await this.mixHash(c);
    return c;
  }

  async decryptAndHash(c: Uint8Array): Promise<Bytes> {
    const p = this.k ? await this.k.open(c, this.h) : (c as Bytes);
    await this.mixHash(c);
    return p;
  }

  async split(): Promise<[Cipher, Cipher]> {
    const [a, b] = await hkdf2(this.ck, new Uint8Array());
    return [await Cipher.of(a), await Cipher.of(b)];
  }
}

export interface Split {
  send: Cipher;
  recv: Cipher;
  /** The handshake hash: both ends have the same one (channel binding). */
  hash: Bytes;
}

/**
 * The initiator. `write` sends message 1 and returns; `read` takes message
 * 2 and finishes. `payload`s are encrypted (msg 1's to the daemon's key).
 */
export class Initiator {
  private sym!: Symmetric;
  private e!: CryptoKeyPair;
  private s: CryptoKeyPair;
  private rs: Uint8Array;

  constructor(s: CryptoKeyPair, rs: Uint8Array) {
    this.s = s;
    this.rs = rs;
  }

  async write(payload: Uint8Array, prologue: Uint8Array = new Uint8Array()): Promise<Bytes> {
    this.sym = await Symmetric.init(prologue);
    await this.sym.mixHash(this.rs); // <- s
    this.e = (await subtle.generateKey({ name: "X25519" }, false, ["deriveBits"])) as CryptoKeyPair;
    const ePub = await publicRaw(this.e.publicKey);
    await this.sym.mixHash(ePub); // e
    await this.sym.mixKey(await dh(this.e.privateKey, this.rs)); // es
    const sCt = await this.sym.encryptAndHash(await publicRaw(this.s.publicKey)); // s
    await this.sym.mixKey(await dh(this.s.privateKey, this.rs)); // ss
    return cat(ePub, sCt, await this.sym.encryptAndHash(payload));
  }

  async read(msg: Uint8Array): Promise<{ payload: Bytes; channel: Split }> {
    if (msg.length < 32 + TAG) throw new Error("short handshake message");
    const re = msg.subarray(0, 32);
    await this.sym.mixHash(re); // e
    await this.sym.mixKey(await dh(this.e.privateKey, re)); // ee
    await this.sym.mixKey(await dh(this.s.privateKey, re)); // se
    const payload = await this.sym.decryptAndHash(msg.subarray(32));
    const [send, recv] = await this.sym.split();
    return { payload, channel: { send, recv, hash: this.sym.h } };
  }
}
