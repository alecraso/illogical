// #178: the Mac app in cloud mode, with no person. Control, a fake GitHub
// and a joined machine ("box") run here; the app runs in a fresh tart VM
// (testnet/macos/vm.sh) from the release zip, or ARUGULA_MACOS_APP_ZIP.
// The person is web/fixtures/device.ts, signed in as alice:
//
//   signin    The app starts signing in (ARUGULA_SIGNIN_AUTO=1) and opens
//             control's /#app=<id> in the VM's Safari; the test reads that
//             URL (AppleScript), allows it as alice, and hands the grant to
//             the app's loopback port, as alice's browser would.
//   approve   The app's window redeems it, enrolls as a new device and
//             waits; alice approves it.
//   machines  The daemon the app installed on the Mac joins too (macvm).
//             The app's window, read through the accessibility tree (JXA
//             and System Events), connects to a machine, and its Hosts menu
//             lists box and macvm.
//   reach     Text typed into the app's terminal (System Events keystrokes)
//             runs in that machine's pane.
//
//   node --experimental-strip-types testnet/macos/app-cloud.ts
//   (or testnet/macos/test.sh app)
//
// BREAK=1: alice never approves the app; approve, machines and reach must
// then fail.
//
// Exit codes: 0 every check held, 1 one failed. KEEP=1 leaves the VM up.

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { certBody, type Cert } from "../../web/src/e2e/cert.ts";
import { signText } from "../../web/src/e2e/keys.ts";
import { Device } from "../../web/fixtures/device.ts";
import { fakeGithub } from "../../web/fixtures/fakes.ts";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "../..");
const target = process.env.ARUGULA_MACOS_BIN ?? join(root, "target/debug");
const vm = process.env.ARUGULA_MACOS_VM ?? "arugula-macos";
const zip = process.env.ARUGULA_MACOS_APP_ZIP ?? "https://github.com/arugula-salad/illogical/releases/download/v0.17.0/arugula-desktop-macos-arm64.zip";

const procs: ChildProcess[] = [];
const dirs: string[] = [];
let failed = 0;
const check = (what: string, ok: boolean, detail = "") => {
  console.log(`[macos app ${what}] ${ok ? "ok" : "FAIL"}${detail ? `: ${detail}` : ""}`);
  if (!ok) failed++;
};
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const temp = (w: string) => {
  const d = mkdtempSync(join(tmpdir(), `arugula-macos-app-${w}-`));
  dirs.push(d);
  return d;
};
const v = (...args: string[]) => execFileSync(join(here, "vm.sh"), [args[0], vm, ...args.slice(1)], { encoding: "utf8" }).trim();
/** AppleScript in the VM, from stdin. */
const osa = (script: string) => execFileSync(join(here, "vm.sh"), ["ssh", vm, "osascript", "-"], { input: script, encoding: "utf8" }).trim();
/** JavaScript for Automation in the VM: `ax` is the app's window. */
const jxa = (body: string) =>
  execFileSync(join(here, "vm.sh"), ["ssh", vm, "osascript", "-l", "JavaScript", "-"], {
    input: `const se = Application("System Events");
const proc = se.processes.byName("arugula-desktop");
const all = () => proc.windows[0].entireContents();
const g = (e, k) => { try { const v = e[k](); return v == null ? "" : String(v); } catch (_) { return ""; } };
const find = (role, name) => all().find((e) => g(e, "role") === role && (name === undefined || g(e, "name").startsWith(name) || g(e, "help") === name));
${body}`,
    encoding: "utf8",
  }).trim();
async function poll<T>(what: string, f: () => T | undefined | Promise<T | undefined>, ms = 30_000): Promise<T> {
  const until = Date.now() + ms;
  let last: unknown;
  for (;;) {
    try {
      const x = await f();
      if (x) return x;
    } catch (e) {
      last = e;
    }
    if (Date.now() > until) throw new Error(`timed out waiting for ${what}${last ? ` (${last})` : ""}`);
    await sleep(500);
  }
}

function cleanup() {
  for (const p of procs) p.kill("SIGKILL");
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
  if (!process.env.KEEP) v("down");
}

try {
  // Control and its fake GitHub, here.
  const gh = fakeGithub(0);
  await new Promise((r) => gh.once("listening", r));
  const github = `http://127.0.0.1:${(gh.address() as AddressInfo).port}`;
  const db = join(temp("control"), "control.db");
  procs.push(
    spawn(`${target}/arugula-control`, [
      ...["--listen", "127.0.0.1:0", "--public-url", "http://127.0.0.1:0", "--db", db, "--static-dir", join(root, "web/dist")],
      ...["--github-client-id", "id", "--github-client-secret", "s", "--github-url", github, "--github-api", github],
    ], { stdio: "ignore" }),
  );
  const port = await poll("control's port", () => {
    try {
      return Number(readFileSync(join(dirname(db), "listen"), "utf8").trim().split(":").pop());
    } catch {
      return undefined;
    }
  });
  const control = `http://127.0.0.1:${port}`;
  await poll("control", () => fetch(`${control}/control.json`).then((r) => r.ok));

  // Alice, signed in: the account's first device.
  const alice = await Device.signIn({ control, login: "alice", name: "laptop" });

  // A machine on the account, here.
  const box = temp("box");
  const joining = spawn(`${target}/arugulad`, ["join", control, "--name", "box", "--state-dir", box, "--account", alice.keys.id], { stdio: ["ignore", "pipe", "inherit"] });
  procs.push(joining);
  const code = await new Promise<string>((res) => {
    let out = "";
    joining.stdout!.on("data", (d) => {
      out += d;
      const m = out.match(/#join=([A-Z0-9]{5}-[A-Z0-9]{5})/);
      if (m) res(m[1]);
    });
  });
  await alice.approveJoin(code);
  await new Promise((r) => joining.on("exit", r));
  procs.push(
    spawn(`${target}/arugulad`, [
      ...["--listen", "127.0.0.1:0", "--name", "box", "--state-dir", box],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent", "--no-claude-ide"],
    ], { stdio: "ignore" }),
  );
  const listed = await alice.waitOnline("box");

  // The VM, with control's port on its loopback too (a secure context, and
  // the same origin as here).
  v("down");
  v("up");
  const ssh = v("sshcmd");
  procs.push(spawn("sh", ["-c", `exec ${ssh} -N -o ExitOnForwardFailure=yes -R 127.0.0.1:${port}:127.0.0.1:${port}`], { stdio: "ignore" }));
  await poll("the tunnel", () => {
    v("ssh", `curl -sf -o /dev/null ${control}/control.json`);
    return true;
  });
  v("ssh", `set -e; curl -sSL -o /tmp/app.zip '${zip}'; ditto -x -k /tmp/app.zip /Applications`);
  const version = v("ssh", "defaults read /Applications/arugula.app/Contents/Info CFBundleShortVersionString");

  // The app's window as text, through the accessibility tree.
  const page = () => jxa(`all().map((e) => ["role", "name", "value", "description", "help"].map((k) => g(e, k)).join(" | ")).join("\\n")`);

  // 1. The app asks; its sign-in page opens in Safari.
  v("ssh", `open --env ARUGULA_CONTROL=${control} --env ARUGULA_SIGNIN_AUTO=1 -a /Applications/arugula.app`);
  const id = await poll(
    "the app's sign-in in Safari",
    () => {
      const urls = osa('tell application "Safari" to get URL of every document');
      return new RegExp(`${control.replace(/[.]/g, "\\.")}/#app=([0-9a-f]+)`).exec(urls)?.[1];
    },
    60_000,
  );
  check("signin opens control's page in the browser", true, id);
  const shown = await alice.api<{ name: string; code: string }>(`/api/app-login/${id}`);
  const { redirect } = await alice.api<{ redirect: string }>(`/api/app-login/${id}/allow`, {});
  // What alice's page does with it: open the app's loopback address.
  v("ssh", `curl -sS -o /dev/null -w '%{http_code}\\n' '${redirect}'`);

  // 2. The app redeems, enrolls and waits; alice approves it.
  const pending = await poll("the app's device request", async () => {
    const all = await alice.api<{ pending: Cert[] }>("/api/devices");
    // The app's request, the only one waiting.
    return all.pending?.find((c) => c.device !== alice.keys.id);
  }).catch((e) => {
    throw new Error(`${e}\nthe app's window:\n${page()}`);
  });
  const signed: Cert = { ...pending, account: alice.account, approver: alice.keys.id, sig: "" };
  signed.sig = await signText(alice.keys, certBody(signed));
  // BREAK=1: nobody approves it, and the checks below must fail.
  if (!process.env.BREAK) await alice.api(`/api/devices/${pending.device}/approve`, { cert: signed });
  check("approve", (await alice.trusted()).has(pending.device), `${shown.name} (${pending.kind})`);

  // The Mac itself joins too: the daemon the app installed there.
  const vmJoin = spawn("sh", ["-c", `exec ${ssh} '~/.local/bin/arugulad join ${control} --name macvm --account ${alice.keys.id}'`], { stdio: ["ignore", "pipe", "inherit"] });
  procs.push(vmJoin);
  const vmCode = await new Promise<string>((res) => {
    let out = "";
    vmJoin.stdout!.on("data", (d) => {
      out += d;
      const m = out.match(/#join=([A-Z0-9]{5}-[A-Z0-9]{5})/);
      if (m) res(m[1]);
    });
  });
  await alice.approveJoin(vmCode);
  await new Promise((r) => vmJoin.on("exit", r));
  await alice.waitOnline("macvm", 60_000);

  // 3. The app's window reaches a machine (its Hosts button names it),
  // and its Hosts menu lists every machine: box and the Mac it runs on.
  const host = await poll("a machine in the app's window", () => /^AXButton \| (box|macvm)\S* ▾ \|.*\| Hosts$/m.exec(page())?.[1], 60_000).catch(() => "");
  const menu = host ? jxa(`find("AXButton", "Hosts").click(); delay(1); all().map((e) => g(e, "name") + " " + g(e, "value")).join("\\n")`) : "";
  if (host) jxa(`se.keyCode(53)`); // Escape closes the menu
  const every = (t: string) => /\bbox\b/.test(t) && /\bmacvm\b/.test(t);
  check("machines", every(menu), `app ${version}; showing ${host || "nothing"}; box ${listed.id}, macvm`);
  // Typing in its terminal reaches that machine's pane. (The first key
  // after focusing can go missing: a space goes first.)
  const mark = "APP-REACHES-$((5*5))";
  if (host) {
    jxa(`proc.frontmost = true; const t = find("AXTextArea", "Terminal input"); t.focused = true; delay(0.5); se.keystroke(" "); delay(0.5); se.keystroke(${JSON.stringify(`echo ${mark}`)}); se.keyCode(36);`);
  }
  const captured = () =>
    host === "box"
      ? execFileSync(`${target}/arugula`, ["--socket", join(box, "sock"), "capture", "%1"], { encoding: "utf8" })
      : v("ssh", "~/.local/bin/arugula capture %1");
  const reached = await poll("the marker in the pane", () => (host && /^APP-REACHES-25$/m.test(captured()) ? true : undefined), 20_000).catch(() => false);
  check("reach", reached, `typed in the app, ran on ${host || "nothing"}`);
  if (!reached && host) console.log(`${host}'s pane:\n${captured()}`);
  if (process.env.DUMP || !every(menu)) console.log(`the app's window:\n${menu || page()}`);
} catch (e) {
  check("run", false, String((e as Error)?.stack ?? e));
} finally {
  cleanup();
}
process.exit(failed ? 1 : 0);
