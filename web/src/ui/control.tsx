// The control-mode screens (M17): signing in, waiting for this browser to
// be approved, approving other devices and daemons' join codes, the device
// list, and how to add a machine.

import { useEffect, useState } from "preact/hooks";
import { passkeyRegister, passkeySignIn, previewInvite, type ControlSession, type JoinRequest } from "../control";
import { fingerprint, type Cert } from "../e2e/cert.ts";
import { useSubscribe } from "./hooks";
import { directory } from "../hosts";
import { CopyButton, CopyText, download } from "./copy";
import { ROLE_HELP, roleAs, roleLabel } from "./roles";
import type { MenuItem } from "./menu";
import type { Team } from "../control";
import type { TeamRole } from "../e2e/team.ts";

export function useControl(s: ControlSession) {
  useSubscribe((fn) => s.subscribe(fn));
}

function Center({ children }: { children: preact.ComponentChildren }) {
  return <div class="control-center">{children}</div>;
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
    const next = encodeURIComponent(location.pathname + location.hash);
    return (
      <Center>
        <h1>illogical</h1>
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
      </Center>
    );
  }
  if (s.phase === "waiting")
    return (
      <Center>
        <h1>Approve this browser</h1>
        <p>
          You're signed in as <b>{s.login}</b>. Before this browser can reach your machines, approve it from a device you already use: open
          illogical there and it will ask.
        </p>
        <p>It will show this fingerprint; check it matches:</p>
        <p class="fingerprint" data-fingerprint={s.keys.id}>
          {fingerprint(s.keys.id)}
        </p>
        <p class="dim">Waiting…</p>
        <RecoveryForm s={s} />
      </Center>
    );
  return null;
}

/** Signed out, but following a link (#103): say what it was for. The hash
 * survives signing in, so it opens once you're in. */
function WhyHere() {
  const [hash, setHash] = useState(location.hash);
  const invite = /^#invite=([0-9a-f]+)\.([0-9a-f]+)$/.exec(hash);
  const [team, setTeam] = useState<{ name: string; by: string } | null>(null);
  useEffect(() => {
    const on = () => setHash(location.hash);
    addEventListener("hashchange", on);
    return () => removeEventListener("hashchange", on);
  }, []);
  useEffect(() => {
    setTeam(null);
    if (invite) void previewInvite(invite[1], invite[2]).then(setTeam);
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

function RecoveryForm({ s }: { s: ControlSession }) {
  const [open, setOpen] = useState(false);
  const [code, setCode] = useState("");
  const [err, setErr] = useState("");
  if (!open)
    return (
      <button class="control-linkish" data-use-recovery onClick={() => setOpen(true)}>
        Lost your other devices? Use a recovery code
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
      <input placeholder="Recovery code" value={code} onInput={(e) => setCode((e.target as HTMLInputElement).value)} aria-label="Recovery code" />
      <button type="submit">Use it</button>
      {err ? <p class="control-error">{err}</p> : null}
    </form>
  );
}

/** Once, after the account's first device: the codes to keep. */
function RecoveryCodes({ s }: { s: ControlSession }) {
  const all = s.recoveryCodes!.join("\n") + "\n";
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
        <button data-download-codes onClick={() => download("illogical-recovery-codes.txt", all)}>
          Download .txt
        </button>
        <button class="primary" data-saved-codes onClick={() => s.savedRecoveryCodes()}>
          I've saved them
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
          <b>Install illogical on the machine</b> (macOS or Linux):
          <CopyText text={INSTALL} data-install />
          <span class="dim">
            Or with <a href="https://illogical.widgets.wtf/#install" target="_blank" rel="noopener">Homebrew, or from source</a>.
          </span>
        </li>
        <li>
          <b>Join it to this account:</b>
          <CopyText text={`~/.local/bin/illogicald join ${s.info.url}`} data-join-cmd />
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

/** Over the app: approval prompts, a daemon's join, the device list. */
export function ControlOverlay({ s }: { s: ControlSession }) {
  useControl(s);
  const [hash, setHash] = useState(location.hash);
  const [panel, setPanel] = useState<null | "devices" | "add" | "teams" | "plan">(null);
  useEffect(() => {
    const on = () => setHash(location.hash);
    const open = (e: Event) => setPanel((e as CustomEvent<"devices" | "add" | "teams" | "plan">).detail);
    addEventListener("hashchange", on);
    addEventListener("illogical:control-panel", open);
    return () => {
      removeEventListener("hashchange", on);
      removeEventListener("illogical:control-panel", open);
    };
  }, []);
  if (s.phase !== "ready") return null;
  if (s.recoveryCodes) return <RecoveryCodes s={s} />;
  const join = /^#join=([A-Za-z0-9-]+)$/.exec(hash)?.[1];
  if (join) return <JoinPrompt s={s} code={join} />;
  const invite = /^#invite=([0-9a-f]+)\.([0-9a-f]+)$/.exec(hash);
  if (invite) return <InvitePrompt s={s} team={invite[1]} code={invite[2]} />;
  // Someone used an invite to a team I own: add them (sign the roster)?
  const req = s.teams.flatMap((t) => (t.role === "owner" ? t.requests.map((r) => ({ t, r })) : []))[0];
  if (req) return <AdmitPrompt s={s} team={req.t} req={req.r} />;
  const asking = s.pending[0];
  if (asking) return <DevicePrompt s={s} c={asking} />;
  if (s.joined) return <Joined s={s} />;
  if (panel === "devices") return <Devices s={s} close={() => setPanel(null)} />;
  if (panel === "teams") return <Teams s={s} close={() => setPanel(null)} />;
  if (panel === "plan") return <Plan s={s} close={() => setPanel(null)} />;
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
  history.replaceState(null, "", location.pathname + location.search);
  dispatchEvent(new HashChangeEvent("hashchange"));
}

function JoinPrompt({ s, code }: { s: ControlSession; code: string }) {
  const [j, setJ] = useState<JoinRequest | null>(null);
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);
  // "" is just me; else a team's id.
  const [to, setTo] = useState("");
  useEffect(() => {
    s.showJoin(code).then(
      (j) => {
        setJ(j);
        setTo(j.team?.team ?? "");
      },
      (e: Error) => setErr(e.message),
    );
  }, [code]);
  // Teams I own (#100), and the one it asked for even if I don't.
  const owned = s.teams.filter((t) => t.role === "owner" && t.verified);
  const asked = j?.team && !owned.some((t) => t.team === j.team!.team) ? j.team : null;
  const team = owned.find((t) => t.team === to);
  const notOwner = !!to && !team;
  const cancel = () => {
    if (j) void s.rejectJoin(j.code).catch(() => {});
    clearHash();
  };
  return (
    <Modal close={clearHash}>
      <h2>Add a machine?</h2>
      {err ? <p class="control-error">{err}</p> : null}
      {j ? (
        <>
          <p>
            <b>{j.cert.name}</b> asks to join {j.team ? <>the team <b data-join-team={j.team.team}>{j.team.name}</b></> : "your account"} with code{" "}
            <b data-join-code={j.code}>{j.code}</b>. Check that's the code it printed.
          </p>
          <p class="dim">Its key: {fingerprint(j.cert.device)}</p>
          {owned.length || asked ? (
            <p>
              <label>
                Join to{" "}
                <select class="control-select" data-join-to value={to} onChange={(e) => setTo((e.target as HTMLSelectElement).value)}>
                  <option value="">Just me</option>
                  {owned.map((t) => (
                    <option key={t.team} value={t.team}>
                      {t.roster.name}
                    </option>
                  ))}
                  {asked ? <option value={asked.team}>{asked.name} (you're not an owner)</option> : null}
                </select>
              </label>
            </p>
          ) : null}
          {notOwner ? (
            <p class="control-error" data-join-not-owner>
              Only the team's owners add its machines. Ask one of them to approve it, or pick Just me.
            </p>
          ) : (
            <p class="dim" data-join-grants>
              {team
                ? `The members of ${team.roster.name} reach it by their role: owners and editors drive its terminals, viewers watch.`
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
          Cancel
        </button>
        <button
          class="primary"
          data-approve-join
          disabled={!j || busy || notOwner}
          onClick={async () => {
            if (!j) return;
            setBusy(true);
            try {
              await s.approveJoin(j.code, j.cert, team?.team ?? null);
              clearHash();
            } catch (e) {
              setErr((e as Error).message);
              setBusy(false);
            }
          }}
        >
          Approve
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
      <h2>New device</h2>
      <p>
        <b>{c.name}</b> wants to reach your machines. Approve it only if it's yours and shows this fingerprint:
      </p>
      <p class="fingerprint" data-pending={c.device}>
        {fingerprint(c.device)}
      </p>
      {err ? <p class="control-error">{err}</p> : null}
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

function Devices({ s, close }: { s: ControlSession; close: () => void }) {
  const [err, setErr] = useState("");
  const [confirming, setConfirming] = useState<string | null>(null);
  const devices = [...s.trusted.values()].filter((c) => c.kind !== "recovery");
  return (
    <Modal close={close}>
      <h2>Devices and machines</h2>
      {s.rootMismatch ? (
        <p class="control-error">Control reports a different first device for this account than this browser pinned. New devices and machines won't be trusted here.</p>
      ) : null}
      <ul class="control-devices">
        {devices.map((c) => (
          <li key={c.device} data-device={c.device}>
            <span>
              {c.kind === "daemon" ? "▣" : "◉"} {c.name}
              {c.device === s.keys.id ? " (this browser)" : ""}
              {c.device === s.enrollment?.root ? " · first device" : ""}
            </span>
            <span class="dim">{fingerprint(c.device)}</span>
            {c.device !== s.keys.id ? (
              <button
                class={confirming === c.device ? "control-revoke danger" : "control-revoke"}
                title="It loses access at once"
                onClick={() => {
                  if (confirming !== c.device) return setConfirming(c.device);
                  setConfirming(null);
                  s.revoke(c.device).catch((e: Error) => setErr(e.message));
                }}
              >
                {confirming === c.device ? "Really remove?" : "Remove"}
              </button>
            ) : null}
          </li>
        ))}
      </ul>
      {err ? <p class="control-error">{err}</p> : null}
      <p class="dim">
        Account <CopyText inline text={s.account} data-account />
      </p>
      <NameLine s={s} />
      {s.info.passkeys ? (
        <p class="dim">
          {s.passkeys ? `${s.passkeys} passkey${s.passkeys === 1 ? "" : "s"} can sign in to this account. ` : "No passkey signs in to this account yet. "}
          <button
            class="control-linkish"
            data-add-passkey
            onClick={() =>
              passkeyRegister().then(
                () => void s.boot(),
                (e: Error) => setErr(e.message),
              )
            }
          >
            Add one
          </button>
        </p>
      ) : null}
      <div class="prompt-buttons">
        <button onClick={() => s.signOut(false)}>Sign out</button>
        <button onClick={close}>Done</button>
      </div>
    </Modal>
  );
}

const panel = (p: "devices" | "add" | "teams" | "plan") => () => dispatchEvent(new CustomEvent("illogical:control-panel", { detail: p }));

/** For the host menu. */
export function controlMenuItems(s: ControlSession): MenuItem[] {
  const shown = s.daemons.find((d) => d.name === directory.current);
  return [
    "separator",
    { header: `${s.login}${s.stale ? " · control unreachable" : ""}` },
    ...(s.sandboxesOpen ? [{ label: "New hosted VM", run: () => void s.startSandbox() }] : []),
    ...(shown?.sandbox ? [{ label: "Delete this VM", run: () => void s.deleteSandbox(shown.sandbox!) }] : []),
    { label: "Add a machine…", run: panel("add") },
    ...accountItems(s),
  ];
}

/** The account's own items: in the host menu, and on the no-machines
 * screen's header (#97). */
function accountItems(s: ControlSession): MenuItem[] {
  return [
    { label: "Teams…", run: panel("teams") },
    { label: "Devices and machines…", run: panel("devices") },
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
            onClick={() => s.acceptInvite(team, code).then(() => setDone(true), (e: Error) => setErr(e.message))}
          >
            Join
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
      <p class="dim">Owners approve everyone who uses an invite. Machines join a team when an owner approves them for it.</p>
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
  // Which button waits for a second click: "lock", or a member to remove.
  const [confirming, setConfirming] = useState<string | null>(null);
  const owner = t.role === "owner";
  return (
    <section class="team" data-team={t.team}>
      <h3>
        {t.roster.name} {t.locked ? <span class="control-error">· locked</span> : null}
      </h3>
      {owner ? (
        <>
          <p class="dim">
            Team id <CopyText inline text={t.team} data-team-id />. Add a machine to it with:
          </p>
          <CopyText text={`illogicald join ${s.info.url} --team ${t.team}`} data-team-join />
        </>
      ) : (
        <p class="dim" data-ask-owner>
          Ask an owner to add a machine.
        </p>
      )}
      <ul class="control-devices">
        {t.roster.members.map((m) => (
          <li key={m.account} data-member={m.account}>
            <span>
              {m.name}
              {m.account === s.account ? " (you)" : ""}
            </span>
            {owner && m.account !== s.account ? (
              <select
                value={m.role}
                aria-label={`${m.name}'s role`}
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
              <button
                class={confirming === m.account ? "control-revoke danger" : "control-revoke"}
                data-remove-member={m.account}
                title="They lose access to the team's machines at once"
                onClick={() => {
                  if (confirming !== m.account) return setConfirming(m.account);
                  setConfirming(null);
                  act(() => s.changeTeam(t.team, (ms) => ms.filter((x) => x.account !== m.account)));
                }}
              >
                {confirming === m.account ? "Really remove?" : "Remove"}
              </button>
            ) : (
              <span />
            )}
          </li>
        ))}
      </ul>
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
            <button data-invite={t.team} disabled={t.locked} onClick={() => act(async () => setLink(await s.invite(t.team, role)))}>
              Make a link
            </button>
          </div>
          {link ? (
            <p>
              Anyone with this link can ask to join (for a week); you approve each:
              <CopyText text={link} share data-invite-link />
            </p>
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
