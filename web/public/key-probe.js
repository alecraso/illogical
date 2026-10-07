// The device key probe (#94): store non-extractable Ed25519 and X25519
// keys in IndexedDB the way Arugula does, reload, and check they still
// sign and agree. Also checks the fallback Arugula uses where they
// don't (PKCS#8 wrapped with a non-extractable AES-GCM key). Plain script,
// no build step and no network, so it can be opened on any browser.

const subtle = crypto.subtle;
const DB = "arugula-key-probe";
const $ = (id) => document.getElementById(id);

function open() {
  return new Promise((res, rej) => {
    const r = indexedDB.open(DB, 1);
    r.onupgradeneeded = () => r.result.createObjectStore("kv");
    r.onsuccess = () => res(r.result);
    r.onerror = () => rej(r.error);
  });
}

async function kv(mode, fn) {
  const db = await open();
  return new Promise((res, rej) => {
    const tx = db.transaction("kv", mode);
    const req = fn(tx.objectStore("kv"));
    tx.oncomplete = () => res(req ? req.result : undefined);
    tx.onerror = () => rej(tx.error);
    tx.onabort = () => rej(tx.error);
  });
}

const put = (k, v) => kv("readwrite", (s) => void s.put(v, k));
const get = (k) => kv("readonly", (s) => s.get(k));
const raw = async (k) => new Uint8Array(await subtle.exportKey("raw", k));
const same = (a, b) => a.length === b.length && a.every((x, i) => x === b[i]);

/** Signs what `pub` (raw) verifies. Throws with why not. */
async function signs(priv, pub) {
  const msg = new TextEncoder().encode("arugula key probe");
  const sig = await subtle.sign("Ed25519", priv, msg);
  const key = await subtle.importKey("raw", pub, { name: "Ed25519" }, false, ["verify"]);
  if (!(await subtle.verify("Ed25519", key, sig, msg))) throw new Error("the signature doesn't verify");
}

/** Agrees on a secret with a fresh key, over X25519. */
async function agrees(priv, pub) {
  const other = await subtle.generateKey({ name: "X25519" }, false, ["deriveBits"]);
  const mine = await subtle.importKey("raw", pub, { name: "X25519" }, true, []);
  const a = new Uint8Array(await subtle.deriveBits({ name: "X25519", public: other.publicKey }, priv, 256));
  const b = new Uint8Array(await subtle.deriveBits({ name: "X25519", public: mine }, other.privateKey, 256));
  if (!same(a, b)) throw new Error("the secrets differ");
}

const missing = (v) => (v === null ? "came back as null" : "came back empty");

/** Each check: what it stores, and how to check what comes back. */
const CHECKS = [
  {
    key: "ed",
    what: "Ed25519 key (signs approvals)",
    async make() {
      const k = await subtle.generateKey({ name: "Ed25519" }, false, ["sign", "verify"]);
      return { keys: k, pub: await raw(k.publicKey) };
    },
    async check(v) {
      if (!v || !v.keys) throw new Error(missing(v));
      await signs(v.keys.privateKey, v.pub);
    },
  },
  {
    key: "x",
    what: "X25519 key (Noise, to reach machines)",
    async make() {
      const k = await subtle.generateKey({ name: "X25519" }, false, ["deriveBits"]);
      return { keys: k, pub: await raw(k.publicKey) };
    },
    async check(v) {
      if (!v || !v.keys) throw new Error(missing(v));
      await agrees(v.keys.privateKey, v.pub);
    },
  },
  {
    key: "both",
    what: "Both in one record, as Arugula stored them before #94",
    async make() {
      const sign = await subtle.generateKey({ name: "Ed25519" }, false, ["sign", "verify"]);
      const noise = await subtle.generateKey({ name: "X25519" }, false, ["deriveBits"]);
      return { sign, noise, signPub: await raw(sign.publicKey), noisePub: await raw(noise.publicKey) };
    },
    async check(v) {
      if (!v || !v.sign) throw new Error(missing(v));
      await signs(v.sign.privateKey, v.signPub);
      await agrees(v.noise.privateKey, v.noisePub);
    },
  },
  {
    key: "wrapped",
    what: "Both wrapped with an AES-GCM key (Arugula's fallback)",
    async make() {
      const sign = await subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
      const noise = await subtle.generateKey({ name: "X25519" }, true, ["deriveBits"]);
      const wrap = await subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["wrapKey", "unwrapKey"]);
      const seal = async (k) => {
        const iv = crypto.getRandomValues(new Uint8Array(12));
        return { iv, key: await subtle.wrapKey("pkcs8", k, wrap, { name: "AES-GCM", iv }) };
      };
      return { wrap, sign: await seal(sign.privateKey), noise: await seal(noise.privateKey), signPub: await raw(sign.publicKey), noisePub: await raw(noise.publicKey) };
    },
    async check(v) {
      if (!v || !v.wrap) throw new Error(missing(v));
      const open = (s, alg, use) => subtle.unwrapKey("pkcs8", s.key, v.wrap, { name: "AES-GCM", iv: s.iv }, { name: alg }, false, use);
      await signs(await open(v.sign, "Ed25519", ["sign"]), v.signPub);
      await agrees(await open(v.noise, "X25519", ["deriveBits"]), v.noisePub);
    },
  },
];

async function run(c) {
  try {
    await c.check(await get(c.key));
    return { ok: true, why: "" };
  } catch (e) {
    return { ok: false, why: String((e && e.message) || e) };
  }
}

/** First load: store a fresh set, check it in this load, then reload. */
async function store() {
  await kv("readwrite", (s) => void s.clear());
  const before = {};
  for (const c of CHECKS) {
    try {
      await put(c.key, await c.make());
      before[c.key] = await run(c);
    } catch (e) {
      before[c.key] = { ok: false, why: `can't make or store it: ${(e && e.message) || e}` };
    }
  }
  await put("before", before);
  location.hash = "reloaded";
  location.reload();
}

function browser() {
  const ua = navigator.userAgent;
  const version = /Version\/([\d.]+)/.exec(ua)?.[1];
  const ios = /(iPhone|iPad)[^)]*OS ([\d_]+)/.exec(ua);
  const safari = /Safari\//.test(ua) && !/Chrome\/|Chromium\/|Edg\/|Firefox\/|CriOS\/|FxiOS\//.test(ua);
  const where = ios ? `${ios[1]}, iOS ${ios[2].replace(/_/g, ".")}` : /Mac OS X/.test(ua) ? "macOS" : /Linux/.test(ua) ? "Linux" : "";
  if (safari && version) return `Safari ${version}${where ? ` on ${where}` : ""}`;
  return `Not Safari${where ? `, on ${where}` : ""}: see the user agent below`;
}

function row(what, before, after) {
  const tr = document.createElement("tr");
  const cell = (r) => {
    const td = document.createElement("td");
    td.className = r.ok ? "yes" : "no";
    td.textContent = r.ok ? "works" : `fails: ${r.why}`;
    return td;
  };
  const name = document.createElement("td");
  name.textContent = what;
  tr.append(name, cell(before), cell(after));
  return tr;
}

async function report() {
  const before = (await get("before")) || {};
  const after = {};
  for (const c of CHECKS) after[c.key] = await run(c);
  const head = document.createElement("tr");
  for (const t of ["", "Same page load", "After a reload"]) {
    const td = document.createElement("td");
    td.className = "dim";
    td.textContent = t;
    head.append(td);
  }
  $("rows").append(head, ...CHECKS.map((c) => row(c.what, before[c.key] || { ok: false, why: "not run" }, after[c.key])));
  const v = $("verdict");
  let verdict;
  if (after.ed.ok && after.x.ok && after.both.ok) {
    verdict = "keys";
    v.className = "yes";
    v.textContent = "Yes: device keys survive a reload here.";
  } else if (after.wrapped.ok) {
    verdict = "wrapped";
    v.className = "no";
    v.textContent = "No: device keys don't survive a reload here. Arugula's fallback (wrapped keys) does, so Arugula keeps them wrapped.";
  } else {
    verdict = "none";
    v.className = "no";
    v.textContent = "No, and the fallback doesn't work either: Arugula can't keep a device key in this browser.";
  }
  done({ verdict, browser: browser(), ua: navigator.userAgent, before, after });
}

/** For a test driving a real browser (safaridriver): the result, as JSON
 * in #result and `data-verdict` on #verdict (keys, wrapped, none or error). */
function done(result) {
  $("result").textContent = JSON.stringify(result);
  $("verdict").dataset.verdict = result.verdict;
}

$("browser").textContent = browser();
$("ua").textContent = navigator.userAgent;
$("again").onclick = () => {
  history.replaceState(null, "", location.pathname);
  location.reload();
};
(location.hash === "#reloaded" ? report() : store()).catch((e) => {
  $("verdict").className = "no";
  $("verdict").textContent = `The probe itself failed: ${(e && e.message) || e}`;
  done({ verdict: "error", browser: browser(), ua: navigator.userAgent, error: String((e && e.message) || e) });
});
