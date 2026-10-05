// The approving device (device.ts) from a shell, for tests that aren't
// TypeScript (testnet/test.sh, Rust). The device lives in a state file
// between steps. Each command prints one JSON object on stdout and exits 0,
// or prints why on stderr and exits 1.
//
//   node --experimental-strip-types device-cli.ts --state FILE COMMAND ...
//
//   signin --control URL [--via FROM=TO]... [--login L] [--name N]
//                          sign in and enroll: {account, fingerprint, device, approved}
//   approve CODE           approve a daemon's join code: {device, name}
//   devices                the account's trusted devices: {devices: [{id, kind, name}]}
//   online NAME [SECS]     wait for a daemon to be online: {id, name, online, urls}
//   pane NAME MARKER [SECS] round-trip MARKER through its first pane, over
//                          control's relay: {id, name}

import { fingerprint } from "../src/e2e/cert.ts";
import { Device } from "./device.ts";

const args = process.argv.slice(2);
const opt = (name: string): string[] => {
  const out: string[] = [];
  for (let i = args.indexOf(name); i >= 0; i = args.indexOf(name)) {
    out.push(args[i + 1]);
    args.splice(i, 2);
  }
  return out;
};
const [state] = opt("--state");
const [control] = opt("--control");
const via = Object.fromEntries(opt("--via").map((v) => [v.slice(0, v.indexOf("=")), v.slice(v.indexOf("=") + 1)]));
const [login] = opt("--login");
const [name] = opt("--name");
const [cmd, ...rest] = args;
const out = (v: unknown) => console.log(JSON.stringify(v));

async function main() {
  if (!state) throw new Error("--state FILE is required");
  if (cmd === "signin") {
    if (!control) throw new Error("signin needs --control URL");
    const d = await Device.signIn({ control, via, login, name });
    await d.save(state);
    return out({ account: d.account, fingerprint: fingerprint(d.root), device: d.keys.id, approved: d.approved });
  }
  const d = await Device.load(state);
  const secs = (s: string | undefined, def: number) => (s ? Number(s) : def) * 1000;
  const find = async (n: string) => {
    const hit = (await d.directory()).find((x) => x.id === n || x.name === n);
    if (!hit) throw new Error(`control lists no daemon ${n}`);
    return hit;
  };
  switch (cmd) {
    case "approve": {
      const c = await d.approveJoin(rest[0]);
      return out({ device: c.device, name: c.name });
    }
    case "devices": {
      const t = await d.trusted();
      return out({ devices: [...t.values()].map((c) => ({ id: c.device, kind: c.kind, name: c.name })) });
    }
    case "online":
      return out(await d.waitOnline(rest[0], secs(rest[1], 30)));
    case "pane": {
      const box = await find(rest[0]);
      await d.roundTrip(box.id, rest[1], { timeoutMs: secs(rest[2], 15) });
      return out({ id: box.id, name: box.name });
    }
    default:
      throw new Error(`unknown command ${cmd ?? "(none)"}: signin, approve, devices, online, pane`);
  }
}

main().then(
  () => process.exit(0),
  (e: Error) => {
    console.error(`device: ${e.message}`);
    process.exit(1);
  },
);
