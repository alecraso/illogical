// The control-mode screens (M17): signing in, waiting for this browser to
// be approved, approving other devices and daemons' join codes, the device
// list, and how to add a machine.

import { useEffect, useState } from "preact/hooks";
import { AlongsideError, cameToRecover, deviceName, inApp, inviteInHash, joinInHash, passkeyRegister, passkeySignIn, previewInvite, RefusedError, signInNext, type ControlSession, type JoinRequest } from "../control";
import { fingerprint, normalizeCode, type Cert } from "../e2e/cert.ts";
import { useSubscribe } from "./hooks";
import { directory } from "../hosts";
import { CopyButton, CopyText, download } from "./copy";
import { ROLE_HELP, roleAs, roleLabel } from "./roles";
import type { MenuItem } from "./menu";
import type { PresignedInvite, ShareOffer, Team } from "../control";
import type { TeamRole } from "../e2e/team.ts";
import { qr, qrPath } from "./qr";
import { AccountPanel } from "./account";
import { ConfirmRemove } from "./confirm";

export function useControl(s: ControlSession) {
  useSubscribe((fn) => s.subscribe(fn));
}

function Center({ children }: { children: preact.ComponentChildren }) {
  return <div class="control-center">{children}</div>;
}

/** The hosted control's terms (#172); a control you run yourself has its
 * own, or none. */
const HOSTED = "control.arugula.io";
const SITE = "https://illogical.widgets.wtf";

function HostedTerms() {
  if (location.hostname !== HOSTED) return null;
  return (
    <p class="control-legal dim" data-legal>
      Free during the beta, provided as is. By signing in you agree to the <a href={`${SITE}/terms`}>terms</a>; the{" "}
      <a href={`${SITE}/privacy`}>privacy notice</a> says what this service keeps.
    </p>
  );
}

/** Everything before the app: sign in, approval, no machines yet. */
export function ControlGate({ s }: { s: ControlSession }) {
  useControl(s);
  if (s.phase === "loading") return <Center>Connecting to control…</Center>;
  if (s.phase === "error")
    return (
      <Center>
        <h1>Something went wrong</h1>
        <p class="control-error">{s.error}</p>
        <button class="primary" onClick={() => location.reload()}>
          Try again
        </button>
      </Center>
    );
  if (s.phase === "signed-out") {
    const next = encodeURIComponent(signInNext());
    return (
      <Center>
        <h1>Arugula</h1>
        <WhyHere />
        <p>Your terminals, on every machine, from any device. End to end encrypted: this service introduces your devices to your machines and relays for them, but can't read what they say.</p>
        <div class="control-signins">
          {s.info.github ? (
            <a class="primary control-signin" href={`/auth/github?next=${next}`} data-signin="github">
              Sign in with GitHub
            </a>
          ) : null}
          {s.info.passkeys ? <PasskeyButtons /> : null}
        </div>
        {!s.info.github && !s.info.passkeys ? <p class="control-error">No sign-in is configured on this control.</p> : null}
        <HostedTerms />
      </Center>
    );
  }
  if (s.phase === "waiting") return <Waiting s={s} />;
  if (s.phase === "lost-key")
    return (
      <Center>
        <h1>This browser lost its device key</h1>
        <p data-lost-key>
          It can't read back the key it was approved with (<b>{s.enrollment?.cert.name ?? "this browser"}</b>, {fingerprint(s.enrollment?.cert.device ?? "")}), so it can't approve machines or reach them. Some versions of Safari lose keys this way.
        </p>
        <p>
          Enroll it again as a new device: another of your devices approves it, or a recovery code does. The old entry stays in <i>Devices and machines</i>.
        </p>
        <button class="primary" data-enroll-again onClick={() => void s.enrollAgain()}>
          Forget this browser and enroll again
        </button>
        <SignOuts s={s} />
      </Center>
    );
  // #327: the account doesn't trust this browser's key any more (removed,
  // or its approval went), so its approvals would be refused. Say so before
  // anyone clicks Approve.
  if (s.phase === "untrusted")
    return (
      <Center>
        <h1>This browser isn't trusted any more</h1>
        <p data-untrusted={s.untrustedWhy}>
          Your account {s.untrustedWhy === "removed" ? "removed this browser" : "no longer trusts this browser"} (<b>{s.enrollment?.cert.name ?? "this browser"}</b>,{" "}
          {fingerprint(s.keys.id)}), so it can't approve machines or devices, or reach your machines.
        </p>
        <p>
          Forget it here and enroll it again as a new device. Another of your devices approves it, or a recovery code does: the next screen asks for one.
        </p>
        <button class="primary" data-enroll-again onClick={() => void s.enrollAgain(true)}>
          Forget this browser and enroll again
        </button>
        <SignOuts s={s} />
      </Center>
    );
  if (s.phase === "turned-down")
    return (
      <Center>
        <h1>Turned down</h1>
        <p data-turned-down>
          <b>{s.turnedDownBy || "Another device"}</b> turned this browser down. If that was a mistake, ask again and approve it there.
        </p>
        <button class="primary" data-try-again onClick={() => s.tryAgain()}>
          Try again
        </button>
        <RecoveryForm s={s} label="Use a recovery code" />
        <SignOuts s={s} />
      </Center>
    );
  return null;
}

/** Sign out, keeping this browser's key for next time, or forgetting it
 * too (#327): a key the account no longer trusts only gets in the way. */
function SignOuts({ s }: { s: ControlSession }) {
  return (
    <p class="control-signouts">
      <button class="control-linkish" data-sign-out onClick={() => void s.signOut(false)}>
        Sign out
      </button>
      {" · "}
      <button class="control-linkish" data-sign-out-forget onClick={() => void s.signOut(true)}>
        Sign out and forget this browser
      </button>
    </p>
  );
}

/** Here from forgetting a stale browser: the recovery form starts open. */
let recover: boolean | undefined;
const recovering = () => (recover ??= cameToRecover());

/** This browser waits for a device the account trusts to approve it.
 * #326: it says it's the browser (or the app's window) being approved, by
 * name, and that adding a machine is a separate approval that doesn't need
 * this one. Here from a machine's approval link (`#join=`), it leads with
 * that: the code goes to a device already in the account, which sees this
 * browser's request alongside and approves both at once. */
function Waiting({ s }: { s: ControlSession }) {
  const [hash, setHash] = useState(location.hash);
  useEffect(() => {
    const on = () => setHash(location.hash);
    addEventListener("hashchange", on);
    return () => removeEventListener("hashchange", on);
  }, []);
  const code = joinInHash(hash);
  const name = deviceName();
  // "the Arugula app on jake-air", or "this browser (Chrome on Mac)".
  const what = inApp() ? `the ${name}` : `this browser (${name})`;
  const fp = (
    <p class="fingerprint" data-fingerprint={s.keys.id}>
      {fingerprint(s.keys.id)}
    </p>
  );
  if (code)
    return (
      <Center>
        <h1>Approve the machine on a device you use</h1>
        <p data-waiting-join={code}>
          You're signed in as <b>{s.login}</b>, here to add a machine with code <b>{code}</b>. Only a device already in your account can approve it,
          and this browser isn't one yet. On a browser or phone you use with Arugula, open:
        </p>
        <CopyText text={`${s.info.url}/#join=${code}`} data-control-join-link />
        <p>Approve the code there, and pick where the machine goes: your account, or a team you own. That's the one approval the machine needs.</p>
        <p class="dim" data-waiting-also>
          That device also lists {what} next to the machine, with this fingerprint. Approve it too only if you want this browser to reach your
          machines; the machine joins either way.
        </p>
        {fp}
        <p class="dim">Waiting…</p>
        <RecoveryForm s={s} open={recovering()} />
        <SignOuts s={s} />
      </Center>
    );
  return (
    <Center>
      <h1>{inApp() ? "Approve this app as a device" : "Approve this browser"}</h1>
      <p data-waiting-browser={name}>
        You're signed in as <b>{s.login}</b>. This approves {what} as one of your devices, so it can reach your machines. A device you already use
        approves it: open <CopyText inline text={s.info.url} data-control-url /> on that device; it asks there.
      </p>
      <p>It shows this fingerprint; check it matches:</p>
      {fp}
      <p class="dim">Waiting…</p>
      <p class="control-aside" data-waiting-machine>
        Here to add a machine to your account or a team? That's a separate approval, and it doesn't need this one. The machine shows a code (in
        Getting started, or where you ran <code>arugulad join</code>): approve that code on a device you already use.
      </p>
      <RecoveryForm s={s} open={recovering()} />
      <SignOuts s={s} />
    </Center>
  );
}

/** Signed out, but following a link (#103): say what it was for. The hash
 * survives signing in, so it opens once you're in. */
function WhyHere() {
  const [hash, setHash] = useState(location.hash);
  const invite = inviteInHash(hash);
  const [team, setTeam] = useState<{ name: string; by: string } | null>(null);
  useEffect(() => {
    const on = () => setHash(location.hash);
    addEventListener("hashchange", on);
    on();
    return () => removeEventListener("hashchange", on);
  }, []);
  useEffect(() => {
    setTeam(null);
    if (invite) void previewInvite(invite.team, invite.code, invite.presigned).then(setTeam);
  }, [hash]);
  if (invite)
    return (
      <p class="control-why" data-why="invite">
        {team ? (
          <>
            {team.by ? `${team.by} invited you` : "You've been invited"} to <b data-why-team>{team.name}</b>.
          </>
        ) : (
          "You've been invited to a team."
        )}{" "}
        Sign in or make an account to accept.
      </p>
    );
  if (/^#join=/.test(hash))
    return (
      <p class="control-why" data-why="join">
        Sign in to approve this machine.
      </p>
    );
  if (/^#app=/.test(hash))
    return (
      <p class="control-why" data-why="app">
        Sign in to let the Arugula app use your account.
      </p>
    );
  return null;
}

function PasskeyButtons() {
  const [err, setErr] = useState("");
  // #102: a new account says what teammates call it first.
  const [naming, setNaming] = useState(false);
  const [name, setName] = useState("");
  const go = (f: () => Promise<void>) =>
    f().then(
      () => location.reload(),
      (e: Error) => setErr(e.name === "NotAllowedError" ? "Cancelled." : e.message),
    );
  return (
    <>
      <button class="primary control-signin" data-signin="passkey" onClick={() => go(passkeySignIn)}>
        Sign in with a passkey
      </button>
      {naming ? (
        <form
          class="control-code control-name"
          onSubmit={(e) => {
            e.preventDefault();
            if (name.trim()) void go(() => passkeyRegister(name));
          }}
        >
          <input
            data-signup-name
            placeholder="Your name"
            maxLength={64}
            value={name}
            onInput={(e) => setName((e.target as HTMLInputElement).value)}
            aria-label="Your name"
            autoFocus
          />
          <button type="submit" class="primary" data-signup-go disabled={!name.trim()}>
            Make the account
          </button>
          <p class="dim">Teammates see this name. You can change it later.</p>
        </form>
      ) : (
        <button class="control-signin control-secondary" data-signup="passkey" onClick={() => setNaming(true)}>
          New here? Make an account with a passkey
        </button>
      )}
      {err ? <p class="control-error">{err}</p> : null}
    </>
  );
}

/** What other people see (#102), changed in place. */
function NameLine({ s }: { s: ControlSession }) {
  const [editing, setEditing] = useState(false);
  const [name, setName] = useState(s.name);
  const [err, setErr] = useState("");
  if (!editing)
    return (
      <p class="dim">
        Name <b data-account-name>{s.name || "none yet"}</b>{" "}
        <button
          class="control-linkish"
          data-edit-name
          onClick={() => {
            setName(s.name);
            setEditing(true);
          }}
        >
          Change
        </button>
      </p>
    );
  return (
    <form
      class="control-code control-name"
      onSubmit={(e) => {
        e.preventDefault();
        s.setName(name).then(
          () => setEditing(false),
          (x: Error) => setErr(x.message),
        );
      }}
    >
      <input data-name-input maxLength={64} value={name} onInput={(e) => setName((e.target as HTMLInputElement).value)} aria-label="Name" autoFocus />
      <button type="submit" data-save-name disabled={!name.trim()}>
        Save
      </button>
      {err ? <p class="control-error">{err}</p> : null}
    </form>
  );
}

function RecoveryForm({ s, label = "Lost your other devices? Use a recovery code", open: startOpen = false }: { s: ControlSession; label?: string; open?: boolean }) {
  const [open, setOpen] = useState(startOpen);
  const [code, setCode] = useState("");
  const [err, setErr] = useState("");
  // A button that looks like one (#327): as a link it read as a sentence.
  if (!open)
    return (
      <button class="control-secondary" data-use-recovery onClick={() => setOpen(true)}>
        {label}
      </button>
    );
  return (
    <form
      class="control-code"
      onSubmit={(e) => {
        e.preventDefault();
        s.useRecoveryCode(code).catch((x: Error) => setErr(x.message));
      }}
    >
      <input placeholder="Recovery code" value={code} onInput={(e) => setCode((e.target as HTMLInputElement).value)} aria-label="Recovery code" data-recovery-input autoFocus={startOpen} />
      <button type="submit">Use it</button>
      {err ? <p class="control-error">{err}</p> : null}
    </form>
  );
}

/** Once, after the account's first device (or new codes): the codes to keep. */
function RecoveryCodes({ s }: { s: ControlSession }) {
  const all = s.recoveryCodes!.join("\n") + "\n";
  const [stored, setStored] = useState(false);
  return (
    <Modal>
      <h2>Your recovery codes</h2>
      <p>If you lose every device that can approve new ones, one of these lets a new browser in. Each works once. Keep them somewhere safe and offline: they're shown only now, and this service never had them.</p>
      {s.recoveryCodes!.map((c) => (
        <p key={c}>
          <CopyText text={c} data-recovery-code />
        </p>
      ))}
      <div class="prompt-buttons">
        <CopyButton text={all} label="Copy both" />
        <button data-download-codes onClick={() => download("arugula-recovery-codes.txt", all)}>
          Download .txt
        </button>
      </div>
      <label class="control-check">
        <input type="checkbox" data-stored-codes checked={stored} onChange={(e) => setStored((e.target as HTMLInputElement).checked)} />
        I've stored these somewhere safe
      </label>
      <PasskeyNudge s={s} />
      <div class="prompt-buttons">
        <button class="primary" data-saved-codes disabled={!stored} onClick={() => s.savedRecoveryCodes()}>
          Continue
        </button>
      </div>
    </Modal>
  );
}

/** Ready, but no machine to show yet: the account's menu (#97), any team
 * this account is waiting on (#103), and how to add a machine (#98). */
export function NoMachines({ s }: { s: ControlSession }) {
  const items = accountItems(s).filter((i): i is Extract<MenuItem, { label: string }> => typeof i === "object" && "label" in i);
  return (
    <div class="control-page">
      <header class="control-account" data-account-bar>
        <span class="control-who">
          Signed in as <b data-signed-in-as>{s.login}</b>
        </span>
        {items.map((i) => (
          <button key={i.label} class="control-linkish" onClick={i.run}>
            {i.label}
          </button>
        ))}
      </header>
      <main>
        {s.asked.map((a) => (
          <p key={a.team} class="control-asked" data-asked={a.team}>
            Waiting for {a.owners.length ? a.owners.join(" or ") : "an owner"} to add you to <b>{a.name}</b>. Their machines appear here when they do.
          </p>
        ))}
        <h1>{s.asked.length ? "Add your own machine" : "Add a machine"}</h1>
        <AddMachine s={s} />
      </main>
    </div>
  );
}

const INSTALL = "curl -fsSL https://illogical.widgets.wtf/install.sh | sh";

function AddMachine({ s }: { s: ControlSession }) {
  const owns = s.teams.some((t) => t.role === "owner");
  return (
    <div class="control-add">
      <ol class="control-steps">
        <li>
          <b>Install Arugula on the machine</b> (macOS or Linux):
          <CopyText text={INSTALL} data-install />
          <span class="dim">
            Or with <a href="https://illogical.widgets.wtf/#install" target="_blank" rel="noopener">Homebrew, or from source</a>.
          </span>
        </li>
        <li>
          <b>Join it to this account:</b>
          <CopyText text={`~/.local/bin/arugulad join ${s.info.url}`} data-join-cmd />
          {owns ? (
            <span class="dim">
              To make it a team's machine, add <code>--team</code> and the team's id (in Teams…).
            </span>
          ) : null}
        </li>
        <li>
          <b>Approve it here.</b> It prints a link and a code: open the link here, or type the code below. Codes last 15 minutes.
          <JoinCodeForm s={s} />
        </li>
      </ol>
      <p class="dim">
        A <i>machine</i> runs your terminals; a <i>device</i> (this browser, your phone) reaches them.
      </p>
      {s.sandboxesOpen ? <HostedVm s={s} /> : null}
    </div>
  );
}

/** A hosted VM (M20): no machine of your own needed. */
function HostedVm({ s }: { s: ControlSession }) {
  useControl(s);
  const [err, setErr] = useState("");
  const starting = s.starting ? s.sandboxes.find((x) => x.id === s.starting) : undefined;
  return (
    <div class="hosted-vm">
      <p>No machine to hand? Use a hosted VM: a fresh Linux machine, deleted when you close its last tab.</p>
      <button class="primary" data-start-vm disabled={!!starting && !starting.state.startsWith("failed")} onClick={() => s.startSandbox().catch((e: Error) => setErr(e.message))}>
        {starting && !starting.state.startsWith("failed") ? `Starting (${starting.state})…` : "Start a hosted VM"}
      </button>
      {starting?.state.startsWith("failed") ? <p class="control-error">{starting.state}</p> : null}
      {err ? <p class="control-error">{err}</p> : null}
    </div>
  );
}

function JoinCodeForm({ s }: { s: ControlSession }) {
  const [code, setCode] = useState("");
  return (
    <form
      class="control-code"
      onSubmit={(e) => {
        e.preventDefault();
        location.hash = `join=${code.trim()}`;
      }}
    >
      <input placeholder="XXXXX-XXXXX" value={code} onInput={(e) => setCode((e.target as HTMLInputElement).value)} aria-label="Join code" />
      <button type="submit" disabled={!code.trim() || s.phase !== "ready"}>
        Look up
      </button>
    </form>
  );
}

type Panel = "devices" | "add" | "add-device" | "teams" | "plan" | "account";

const openPanel = (p: Panel) => () => dispatchEvent(new CustomEvent("arugula:control-panel", { detail: p }));

/** Over the app: approval prompts, a daemon's join, the device list. */
export function ControlOverlay({ s }: { s: ControlSession }) {
  useControl(s);
  const [hash, setHash] = useState(location.hash);
  const [panel, setPanel] = useState<null | Panel>(null);
  useEffect(() => {
    const on = () => setHash(location.hash);
    const open = (e: Event) => setPanel((e as CustomEvent<Panel>).detail);
    addEventListener("hashchange", on);
    addEventListener("arugula:control-panel", open);
    // Says the panel event has a listener, so a test can wait for it
    // rather than send one into nothing.
    document.documentElement.dataset.controlPanels = "";
    // Mounted afresh (the page switches machine when one is approved, and
    // that happens before the prompt clears the hash): catch up with a
    // hashchange that fired before this listener was there.
    on();
    return () => {
      removeEventListener("hashchange", on);
      removeEventListener("arugula:control-panel", open);
      delete document.documentElement.dataset.controlPanels;
    };
  }, []);
  if (s.phase !== "ready") return null;
  if (s.recoveryCodes) return <RecoveryCodes s={s} />;
  const join = /^#join=([A-Za-z0-9-]+)$/.exec(hash)?.[1];
  if (join) return <JoinPrompt s={s} code={join} />;
  // The app's device asking to be approved comes first: it's the next step
  // after allowing its sign-in.
  const appLogin = /^#app=([0-9a-f]{16,128})$/.exec(hash)?.[1];
  if (appLogin && !s.pending[0]) return <AppLoginPrompt s={s} id={appLogin} />;
  if (hash === "#app-done" && !s.pending[0]) return <AppLoginDone />;
  const invite = inviteInHash(hash) ?? inviteInHash(usedLink);
  if (invite)
    return invite.presigned ? (
      <PresignedPrompt s={s} team={invite.team} seed={invite.code} />
    ) : (
      <InvitePrompt s={s} team={invite.team} code={invite.code} />
    );
  // Someone used an invite to a team I own: add them (sign the roster)?
  const req = s.teams.flatMap((t) => (t.role === "owner" ? t.requests.map((r) => ({ t, r })) : []))[0];
  if (req) return <AdmitPrompt s={s} team={req.t} req={req.r} />;
  const asking = s.pending[0];
  // #326: a device that came to approve a machine's join: both together.
  const asked = asking && s.pendingJoins.get(asking.device);
  if (asking && asked) return <JoinPrompt key={asked} s={s} code={asked} from={asking} />;
  if (asking) return <DevicePrompt s={s} c={asking} />;
  const offer = s.offers[0];
  if (offer) return <ShareOfferPrompt s={s} o={offer} />;
  if (s.joined) return <Joined s={s} />;
  const notice = s.notices[0];
  if (notice) return <Notice s={s} n={notice} />;
  if (panel === "devices") return <Devices s={s} close={() => setPanel(null)} />;
  if (panel === "teams") return <Teams s={s} close={() => setPanel(null)} />;
  if (panel === "plan") return <Plan s={s} close={() => setPanel(null)} />;
  if (panel === "account")
    return (
      <Modal close={() => setPanel(null)}>
        <AccountPanel s={s} close={() => setPanel(null)} />
      </Modal>
    );
  if (panel === "add-device") return <AddDevice s={s} close={() => setPanel(null)} />;
  if (panel === "add")
    return (
      <Modal close={() => setPanel(null)}>
        <h2>Add a machine</h2>
        <AddMachine s={s} />
      </Modal>
    );
  return null;
}

function Modal({ children, close }: { children: preact.ComponentChildren; close?: () => void }) {
  return (
    <div class="prompt-backdrop" onClick={(e) => e.target === e.currentTarget && close?.()}>
      <div class="prompt control-prompt">{children}</div>
    </div>
  );
}

function clearHash() {
  usedLink = "";
  history.replaceState(null, "", location.pathname + location.search);
  dispatchEvent(new HashChangeEvent("hashchange"));
}

/** An invite link that was used: out of the address bar, so a reload
 * doesn't offer it again (#208), but its prompt stays up until closed,
 * through the overlay mounting afresh. */
let usedLink = "";

function dropHash() {
  if (location.hash) usedLink = location.hash;
  history.replaceState(null, "", location.pathname + location.search);
}

/** A machine's join, by its code: from its approval link, typed in, or
 * (#326) brought by a waiting device that came to approve it (`from`).
 * Devices waiting with this code are listed alongside, and one Approve
 * covers the machine and them. */
function JoinPrompt({ s, code, from }: { s: ControlSession; code: string; from?: Cert }) {
  const [j, setJ] = useState<JoinRequest | null>(null);
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);
  // Control refused an approval from here (#327): closing then isn't
  // turning the machine down. It keeps waiting for one that works.
  const [refused, setRefused] = useState(false);
  // "" is just me; else a team's id.
  const [to, setTo] = useState("");
  // Devices alongside that the person unticked.
  const [skip, setSkip] = useState<Set<string>>(new Set());
  useEffect(() => {
    // This browser's own trust, fresh, before it offers Approve (#327).
    void s.refresh();
    s.showJoin(code).then(
      (j) => {
        setJ(j);
        setTo(j.team?.team ?? "");
      },
      (e: Error) => setErr(e.message),
    );
  }, [code]);
  // The machine's code is gone (approved elsewhere, expired): the device
  // that brought it asks on its own.
  if (from && err && !j) return <DevicePrompt s={s} c={from} />;
  // This browser waited with this code (#326), and it's gone: most likely
  // approved with this browser, on the device that approved both.
  if (!from && err && !j && s.brought === normalizeCode(code))
    return (
      <Modal close={clearHash}>
        <h2>Add a machine?</h2>
        <p data-join-done={code}>
          Code <b>{s.brought}</b> isn't waiting any more: the device that approved this browser approved the machine with it, or the code expired. The
          machine says which.
        </p>
        <div class="prompt-buttons">
          <button class="primary" onClick={clearHash}>
            Done
          </button>
        </div>
      </Modal>
    );
  // Teams I'm in (#332), and the one it asked for even if I can't add to it.
  const teams = s.addableTeams();
  const asked = j?.team && !teams.some((t) => t.team === j.team!.team) ? j.team : null;
  const team = teams.find((t) => t.team === to);
  const cant = !!to && !team;
  const locked = !!asked && !!s.teams.find((t) => t.team === asked.team)?.locked;
  // Devices waiting with this code (#326), approved with the machine
  // unless unticked.
  const alongside = j ? s.pending.filter((c) => s.pendingJoins.get(c.device) === j.code) : [];
  const also = alongside.filter((c) => !skip.has(c.device));
  const cancel = () => {
    if (j && !refused) void s.rejectJoin(j.code).then(() => s.refresh(), () => {});
    clearHash();
  };
  const failed = (e: unknown) => {
    setErr((e as Error).message);
    if (e instanceof RefusedError) setRefused(true);
    setBusy(false);
  };
  // M49: the Arugula CLI on a machine, asking to be one of your devices.
  if (j?.cert.kind === "cli") return <CliJoin s={s} j={j} cancel={cancel} refused={refused} failed={failed} />;
  const machine = j ? (
    <>
      <p>
        <b>{j.cert.name}</b> asks to join {j.team ? <>the team <b data-join-team={j.team.team}>{j.team.name}</b></> : "your account"} with code{" "}
        <b data-join-code={j.code}>{j.code}</b>. Check it's the code the machine shows (in Getting started, or where you ran <code>arugulad join</code>).
      </p>
      <p class="dim">Its key: {fingerprint(j.cert.device)}</p>
    </>
  ) : null;
  return (
    <Modal close={from ? undefined : clearHash}>
      <h2>{alongside.length ? "Add a machine and a device?" : "Add a machine?"}</h2>
      {err ? (
        <p class="control-error" data-join-error>
          {err}
        </p>
      ) : null}
      {j ? (
        <>
          {alongside.length ? (
            <div class="control-both" data-join-both>
              <div class="control-both-card" data-join-machine>
                <div class="control-both-kind">{j.cert.name} (machine)</div>
                {machine}
              </div>
              {alongside.map((c) => (
                <div key={c.device} class="control-both-card" data-join-alongside={c.device}>
                  <div class="control-both-kind">{c.name} (browser)</div>
                  <p>Signed in to come here and approve this machine. Approved too, it reaches your machines. It shows this fingerprint:</p>
                  <p class="fingerprint" data-pending={c.device}>
                    {fingerprint(c.device)}
                  </p>
                  <label class="control-check">
                    <input
                      type="checkbox"
                      data-join-also={c.device}
                      checked={!skip.has(c.device)}
                      onChange={(e) => {
                        const next = new Set(skip);
                        if ((e.target as HTMLInputElement).checked) next.delete(c.device);
                        else next.add(c.device);
                        setSkip(next);
                      }}
                    />{" "}
                    Approve it too
                  </label>
                </div>
              ))}
            </div>
          ) : (
            machine
          )}
          {s.enrollment ? (
            <p>
              Your account:{" "}
              <b class="fingerprint" data-join-account={s.enrollment.root}>
                {fingerprint(s.enrollment.root)}
              </b>
              . Once you approve, the machine shows its account's fingerprint: check it's this one there.
            </p>
          ) : null}
          {teams.length || asked ? (
            <p>
              <label>
                Join to{" "}
                <select class="control-select" data-join-to value={to} onChange={(e) => setTo((e.target as HTMLSelectElement).value)}>
                  <option value="">Just me</option>
                  {teams.map((t) => (
                    <option key={t.team} value={t.team}>
                      {t.roster.name}
                    </option>
                  ))}
                  {asked ? (
                    <option value={asked.team}>
                      {asked.name} ({locked ? "locked" : "you're not in it"})
                    </option>
                  ) : null}
                </select>
              </label>
            </p>
          ) : null}
          {cant ? (
            <p class="control-error" data-join-not-member>
              {locked
                ? `${asked!.name} is locked: only its owners add machines to it. Ask one of them to approve it, or pick Just me.`
                : `Only ${asked!.name}'s members add machines to it. Ask one of them to approve it, or pick Just me.`}
            </p>
          ) : (
            <p class="dim" data-join-grants>
              {team
                ? `Everyone in ${team.roster.name} sees it and reaches it by their role: owners and editors drive its terminals, viewers watch. Its owners also see private panes, and can take it out of the team. It stays yours.`
                : "Only your devices reach it, and they can drive its terminals."}{" "}
              Control relays the connection but can't read it.
            </p>
          )}
        </>
      ) : err ? null : (
        <p class="dim">Looking up {code}…</p>
      )}
      <div class="prompt-buttons">
        <button data-cancel-join onClick={cancel}>
          {refused ? "Close" : "Cancel"}
        </button>
        <button
          class="primary"
          data-approve-join
          disabled={!j || busy || cant}
          onClick={async () => {
            if (!j) return;
            setBusy(true);
            try {
              // The machine, then the devices that came with it.
              await s.approveJoin(j.code, j.cert, team?.team ?? null, also);
              clearHash();
            } catch (e) {
              // The machine is in: only the device is left to ask again.
              if (e instanceof AlongsideError) setJ(null);
              failed(e);
            }
          }}
        >
          {also.length > 1 ? "Approve them all" : also.length ? "Approve both" : "Approve"}
        </button>
      </div>
    </Modal>
  );
}

/** M49: the Arugula CLI on some machine asks to be one of this account's
 * devices (`arugula login`). Approved, it reaches the account's machines
 * and can approve devices and machines, as this browser can. */
function CliJoin({ s, j, cancel, refused, failed }: { s: ControlSession; j: JoinRequest; cancel: () => void; refused: boolean; failed: (e: unknown) => void }) {
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);
  return (
    <Modal close={clearHash}>
      <h2>Add a terminal?</h2>
      {err ? (
        <p class="control-error" data-join-error>
          {err}
        </p>
      ) : null}
      <p data-join-cli={j.cert.name}>
        The Arugula command line on <b>{j.cert.name}</b> asks to be one of your devices, with code <b data-join-code={j.code}>{j.code}</b>. Check it's the code
        it shows where you ran <code>arugula login</code>.
      </p>
      <p class="dim">Its key: {fingerprint(j.cert.device)}</p>
      {s.enrollment ? (
        <p>
          Your account:{" "}
          <b class="fingerprint" data-join-account={s.enrollment.root}>
            {fingerprint(s.enrollment.root)}
          </b>
          . Once you approve, it shows its account's fingerprint: check it's this one there.
        </p>
      ) : null}
      <p class="dim">It reaches your machines (directly or through control's relay, end to end encrypted) and can approve devices, as this one can.</p>
      <div class="prompt-buttons">
        <button data-cancel-join onClick={cancel}>
          {refused ? "Close" : "Cancel"}
        </button>
        <button
          class="primary"
          data-approve-join
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            try {
              await s.approveJoin(j.code, j.cert);
              clearHash();
            } catch (e) {
              setErr((e as Error).message);
              setBusy(false);
              failed(e);
            }
          }}
        >
          Approve
        </button>
      </div>
    </Modal>
  );
}

/** M48: the desktop app asks to sign in as this account. Its device key
 * is approved separately afterwards (the "New device?" prompt), so this
 * only lets it ask. Allowing hands a grant to the app on this computer
 * (its loopback port): an app elsewhere that sent this link gets nothing. */
function AppLoginPrompt({ s, id }: { s: ControlSession; id: string }) {
  const [a, setA] = useState<{ name: string; code: string; allowed: boolean; from: string; same_network: boolean } | null>(null);
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    s.showAppLogin(id).then(setA, (e: Error) => setErr(e.message));
  }, [id]);
  return (
    <Modal close={clearHash}>
      <h2>Sign in the app?</h2>
      {err ? <p class="control-error" data-app-login-error>{err}</p> : null}
      {a?.allowed ? (
        <p data-app-login-error>This sign-in was already allowed. Start it again in the app if it didn't finish.</p>
      ) : a ? (
        <>
          <p>
            <b data-app-login-name>{a.name}</b> asks to sign in as you. Check the app shows <b data-app-login-code={a.code}>{a.code}</b>.
          </p>
          {a.same_network ? null : (
            <p class="control-error" data-app-login-elsewhere>
              It asked from another network ({a.from}) than this browser's. If the app isn't on this computer, cancel.
            </p>
          )}
          <p class="dim">
            Only the app on this computer can finish it. If you didn't just press Sign in in the Arugula app, cancel: someone may have sent you
            this link.
          </p>
          <div class="prompt-buttons">
            <button onClick={clearHash}>Cancel</button>
            <button
              class="primary"
              data-app-login-allow
              disabled={busy}
              onClick={async () => {
                setBusy(true);
                try {
                  // To the app's loopback port; it sends this page back.
                  location.href = await s.allowAppLogin(id);
                } catch (e) {
                  setErr((e as Error).message);
                  setBusy(false);
                }
              }}
            >
              Allow
            </button>
          </div>
        </>
      ) : err ? null : (
        <p class="dim">Looking it up…</p>
      )}
    </Modal>
  );
}

/** Back from handing the app its grant. */
function AppLoginDone() {
  useEffect(() => {
    // Out of the way of the device prompt that follows.
    const t = setTimeout(clearHash, 6000);
    return () => clearTimeout(t);
  }, []);
  return (
    <Modal close={clearHash}>
      <h2>Sign in the app?</h2>
      <p data-app-login-done>
        Signed in. Next the app's window asks to be approved as a device, so it reaches your machines: the prompt shows here in a moment. Its own
        machine joins separately, with a code, and doesn't need this.
      </p>
      <div class="prompt-buttons">
        <button class="primary" onClick={clearHash}>
          Done
        </button>
      </div>
    </Modal>
  );
}

/** Someone shares a session on their machine with this account: it's
 * listed (and its notifications reach here) only once accepted. */
function ShareOfferPrompt({ s, o }: { s: ControlSession; o: ShareOffer }) {
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);
  const who = o.owner_name || o.owner_login || "Someone";
  const answer = (accept: boolean) => async () => {
    setBusy(true);
    try {
      await s.answerShare(o.daemon, accept);
    } catch (e) {
      setErr((e as Error).message);
    }
    setBusy(false);
  };
  return (
    <Modal>
      <h2>A shared session</h2>
      <p data-share-offer={o.daemon}>
        <b data-share-offer-owner>{who}</b>
        {o.owner_login && o.owner_login !== who ? ` (${o.owner_login})` : ""} wants to share a session on their machine{" "}
        <b data-share-offer-machine>{o.name}</b> with you.
      </p>
      <p class="dim">Accept only if you know them. Accepting lists the machine here and lets its notifications reach you.</p>
      {err ? <p class="control-error">{err}</p> : null}
      <div class="prompt-buttons">
        <button data-share-decline disabled={busy} onClick={answer(false)}>
          Decline
        </button>
        <button class="primary" data-share-accept disabled={busy} onClick={answer(true)}>
          Accept
        </button>
      </div>
    </Modal>
  );
}

function DevicePrompt({ s, c }: { s: ControlSession; c: Cert }) {
  const [err, setErr] = useState("");
  const act = (f: () => Promise<void>) => f().catch((e: Error) => setErr(e.message));
  return (
    <Modal>
      <h2>New device?</h2>
      <p>
        <b>{c.name}</b> wants to reach your machines. Approve it only if it's yours and shows this fingerprint:
      </p>
      <p class="fingerprint" data-pending={c.device}>
        {fingerprint(c.device)}
      </p>
      {err ? <p class="control-error">{err}</p> : null}
      <p class="dim">If you didn't just sign in on it, turn it down.</p>
      <div class="prompt-buttons">
        <button data-reject onClick={() => act(() => s.reject(c))}>
          Turn down
        </button>
        <button class="primary" data-approve onClick={() => act(() => s.approve(c))}>
          Approve
        </button>
      </div>
    </Modal>
  );
}

/** The control URL as a QR code, a link to copy, and what to do (#105).
 * When the new device asks, DevicePrompt comes up over this. */
function AddDevice({ s, close }: { s: ControlSession; close: () => void }) {
  const code = qrPath(qr(s.info.url));
  return (
    <Modal close={close}>
      <h2>Add a phone or browser</h2>
      <svg class="control-qr" viewBox={`0 0 ${code.n} ${code.n}`} role="img" aria-label={`QR code for ${s.info.url}`} data-qr={s.info.url}>
        <rect width={code.n} height={code.n} fill="#fff" />
        <path d={code.d} fill="#000" />
      </svg>
      <CopyText text={s.info.url} share data-control-url />
      <ol class="control-steps">
        <li>Open it on the phone or browser (scan the code, or send the link).</li>
        <li>Sign in the way you did here{s.login && s.login !== "you" ? `, as ${s.login}` : ""}.</li>
        <li>Approve it here: this browser asks. Check the fingerprints match.</li>
      </ol>
      <div class="prompt-buttons">
        <button onClick={close}>Done</button>
      </div>
    </Modal>
  );
}

/** For a GitHub-only account: a passkey, so it isn't tied to GitHub. */
function PasskeyNudge({ s }: { s: ControlSession }) {
  const [err, setErr] = useState("");
  if (!s.info.passkeys || s.passkeys > 0) return null;
  return (
    <p class="dim" data-passkey-nudge>
      Add a passkey so this account isn't tied to GitHub.{" "}
      <button class="control-linkish" data-add-passkey onClick={() => s.addPasskey().catch((e: Error) => setErr(e.name === "NotAllowedError" ? "Cancelled." : e.message))}>
        Add a passkey
      </button>
      {err ? <span class="control-error"> {err}</span> : null}
    </p>
  );
}

const since = (ms: number) => {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  return s < 60 ? "just now" : s < 3600 ? `${Math.round(s / 60)}m ago` : s < 86400 ? `${Math.round(s / 3600)}h ago` : `${Math.round(s / 86400)}d ago`;
};

/** *Move to…* on a machine (#100): into a team you're in (#332), or back
 * to just you. Only shown when there's somewhere to move it. */
function MoveMachine({ s, c, team, online }: { s: ControlSession; c: Cert; team: string | null; online: boolean }) {
  const [to, setTo] = useState<string | null | undefined>(undefined);
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);
  const teams = s.addableTeams();
  if (teams.length === 0 && !team) return null;
  const name = (id: string | null) => (id ? s.teams.find((t) => t.team === id)?.roster.name ?? "a team" : "just you");
  return (
    <div class="control-move" data-move={c.device}>
      <label class="dim">
        In{" "}
        <select
          class="control-select"
          data-move-to
          value={to === undefined ? (team ?? "") : (to ?? "")}
          onChange={(e) => {
            const v = (e.target as HTMLSelectElement).value || null;
            setErr("");
            setTo(v === team ? undefined : v);
          }}
        >
          <option value="">Just me</option>
          {teams.map((t) => (
            <option key={t.team} value={t.team}>
              {t.roster.name}
            </option>
          ))}
          {team && !teams.some((t) => t.team === team) ? <option value={team}>{name(team)}</option> : null}
        </select>
      </label>
      {to !== undefined ? (
        <p class="dim control-explain" data-move-explain>
          {to
            ? `Everyone in ${name(to)} sees ${c.name} and reaches it by their role${team ? `, and ${name(team)}'s members lose it` : ""}; its owners also see private panes. It stays yours, and you can take it out again.`
            : `${name(team)}'s members lose ${c.name} at once; only your devices reach it.`}
          {online ? " " : " It's offline, so it moves when it next connects. "}
          <button
            class="primary"
            data-move-go
            disabled={busy}
            onClick={() => {
              setBusy(true);
              s.moveDaemon(c.device, to).then(
                () => {
                  setBusy(false);
                  setTo(undefined);
                },
                (e: Error) => {
                  setBusy(false);
                  setErr(e.message);
                },
              );
            }}
          >
            Move
          </button>{" "}
          <button data-move-cancel onClick={() => setTo(undefined)}>
            Cancel
          </button>
        </p>
      ) : null}
      {err ? <p class="control-error">{err}</p> : null}
    </div>
  );
}

/** When a device was added and which device approved it (#327). */
function Approved({ s, c }: { s: ControlSession; c: Cert }) {
  if (c.approver === c.device) return <span class="dim"> · added {since(c.created)}, the first device</span>;
  const by = s.trusted.get(c.approver);
  const on = !by ? "on a device since removed" : by.kind === "recovery" ? `with ${by.name}` : `on ${by.name}${by.device === s.keys.id ? " (this browser)" : ""}`;
  return (
    <span class="dim" data-approved-by={c.approver}>
      {" "}
      · added {since(c.created)}, approved {on}
    </span>
  );
}

function Devices({ s, close }: { s: ControlSession; close: () => void }) {
  const [err, setErr] = useState("");
  // The machine or device whose Remove was clicked: a dialog asks.
  const [removing, setRemoving] = useState<Cert | null>(null);
  const [renewing, setRenewing] = useState(false);
  const devices = [...s.trusted.values()].filter((c) => c.kind !== "recovery");
  const machines = devices.filter((c) => c.kind === "daemon");
  const browsers = devices.filter((c) => c.kind !== "daemon");
  const remove = (c: Cert) => (
    <button class="control-revoke" data-remove={c.device} onClick={() => setRemoving(c)}>
      Remove
    </button>
  );
  const left = s.recoveryLeft;
  return (
    <Modal close={close}>
      <h2>Devices and machines</h2>
      <p class="dim">
        Signed in as <b data-account={s.account}>{s.login}</b>.
      </p>
      {s.enrollment ? (
        <p class="dim">
          Your account's fingerprint:{" "}
          <span class="fingerprint-inline" data-account-fingerprint={s.enrollment.root}>
            {fingerprint(s.enrollment.root)}
          </span>
          . A machine shows it when it joins; check they match.
        </p>
      ) : null}
      {s.rootMismatch ? (
        <p class="control-error">Control reports a different first device for this account than this browser pinned. New devices and machines won't be trusted here.</p>
      ) : null}
      <h3 class="control-group">Machines</h3>
      <p class="dim">They run your terminals.</p>
      {machines.length === 0 ? <p class="dim">None yet.</p> : null}
      <ul class="control-devices" data-machines>
        {machines.map((c) => {
          const d = s.daemons.find((x) => x.id === c.device);
          const team = d?.team ? s.teams.find((t) => t.team === d.team)?.roster.name ?? "a team" : null;
          const path = d?.name === directory.current && directory.path ? directory.path : d?.urls.length ? "direct" : "relayed";
          return (
            <li key={c.device} data-device={c.device}>
              <span>
                {c.name}
                {team ? (
                  <span class="control-badge" data-team-badge>
                    {team}
                  </span>
                ) : null}
                <span class="dim" data-status>
                  {" · "}
                  {d?.online ? "online" : d?.last_seen ? `seen ${since(d.last_seen)}` : "offline"}
                  {d?.online ? ` · ${path}` : ""}
                </span>
                <Approved s={s} c={c} />
              </span>
              <span class="dim">{fingerprint(c.device)}</span>
              {remove(c)}
              <MoveMachine s={s} c={c} team={d?.team ?? null} online={!!d?.online} />
            </li>
          );
        })}
      </ul>
      <button data-add-machine onClick={openPanel("add")}>
        Add a machine…
      </button>
      <h3 class="control-group">Browsers and phones</h3>
      <p class="dim">They reach your machines.</p>
      <ul class="control-devices" data-browsers>
        {browsers.map((c) => (
          <li key={c.device} data-device={c.device}>
            <span>
              {c.name}
              {c.device === s.keys.id ? <b data-this-browser> (this browser)</b> : ""}
              <Approved s={s} c={c} />
            </span>
            <span class="dim">{fingerprint(c.device)}</span>
            {c.device !== s.keys.id ? remove(c) : <span />}
          </li>
        ))}
      </ul>
      <button data-add-device onClick={openPanel("add-device")}>
        Add a phone or browser…
      </button>
      <h3 class="control-group">Recovery codes</h3>
      <p class="dim">
        <span data-recovery-left={left}>
          {left === 0 ? "No recovery codes left." : `${left} recovery code${left === 1 ? "" : "s"} left.`}
        </span>{" "}
        {renewing ? "New codes replace these: the old ones stop working. " : ""}
        <button
          class={renewing ? "control-linkish danger" : "control-linkish"}
          data-new-codes
          onClick={() => {
            if (!renewing) return setRenewing(true);
            setRenewing(false);
            s.newRecoveryCodes().catch((e: Error) => setErr(e.message));
          }}
        >
          {renewing ? "Make new ones" : "Make new codes"}
        </button>
      </p>
      <NameLine s={s} />
      {s.info.passkeys && s.passkeys ? (
        <p class="dim">
          {`${s.passkeys} passkey${s.passkeys === 1 ? "" : "s"} can sign in to this account. `}
          <button class="control-linkish" data-add-passkey onClick={() => s.addPasskey().catch((e: Error) => setErr(e.message))}>
            Add one
          </button>
        </p>
      ) : (
        <PasskeyNudge s={s} />
      )}
      {err ? <p class="control-error">{err}</p> : null}
      {removing ? (
        <ConfirmRemove
          title={`Remove ${removing.name}?`}
          cancel={() => setRemoving(null)}
          go={() => {
            setRemoving(null);
            s.revoke(removing.device).catch((e: Error) => setErr(e.message));
          }}
        >
          {removing.kind === "daemon" ? (
            <p data-remove-explain>
              It loses access at once: it's taken off your account. Arugula keeps running on it, reachable only locally. To add it back, join it again with{" "}
              <code>arugulad join</code>, which makes a new key.
            </p>
          ) : (
            <p data-remove-explain>It loses access to your machines at once. To use it again, add it as a new device: it gets a new key.</p>
          )}
          <p class="fingerprint">{fingerprint(removing.device)}</p>
        </ConfirmRemove>
      ) : null}
      <div class="prompt-buttons">
        <button onClick={() => s.signOut(false)}>Sign out</button>
        <button onClick={close}>Done</button>
      </div>
    </Modal>
  );
}

const panel = openPanel;

/** For the host menu. */
export function controlMenuItems(s: ControlSession): MenuItem[] {
  const shown = s.daemons.find((d) => d.name === directory.current);
  return [
    "separator",
    { header: `${s.login}${s.stale ? " · control unreachable" : ""}` },
    ...(s.sandboxesOpen ? [{ label: "New hosted VM", run: () => void s.startSandbox() }] : []),
    ...(shown?.sandbox ? [{ label: "Delete this VM", run: () => void s.deleteSandbox(shown.sandbox!) }] : []),
    { label: "Add a machine…", run: panel("add") },
    { label: "Add a phone or browser…", run: openPanel("add-device") },
    ...accountItems(s),
  ];
}

/** The account's own items: in the host menu, and on the no-machines
 * screen's header (#97). */
function accountItems(s: ControlSession): MenuItem[] {
  return [
    { label: "Teams…", run: panel("teams") },
    { label: "Devices and machines…", run: panel("devices") },
    { label: "Sign-in and account…", run: panel("account") },
    ...(s.billing?.billing ? [{ label: s.billing.relay.warning ? "Plan and usage… (over the free relay)" : "Plan and usage…", run: panel("plan") }] : []),
    { label: "Sign out", run: () => void s.signOut(false) },
  ];
}

const mb = (b: number) => `${(b / 1e6).toFixed(b < 1e7 ? 1 : 0)} MB`;

function Plan({ s, close }: { s: ControlSession; close: () => void }) {
  const b = s.billing;
  const [err, setErr] = useState("");
  if (!b) return null;
  return (
    <Modal close={close}>
      <h2>Plan and usage</h2>
      <p>
        You're on <b>{b.plan === "paid" ? "a paid plan" : "the free plan"}</b>. This month: {mb(b.relay.bytes)} through the relay
        {b.plan === "paid" ? "" : ` of ${mb(b.relay.allowance)} free`}, {b.sandbox_minutes} hosted VM minutes.
      </p>
      {b.relay.warning ? (
        <p class="control-error" data-relay-warning>
          You're over the free relay allowance{b.relay.slowed ? ", so relayed traffic is slowed down" : ""}. Direct connections (your tailnet, your LAN) don't count. A plan lifts it.
        </p>
      ) : null}
      {b.teams.map((t) => (
        <p key={t.team} data-team-plan={t.team}>
          <b>{t.name}</b>: {t.plan === "team" ? `team plan, ${t.seats} seats` : "free"}; {t.sandbox_minutes} VM minutes this month.{" "}
          {t.owner && t.plan !== "team" ? (
            <button class="control-linkish" onClick={() => s.upgrade(t.team).catch((e: Error) => setErr(e.message))}>
              Upgrade ({t.seats} seat{t.seats === 1 ? "" : "s"})
            </button>
          ) : null}
        </p>
      ))}
      {b.plan !== "paid" ? (
        <p>
          <button class="control-linkish" onClick={() => s.upgrade().catch((e: Error) => setErr(e.message))}>
            Add a payment method for hosted VMs (by the minute)
          </button>
        </p>
      ) : null}
      {err ? <p class="control-error">{err}</p> : null}
      <div class="prompt-buttons">
        <button onClick={close}>Done</button>
      </div>
    </Modal>
  );
}

/** An owner said yes to this account's request (#103). */
function Joined({ s }: { s: ControlSession }) {
  const j = s.joined!;
  const has = s.daemons.some((d) => d.team === j.team);
  return (
    <Modal close={() => s.sawJoined()}>
      <h2 data-joined={j.team}>You're in {j.name}</h2>
      <p>{has ? "Its machines are in your host list now." : "Its machines appear here when an owner adds one."}</p>
      <div class="prompt-buttons">
        <button class="primary" onClick={() => s.sawJoined()}>
          OK
        </button>
      </div>
    </Modal>
  );
}

/** Something control kept to tell this account (#206: a team it was in
 * was deleted while this page wasn't open, or was). */
function Notice({ s, n }: { s: ControlSession; n: ControlSession["notices"][number] }) {
  return (
    <Modal close={() => void s.sawNotice(n.id)}>
      <h2 data-notice={n.id}>{n.title}</h2>
      <p>{n.body}</p>
      <div class="prompt-buttons">
        <button class="primary" onClick={() => void s.sawNotice(n.id)}>
          OK
        </button>
      </div>
    </Modal>
  );
}

function InvitePrompt({ s, team, code }: { s: ControlSession; team: string; code: string }) {
  const [info, setInfo] = useState<{ name: string; role: TeamRole } | null>(null);
  const [err, setErr] = useState("");
  const [done, setDone] = useState(false);
  useEffect(() => {
    s.showInvite(team, code).then(setInfo, (e: Error) => setErr(e.message));
  }, [team, code]);
  return (
    <Modal close={clearHash}>
      <h2>Join a team?</h2>
      {err ? <p class="control-error">{err}</p> : null}
      {info && !done ? (
        <p>
          You're invited to <b data-invite-team={team}>{info.name}</b> as {roleAs(info.role)}.
        </p>
      ) : null}
      {done ? <p data-invite-pending>Asked to join. An owner adds you when they're next here; their machines appear once they do.</p> : null}
      <div class="prompt-buttons">
        <button onClick={clearHash}>{done ? "Done" : "Not now"}</button>
        {!done ? (
          <button
            class="primary"
            data-accept-invite
            disabled={!info}
            onClick={() =>
              s.acceptInvite(team, code).then(
                () => {
                  dropHash();
                  setDone(true);
                },
                (e: Error) => setErr(e.message),
              )
            }
          >
            Join
          </button>
        ) : null}
      </div>
    </Modal>
  );
}

/** A presigned invite: accepting it is the whole of joining. */
function PresignedPrompt({ s, team, seed }: { s: ControlSession; team: string; seed: string }) {
  const [info, setInfo] = useState<{ name: string; role: TeamRole } | null>(null);
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);
  // In it, whether by this click or before (the page may redraw this
  // prompt as the team's machines arrive).
  const member = s.teams.find((t) => t.team === team && t.role);
  useEffect(() => {
    if (member) return dropHash();
    s.showPresigned(team, seed).then(
      (p) => setInfo({ name: p.name, role: p.invite.role }),
      (e: Error) => setErr(e.message),
    );
  }, [team, seed]);
  const join = () => {
    setBusy(true);
    s.redeem(team, seed)
      .then(dropHash)
      .catch((e: Error) => setErr(e.message))
      .finally(() => setBusy(false));
  };
  return (
    <Modal close={clearHash}>
      <h2>{member ? `You're in ${member.roster.name}` : `Join ${info?.name ?? "a team"}?`}</h2>
      {err ? <p class="control-error">{err}</p> : null}
      {member ? (
        <p data-invite-joined>Its machines appear in the host menu.</p>
      ) : info ? (
        <p>
          You're invited to <b data-invite-team={team}>{info.name}</b> {roleAs(info.role)}. Joining adds you right away; its owners are told.
        </p>
      ) : null}
      <div class="prompt-buttons">
        <button onClick={clearHash}>{member ? "Done" : "Not now"}</button>
        {!member ? (
          <button class="primary" data-accept-invite disabled={!info || busy} onClick={join}>
            {busy ? "Joining…" : `Join ${info?.name ?? ""}`.trim()}
          </button>
        ) : null}
      </div>
    </Modal>
  );
}

function AdmitPrompt({ s, team, req }: { s: ControlSession; team: Team; req: Team["requests"][number] }) {
  const [err, setErr] = useState("");
  return (
    <Modal>
      <h2>Add to {team.roster.name}?</h2>
      <p>
        <b>{req.name}</b> used an invite, {roleAs(req.role)}. Their account's first device:
      </p>
      <p class="fingerprint" data-admit={req.account}>
        {fingerprint(req.root)}
      </p>
      <p class="dim">If you can, check it with them. Adding them signs the team's new member list on this device.</p>
      {err ? <p class="control-error">{err}</p> : null}
      <div class="prompt-buttons">
        <button onClick={() => s.rejectRequest(team.team, req.account).catch((e: Error) => setErr(e.message))}>Turn down</button>
        <button class="primary" data-admit-yes onClick={() => s.admit(team.team, req).catch((e: Error) => setErr(e.message))}>
          Add them
        </button>
      </div>
    </Modal>
  );
}

function Teams({ s, close }: { s: ControlSession; close: () => void }) {
  const [name, setName] = useState("");
  const [err, setErr] = useState("");
  const act = (f: () => Promise<unknown>) => f().catch((e: Error) => setErr(e.message));
  return (
    <Modal close={close}>
      <h2>Teams</h2>
      <p class="dim" data-teams-intro>
        A team shares its machines with its members. Each member:
      </p>
      <ul class="control-roles">
        {ROLE_HELP.map(([r, what]) => (
          <li key={r}>
            <b>{roleLabel(r)}</b>: {what}
          </li>
        ))}
      </ul>
      <p class="dim">
        An invite link lets one person in right away (with Ask me first, an owner says yes to each). Any member can add their own machines to a team; its owners can take them out.{" "}
        <a href="https://github.com/arugula-salad/illogical/blob/main/docs/teams.md" target="_blank" rel="noreferrer">
          More about teams
        </a>
      </p>
      {s.teams.map((t) => (
        <TeamSection key={t.team} s={s} t={t} act={act} />
      ))}
      <form
        class="control-code"
        onSubmit={(e) => {
          e.preventDefault();
          void act(() => s.createTeam(name)).then(() => setName(""));
        }}
      >
        <input placeholder="New team's name" value={name} onInput={(e) => setName((e.target as HTMLInputElement).value)} aria-label="Team name" />
        <button type="submit" disabled={!name.trim()}>
          Make a team
        </button>
      </form>
      {err ? <p class="control-error">{err}</p> : null}
      <div class="prompt-buttons">
        <button onClick={close}>Done</button>
      </div>
    </Modal>
  );
}

function TeamSection({ s, t, act }: { s: ControlSession; t: Team; act: (f: () => Promise<unknown>) => void }) {
  const [role, setRole] = useState<TeamRole>("editor");
  const [link, setLink] = useState<string | null>(null);
  // An invite that waits for an owner's yes (the old way), not presigned.
  const [askFirst, setAskFirst] = useState(false);
  const [linkAsks, setLinkAsks] = useState(false);
  const [linkWhy, setLinkWhy] = useState("");
  // Which button waits for a second click: "lock".
  const [confirming, setConfirming] = useState<string | null>(null);
  // The member whose Remove was clicked: a dialog asks.
  const [removing, setRemoving] = useState<string | null>(null);
  const owner = t.role === "owner";
  // A member's machine an owner is taking out of the team (#332).
  const [takingOut, setTakingOut] = useState<string | null>(null);
  const machines = s.daemons.filter((d) => d.team === t.team);
  // One-click links not used yet (#134), each with Cancel.
  const [unused, setUnused] = useState<PresignedInvite[]>([]);
  const [reload, setReload] = useState(0);
  useEffect(() => {
    if (!owner) return setUnused([]);
    let live = true;
    s.presignedInvites(t.team).then(
      (l) => live && setUnused(l),
      () => live && setUnused([]),
    );
    return () => {
      live = false;
    };
  }, [s, t.team, owner, t.locked, t.roster.version, link, reload]);
  return (
    <section class="team" data-team={t.team}>
      <h3>
        {t.roster.name} {t.locked ? <span class="control-error">· locked</span> : null}
      </h3>
      {t.locked && !owner ? (
        <p class="dim" data-team-add-locked>
          It's locked: only its owners add machines to it until it's unlocked.
        </p>
      ) : (
        <>
          <p class="dim">
            Team id <CopyText inline text={t.team} data-team-id />. Add a machine to it with <i>In …</i> on it in <i>Devices and machines…</i>, or on the machine:
          </p>
          <CopyText text={`arugulad join ${s.info.url} --team ${t.team}`} data-team-join />
        </>
      )}
      <ul class="control-devices">
        {t.roster.members.map((m) => (
          <li key={m.account} data-member={m.account}>
            <span data-member-name>
              {t.names?.[m.account] ?? m.name}
              {m.account === s.account ? " (you)" : ""}
            </span>
            {owner && m.account !== s.account ? (
              <select
                value={m.role}
                aria-label={`${t.names?.[m.account] ?? m.name}'s role`}
                onChange={(e) => {
                  const r = (e.target as HTMLSelectElement).value as TeamRole;
                  act(() => s.changeTeam(t.team, (ms) => ms.map((x) => (x.account === m.account ? { ...x, role: r } : x))));
                }}
              >
                <RoleOptions />
              </select>
            ) : (
              <span class="dim">{roleLabel(m.role)}</span>
            )}
            {owner && m.account !== s.account ? (
              <button class="control-revoke" data-remove-member={m.account} title="They lose access to the team's machines at once" onClick={() => setRemoving(m.account)}>
                Remove
              </button>
            ) : (
              <span />
            )}
          </li>
        ))}
      </ul>
      {removing ? (
        <ConfirmRemove
          title={`Remove ${t.names?.[removing] ?? t.roster.members.find((m) => m.account === removing)?.name ?? "them"} from ${t.roster.name}?`}
          cancel={() => setRemoving(null)}
          go={() => {
            setRemoving(null);
            act(() => s.changeTeam(t.team, (ms) => ms.filter((x) => x.account !== removing)));
          }}
        >
          <p>They lose the team's machines at once. To come back, they need a new invite.</p>
        </ConfirmRemove>
      ) : null}
      {machines.length ? (
        <ul class="control-devices" data-team-machines={t.team}>
          {machines.map((d) => {
            const mine = !d.account || d.account === s.account;
            return (
              <li key={d.id} data-team-machine={d.id}>
                <span>
                  {d.name}
                  <span class="dim">{mine ? " · yours" : ` · ${d.owner_name || "a member"}'s`}</span>
                </span>
                <span class="dim">{d.online ? "online" : "offline"}</span>
                {owner && !mine ? (
                  <button class="control-revoke" data-take-out={d.id} title="Take it out of the team; it stays its owner's" onClick={() => setTakingOut(d.id)}>
                    Take out
                  </button>
                ) : (
                  <span />
                )}
                {takingOut === d.id ? (
                  <p class="dim control-explain" data-take-out-explain>
                    {t.roster.name}'s members lose {d.name} at once. It stays {d.owner_name || "its owner"}'s, and they can add it again.{" "}
                    <button data-take-out-cancel onClick={() => setTakingOut(null)}>
                      Cancel
                    </button>{" "}
                    <button
                      class="danger"
                      data-take-out-go
                      onClick={() => {
                        setTakingOut(null);
                        act(() => s.moveDaemon(d.id, null));
                      }}
                    >
                      Take it out
                    </button>
                  </p>
                ) : null}
              </li>
            );
          })}
        </ul>
      ) : null}
      {owner ? (
        <>
          <div class="control-code control-invite">
            <label>
              Invite{" "}
              <select value={role} aria-label="Invite as" data-invite-role={t.team} onChange={(e) => setRole((e.target as HTMLSelectElement).value as TeamRole)}>
                {ROLE_HELP.map(([r]) => (
                  <option key={r} value={r}>
                    {roleAs(r).replace(/^as /, "")}
                  </option>
                ))}
              </select>
            </label>
            <label class="control-check" title="Each person waits for an owner's yes, as before">
              <input type="checkbox" data-invite-ask-first checked={askFirst || role === "owner"} disabled={role === "owner"} onChange={(e) => setAskFirst((e.target as HTMLInputElement).checked)} />{" "}
              Ask me first
            </label>
            <button
              data-invite={t.team}
              disabled={t.locked}
              onClick={() =>
                act(async () => {
                  const l = await s.makeInvite(t.team, role, askFirst);
                  setLinkAsks(l.asks);
                  setLinkWhy(l.why ?? "");
                  setLink(l.link);
                })
              }
            >
              Make a link
            </button>
          </div>
          {link && linkWhy ? (
            <p class="dim" data-invite-why>
              {linkWhy}, so this link asks you first. Rerun the install command on them to update.
            </p>
          ) : null}
          {link ? (
            <p>
              {linkAsks
                ? "Anyone with this link can ask to join (for a week); you approve each:"
                : "One person can join with this link, within a day, and you're told when they do. Send it only to them:"}
              <CopyText text={link} share data-invite-link />
            </p>
          ) : null}
          {unused.length ? (
            <>
              <p class="dim">One-click links nobody has used yet:</p>
              <ul class="control-devices" data-presigned-list>
                {unused.map((i) => (
                  <li key={i.key} data-presigned={i.key}>
                    <span>
                      {roleAs(i.role).replace(/^as /, "for ")}
                      {i.by === s.account ? "" : `, from ${i.by_name}`}
                    </span>
                    <span class="dim">{expiresIn(i.expires)}</span>
                    <button
                      class="control-revoke"
                      data-cancel-presigned={i.key}
                      title="Nobody can join with this link any more"
                      onClick={() =>
                        act(async () => {
                          await s.cancelPresigned(t.team, i.key);
                          setReload((n) => n + 1);
                        })
                      }
                    >
                      Cancel
                    </button>
                  </li>
                ))}
              </ul>
            </>
          ) : null}
          {confirming === "lock" ? (
            <p class="control-error" data-lock-warning>
              Lock: only owners reach the team's machines; open invites and requests are dropped.
            </p>
          ) : null}
          <div class="prompt-buttons">
            {confirming === "lock" ? <button onClick={() => setConfirming(null)}>Cancel</button> : null}
            <button
              class={t.locked ? "" : "control-revoke danger"}
              data-lock={t.team}
              onClick={() => {
                if (!t.locked && confirming !== "lock") return setConfirming("lock");
                setConfirming(null);
                setLink(null);
                act(() => s.lockTeam(t.team, !t.locked));
              }}
            >
              {t.locked ? "Unlock" : confirming === "lock" ? "Lock it" : "Lock (owners only)"}
            </button>
          </div>
        </>
      ) : null}
    </section>
  );
}

/** How long a link has left, roughly. */
function expiresIn(at: number): string {
  const min = Math.max(1, Math.round((at - Date.now()) / 60_000));
  return min < 90 ? `expires in ${min} min` : `expires in ${Math.round(min / 60)} h`;
}

function RoleOptions() {
  return (
    <>
      {ROLE_HELP.map(([r]) => (
        <option key={r} value={r}>
          {roleLabel(r)}
        </option>
      ))}
    </>
  );
}
