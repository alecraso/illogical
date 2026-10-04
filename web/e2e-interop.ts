// The browser's e2e code against Rust's (`crates/e2e/examples/interop.rs`):
// the same verdicts on the same certificates, signatures each side
// accepts from the other, and a channel carrying every message kind.
//   just e2e-interop    (or: node --experimental-strip-types e2e-interop.ts)

import { execFileSync, spawn } from "node:child_process";
import { evaluate, certBody, joinCode, type Cert } from "./src/e2e/cert.ts";
import { generateKeys, signText } from "./src/e2e/keys.ts";
import { E2ESocket } from "./src/e2e/channel.ts";
import { follows, newInviteKey, redeemInvite, signInvite, signRedeem, signRoster, type Roster, type TeamPin } from "./src/e2e/team.ts";

const bin = process.env.INTEROP_BIN ?? "../target/debug/examples/interop";
let failed = 0;
const check = (what: string, ok: boolean, detail = "") => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}${detail ? `: ${detail}` : ""}`);
  if (!ok) failed++;
};

// 1. Rust's certificates, evaluated here.
const fx = JSON.parse(execFileSync(bin, ["fixtures"]).toString());
const trusted = [...(await evaluate(fx.trust, fx.certs, fx.revocations)).keys()].sort();
check("same trusted set as Rust", JSON.stringify(trusted) === JSON.stringify(fx.trusted), trusted.join(","));
check("same join code", (await joinCode(fx.daemon)) === fx.joinCode, fx.joinCode);

// 2. Certificates signed here, evaluated by Rust.
const root = await generateKeys();
const phone = await generateKeys();
const mk = async (k: typeof root, by: typeof root, name: string, created: number): Promise<Cert> => {
  const c: Cert = { v: 1, account: "acct2", device: k.id, kind: "browser", name, noise: k.noisePub, sign: k.signPub, created, approver: by.id, sig: "" };
  c.sig = await signText(by, certBody(c));
  return c;
};
const certs = [await mk(root, root, "laptop ✓", 1), await mk(phone, root, "phone", 2)];
const rust = JSON.parse(execFileSync(bin, ["check"], { input: JSON.stringify({ trust: { account: "acct2", root: root.id }, certs }) }).toString());
check("Rust trusts what we signed", JSON.stringify(rust) === JSON.stringify([root.id, phone.id].sort()));

// 3. A team roster and a presigned invite signed here: Rust and this code
// agree on each version, and on a tampered one.
{
  const alice = await generateKeys();
  const bob = await generateKeys();
  const self = async (k: typeof alice, account: string): Promise<Cert> => {
    const c: Cert = { v: 1, account, device: k.id, kind: "browser", name: account, noise: k.noisePub, sign: k.signPub, created: 1, approver: k.id, sig: "" };
    c.sig = await signText(k, certBody(c));
    return c;
  };
  const tc = { alice: [[await self(alice, "alice")], []], bob: [[await self(bob, "bob")], []] } as Record<string, [Cert[], []]>;
  const pin: TeamPin = { team: "0123456789abcdef", founder: "alice", founder_root: alice.id };
  const v1 = await signRoster(
    { v: 1, team: pin.team, name: "Acme", version: 1, at: 1, members: [{ account: "alice", root: alice.id, role: "owner", name: "alice" }] },
    alice,
  );
  const { seed, key } = await newInviteKey();
  const inv = await signInvite({ team: pin.team, role: "editor", expires: Date.now() + 86_400_000, key }, alice);
  const v2 = await redeemInvite(v1, inv, seed, { account: "bob", root: bob.id, name: "bob" });
  const both = async (what: string, prev: Roster | null, r: Roster, want: boolean) => {
    const rs = execFileSync(bin, ["roster"], { input: JSON.stringify({ pin, prev, roster: r, certs: tc }) }).toString().trim() === "true";
    const ts = await follows(r, prev, pin, tc);
    check(what, rs === want && ts === want, `rust ${rs}, ts ${ts}`);
  };
  await both("team v1 follows", null, v1, true);
  await both("a presigned invite redeemed here checks out in Rust", v1, v2, true);
  const raised = await signRedeem({ ...v2, members: v2.members.map((m) => (m.account === "bob" ? { ...m, role: "owner" as const } : m)) }, seed);
  await both("a raised role doesn't", v1, raised, false);
  await both("nor one the invitee's device signs", v1, await signRoster(v2, bob), false);
  const later = await signRoster({ ...v1, at: Date.now() + 60_000 }, alice);
  await both("nor one backdated before the version it follows", later, await signRedeem({ ...(await redeemInvite(later, inv, seed, { account: "bob", root: bob.id, name: "bob" })), at: 2 }, seed), false);
  // Shapes Rust refuses before checking a signature, refused here too.
  const odd = (r: Roster, f: (x: any) => void) => { const x = structuredClone(r) as any; f(x); return x as Roster; };
  await both("nor an expiry written as a string", v1, await signRedeem(odd(v2, (x) => (x.spent[0].expires = String(x.spent[0].expires))), seed), false);
  await both("nor a spent key with a space in it", v1, await signRedeem(odd(v2, (x) => (x.spent[0].key += " x")), seed), false);
  await both("nor a member name with a space in it", v1, await signRedeem(odd(v2, (x) => (x.members[1].name = "bob b")), seed), false);
  await both("nor the same invite again", v2, await redeemInvite(v2, inv, seed, { account: "carol", root: bob.id, name: "carol" }), false);
}

// 4. A channel to a Rust responder.
const child = spawn(bin, ["responder", "127.0.0.1:0"]);
const [addr, id, noise] = await new Promise<string[]>((r) => child.stdout.once("data", (d) => r(d.toString().trim().split(" "))));
try {
  const sock = await E2ESocket.connect([{ url: `ws://${addr}/`, timeoutMs: 2000 }], { id, noise }, phone);
  const texts: string[] = [];
  const bins: Uint8Array[] = [];
  sock.onText = (t) => texts.push(t);
  sock.onBinary = (b) => bins.push(b);
  sock.start();
  sock.sendText('{"type":"attach"}');
  const big = Uint8Array.from({ length: 100_000 }, (_, i) => i % 251);
  sock.sendBinary(big);
  const res = await sock.request("POST", "/api/run?x=1", { cmd: "ls" });
  await new Promise((r) => setTimeout(r, 200));
  check("text echoed", texts[0] === '{"type":"attach"}');
  check("100 KB binary echoed in chunks", bins[0]?.length === big.length && bins[0].every((b, i) => b === big[i]));
  const j = res.json<{ method: string; path: string; len: number }>();
  check("request answered", res.status === 201 && j.method === "POST" && j.path === "/api/run?x=1" && j.len === 12, JSON.stringify(j));
  sock.close();
  // A wrong daemon id (the relay splicing us elsewhere) fails the handshake.
  const wrong = await E2ESocket.connect([{ url: `ws://${addr}/`, timeoutMs: 2000 }], { id: "0000000000000000", noise }, phone).then(
    () => false,
    () => true,
  );
  check("wrong daemon refused", wrong);
} finally {
  child.kill();
}
process.exit(failed ? 1 : 0);
