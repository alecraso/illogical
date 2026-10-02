// The control-mode screens (M17): signing in, waiting for this browser to
// be approved, approving other devices and daemons' join codes, the device
// list, and how to add a machine.

import { useEffect, useState } from "preact/hooks";
import { passkeyRegister, passkeySignIn, type ControlSession } from "../control";
import { fingerprint, type Cert } from "../e2e/cert.ts";
import { useSubscribe } from "./hooks";

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

function PasskeyButtons() {
  const [err, setErr] = useState("");
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
      <button class="control-signin control-secondary" data-signup="passkey" onClick={() => go(passkeyRegister)}>
        New here? Make an account with a passkey
      </button>
      {err ? <p class="control-error">{err}</p> : null}
    </>
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
  return (
    <Modal>
      <h2>Your recovery codes</h2>
      <p>If you lose every device that can approve new ones, one of these lets a new browser in. Each works once. Keep them somewhere safe and offline: they're shown only now, and this service never had them.</p>
      {s.recoveryCodes!.map((c) => (
        <p key={c} class="control-cmd" data-recovery-code>
          {c}
        </p>
      ))}
      <div class="prompt-buttons">
        <button class="primary" data-saved-codes onClick={() => s.savedRecoveryCodes()}>
          I've saved them
        </button>
      </div>
    </Modal>
  );
}

/** Ready, but no machine has joined yet. */
export function NoMachines({ s }: { s: ControlSession }) {
  return (
    <Center>
      <h1>Add a machine</h1>
      <AddMachine s={s} />
    </Center>
  );
}

function AddMachine({ s }: { s: ControlSession }) {
  return (
    <div class="control-add">
      <p>Install illogical on it, then run:</p>
      <pre class="control-cmd">illogicald join {s.info.url}</pre>
      <p>It prints a link with a code. Open the link here (or type the code below) and approve it.</p>
      <JoinCodeForm s={s} />
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
  const [panel, setPanel] = useState<null | "devices" | "add">(null);
  useEffect(() => {
    const on = () => setHash(location.hash);
    const open = (e: Event) => setPanel((e as CustomEvent<"devices" | "add">).detail);
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
  const asking = s.pending[0];
  if (asking) return <DevicePrompt s={s} c={asking} />;
  if (panel === "devices") return <Devices s={s} close={() => setPanel(null)} />;
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
  const [j, setJ] = useState<{ code: string; cert: Cert } | null>(null);
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    s.showJoin(code).then(setJ, (e: Error) => setErr(e.message));
  }, [code]);
  return (
    <Modal close={clearHash}>
      <h2>Add a machine?</h2>
      {err ? <p class="control-error">{err}</p> : null}
      {j ? (
        <>
          <p>
            <b>{j.cert.name}</b> asks to join your account with code <b data-join-code={j.code}>{j.code}</b>. Check that's the code it printed.
          </p>
          <p class="dim">Its key: {fingerprint(j.cert.device)}</p>
        </>
      ) : err ? null : (
        <p class="dim">Looking up {code}…</p>
      )}
      <div class="prompt-buttons">
        <button onClick={clearHash}>Cancel</button>
        <button
          class="primary"
          data-approve-join
          disabled={!j || busy}
          onClick={async () => {
            if (!j) return;
            setBusy(true);
            try {
              await s.approveJoin(j.code, j.cert);
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

/** For the host menu. */
export function controlMenuItems(s: ControlSession) {
  const panel = (p: "devices" | "add") => () => dispatchEvent(new CustomEvent("illogical:control-panel", { detail: p }));
  return [
    "separator" as const,
    { header: `${s.login}${s.stale ? " · control unreachable" : ""}` },
    { label: "Add a machine…", run: panel("add") },
    { label: "Devices and machines…", run: panel("devices") },
  ];
}
