// Getting started (#110): what isn't on the first screen, one step at a
// time: welcome, the phone (Tailscale), the cloud (illogical control),
// agents, and a summary. Opens by itself once per browser, then from the
// session menu and the phone's sheet, and says so when it closes.
//
// The steps that used to be commands to copy are buttons: the daemon runs
// them for its owner (`/api/setup`, crates/daemon/src/setup.rs) and says
// when one needs something only a person can do (a one-time sudo, a switch
// in Tailscale's admin console), with the command or the link. Where the
// daemon can't be asked (a page through control, a guest), each step falls
// back to the command.

import qrcode from "qrcode-generator";
import { useEffect, useState } from "preact/hooks";
import type { Client } from "../client";
import { CopyText } from "./copy";
import { notifyBlocker, pushNow, serveCommand, subscribePush } from "./notify";
import { startAgent } from "./agent-dialog";

const DOCS = "https://github.com/arugula-salad/illogical/blob/main/docs";
/** illogical cloud, unless the daemon was started with `--control` (#207). */
const CONTROL = "https://control.illogical.widgets.wtf";
const SEEN_KEY = "illogical.getting-started";

/** What `/api/host` says about this daemon (the parts for this panel). */
interface HostInfo {
  name: string;
  tailnet_url?: string;
  tailnet_seen?: boolean;
  control?: string;
  team?: string;
}

/** `GET /api/setup`. */
interface Setup {
  tailscale: {
    state: "missing" | "stopped" | "needs-login" | "running";
    host?: string;
    https: boolean;
    serving: boolean;
    url?: string;
    seen: boolean;
  };
  control: {
    joined?: string;
    team?: string;
    pending?: { code: string; approve: string; expires_ms: number };
    /** Approved: the account's fingerprint, to check before it's saved. */
    confirm?: { account: string; approver: string; place: string };
    error?: string;
    url: string;
  };
  claude: { installed: boolean; tools: boolean };
}

/** What a button did. */
interface Outcome {
  ok: boolean;
  error?: string;
  fix?: string;
  link?: { label: string; url: string };
}

const STEPS = [
  { id: "welcome", label: "Welcome" },
  { id: "phone", label: "Phone" },
  { id: "cloud", label: "Cloud" },
  { id: "agents", label: "Agents" },
  { id: "ready", label: "Ready" },
] as const;
type StepId = (typeof STEPS)[number]["id"];

/** Older names for steps, from callers that open it at one. */
type Section = StepId | "control";

let shown: { client: Client | null; section?: Section } | null = null;
const listeners = new Set<() => void>();
const changed = () => listeners.forEach((fn) => fn());
let lastClient: Client | null = null;
/** Set when it closes, for the "it's in the menu" note. */
let closedOnce = false;

export function openGettingStarted(section?: Section, client?: Client) {
  shown = { client: client ?? lastClient, section };
  changed();
}

function seen(): boolean {
  try {
    return localStorage.getItem(SEEN_KEY) !== null;
  } catch {
    // No storage (a private window, blocked): don't greet every time.
    return true;
  }
}

function markSeen() {
  try {
    localStorage.setItem(SEEN_KEY, "seen");
  } catch {
    // Nothing to remember it in.
  }
}

/** Open it the first time this browser sees its own daemon: not on a page
 * from control, not for a guest, and not under automation (tests expect a
 * clean first screen; welcome.spec.ts turns it back on). */
export function useFirstRun(client: Client) {
  lastClient = client;
  const ready = !!client.state && client.state.sessions.length > 0;
  useEffect(() => {
    if (!ready || client.e2e || client.state?.roles || navigator.webdriver || seen()) return;
    markSeen();
    openGettingStarted(undefined, client);
  }, [client, ready]);
}

export function GettingStartedLayer() {
  const [, setTick] = useState(0);
  useEffect(() => {
    const fn = () => setTick((t) => t + 1);
    listeners.add(fn);
    return () => void listeners.delete(fn);
  }, []);
  if (shown)
    return (
      <GettingStarted
        client={shown.client}
        section={shown.section}
        close={() => {
          markSeen();
          shown = null;
          closedOnce = true;
          changed();
          setTimeout(() => {
            closedOnce = false;
            changed();
          }, 6000);
        }}
      />
    );
  // Where it went: it doesn't just vanish.
  return closedOnce ? (
    <div class="start-toast" role="status" data-start-toast>
      Getting started is in the session menu, any time.
    </div>
  ) : null;
}

async function post(path: string, body?: unknown): Promise<Outcome & Record<string, unknown>> {
  try {
    const r = await fetch(path, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: body === undefined ? "{}" : JSON.stringify(body),
    });
    if (!r.ok) return { ok: false, error: (await r.json().catch(() => null))?.error ?? `HTTP ${r.status}` };
    return await r.json();
  } catch (e) {
    return { ok: false, error: String(e) };
  }
}

function stepFor(section?: Section): number {
  const id = section === "control" ? "cloud" : section;
  const i = STEPS.findIndex((s) => s.id === id);
  return i < 0 ? 0 : i;
}

function GettingStarted({ client, section, close }: { client: Client | null; section?: Section; close: () => void }) {
  const [step, setStep] = useState(stepFor(section));
  const [host, setHost] = useState<HostInfo | null>(null);
  const [setup, setSetup] = useState<Setup | null>(null);
  // The daemon can't run the steps here (through control, or not its
  // owner): show the commands instead.
  const [manual, setManual] = useState(false);
  const [, setTick] = useState(0);
  useEffect(() => subscribePush(() => setTick((t) => t + 1)), []);
  useEffect(() => setStep(stepFor(section)), [section]);

  const refresh = async () => {
    try {
      const r = await fetch("/api/setup");
      if (!r.ok) throw new Error(String(r.status));
      setSetup((await r.json()) as Setup);
    } catch {
      setManual(true);
    }
  };
  // The cloud step's part comes back first (the rest asks Tailscale and
  // Claude Code): which control the button joins, at once (#207).
  const [early, setEarly] = useState<Setup["control"] | null>(null);
  useEffect(() => {
    void refresh();
    fetch("/api/setup?part=control")
      .then((r) => (r.ok ? (r.json() as Promise<Pick<Setup, "control">>) : null))
      .then((r) => r && setEarly(r.control))
      .catch(() => {});
    fetch("/api/host")
      .then((r) => (r.ok ? (r.json() as Promise<HostInfo>) : null))
      .then(setHost)
      .catch(() => {});
  }, []);

  // While a join waits for approval (or was just confirmed and the daemon
  // is picking it up), look for the answer.
  const [confirmed, setConfirmed] = useState(false);
  const waiting = !!setup?.control.pending || (confirmed && !setup?.control.joined);
  useEffect(() => {
    if (!waiting) return;
    const t = setInterval(async () => {
      const r = await fetch("/api/setup?part=control").catch(() => null);
      const got = r?.ok ? ((await r.json()) as Pick<Setup, "control">) : null;
      if (got) setSetup((s) => (s ? { ...s, control: got.control } : s));
    }, 2000);
    return () => clearInterval(t);
  }, [waiting]);

  const done: Record<StepId, boolean> = {
    welcome: true,
    phone: !!(setup?.tailscale.serving || host?.tailnet_seen),
    cloud: !!(setup?.control.joined || host?.control),
    agents: !!setup?.claude.tools,
    ready: false,
  };
  const id = STEPS[step].id;
  const last = step === STEPS.length - 1;
  const go = (i: number) => setStep(Math.max(0, Math.min(STEPS.length - 1, i)));
  const name = host?.name ?? "this machine";

  return (
    <div class="prompt-backdrop start-backdrop" role="dialog" aria-label="Getting started" onPointerDown={(e) => e.target === e.currentTarget && close()}>
      <div
        class="start"
        data-getting-started
        data-step={id}
        onKeyDown={(e) => {
          if (e.key === "Escape") close();
          if ((e.target as HTMLElement).closest("input, textarea")) return;
          if (e.key === "ArrowRight") go(step + 1);
          if (e.key === "ArrowLeft") go(step - 1);
        }}
        tabIndex={-1}
      >
        <header class="start-head">
          <div class="start-brand">
            illogical <span>setup</span>
          </div>
          <button class="start-x" aria-label="Close" onClick={close}>
            ✕
          </button>
        </header>

        <ol class="start-rail" aria-label="Steps">
          {STEPS.map((s, i) => (
            <li key={s.id}>
              <button
                class={`start-seg${i === step ? " now" : ""}${done[s.id] && s.id !== "welcome" ? " done" : ""}`}
                aria-current={i === step ? "step" : undefined}
                onClick={() => go(i)}
                data-start-seg={s.id}
              >
                <b>{String(i + 1).padStart(2, "0")}</b> <span>{s.label}</span>
              </button>
            </li>
          ))}
        </ol>

        <div class="start-body">
          <div class="start-kicker" data-start-progress>
            Step {step + 1} / {STEPS.length} · {STEPS[step].label}
          </div>
          {id === "welcome" && <Welcome name={name} client={client} />}
          {id === "phone" && <Phone name={name} setup={setup} host={host} manual={manual} refresh={refresh} />}
          {id === "cloud" && (
            <Cloud setup={setup} early={early} host={host} manual={manual} refresh={refresh} setSetup={setSetup} onConfirmed={() => setConfirmed(true)} />
          )}
          {id === "agents" && <Agents client={client} setup={setup} manual={manual} refresh={refresh} close={close} />}
          {id === "ready" && <Ready done={done} go={go} />}
        </div>

        <footer class="start-foot">
          <button class="start-btn ghost" disabled={step === 0} onClick={() => go(step - 1)}>
            ‹ Back
          </button>
          <span class="start-hint">Always in the session menu: Getting started</span>
          {last ? (
            <button class="start-btn primary" onClick={close}>
              Done
            </button>
          ) : (
            <button class="start-btn primary" onClick={() => go(step + 1)} data-start-next>
              {step === 0 ? "Set it up ›" : done[id] ? "Next ›" : "Skip for now ›"}
            </button>
          )}
        </footer>
      </div>
    </div>
  );
}

// ---- steps

function Welcome({ name, client }: { name: string; client: Client | null }) {
  const panes = client?.state?.panes.length ?? 0;
  return (
    <section class="start-step">
      <h2>Terminals that outlive their windows</h2>
      <p class="start-lede">
        <b>{name}</b> keeps {panes === 1 ? "a pane" : `${panes} panes`} running whether or not a window is open. Close this tab, open it on your
        phone, come back tomorrow: they're where you left them.
      </p>
      <ul class="start-cards">
        <li>
          <span class="start-glyph">◎</span>
          <div>
            <h3>Right-click anything</h3>
            <p>Tabs, panes and the tab bar have menus. That's where everything is.</p>
          </div>
        </li>
        <li>
          <span class="start-glyph">⇲</span>
          <div>
            <h3>Drag to split</h3>
            <p>Drop a tab on a pane's edge to split it there.</p>
          </div>
        </li>
        <li>
          <span class="start-glyph">⌘</span>
          <div>
            <h3>Jump anywhere</h3>
            <p>
              <kbd>Ctrl</kbd> <kbd>Shift</kbd> <kbd>G</kbd> finds a pane, a tab or a command.
            </p>
          </div>
        </li>
        <li>
          <span class="start-glyph">›_</span>
          <div>
            <h3>From a terminal</h3>
            <p>
              The same tabs and splits, over ssh too: <CopyText inline text="illogical tui" />
            </p>
          </div>
        </li>
      </ul>
      <p class="start-dim">Next: your phone, the cloud and agents. Each takes a click, and you can skip any of them.</p>
    </section>
  );
}

/** One line of a checklist: done, waiting, wrong, or not yet. */
function Check({ state, children }: { state: "done" | "wait" | "fail" | "todo"; children: preact.ComponentChildren }) {
  return (
    <li class={`start-check ${state}`} data-check={state}>
      <i />
      <span>{children}</span>
    </li>
  );
}

function Said({ outcome }: { outcome: Outcome | null }) {
  if (!outcome || outcome.ok) return null;
  return (
    <div class="start-said" data-start-error>
      <p>{outcome.error}</p>
      {outcome.fix && (
        <>
          <p class="start-dim">Run this once on the machine, then try again:</p>
          <CopyText text={outcome.fix} data-start-fix />
        </>
      )}
      {outcome.link && (
        <a class="start-btn" href={outcome.link.url} target="_blank" rel="noreferrer" data-start-link>
          {outcome.link.label} ↗
        </a>
      )}
    </div>
  );
}

function Qr({ text }: { text: string }) {
  const qr = qrcode(0, "M");
  qr.addData(text);
  qr.make();
  const n = qr.getModuleCount();
  let d = "";
  for (let r = 0; r < n; r++) for (let c = 0; c < n; c++) if (qr.isDark(r, c)) d += `M${c} ${r}h1v1h-1z`;
  return (
    <svg class="start-qr" viewBox={`-2 -2 ${n + 4} ${n + 4}`} role="img" aria-label={`QR code for ${text}`} data-start-qr>
      <rect x="-2" y="-2" width={n + 4} height={n + 4} fill="#fff" />
      <path d={d} fill="#06080d" />
    </svg>
  );
}

function Phone({ name, setup, host, manual, refresh }: { name: string; setup: Setup | null; host: HostInfo | null; manual: boolean; refresh: () => Promise<void> }) {
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const ts = setup?.tailscale;
  const url = ts?.url ?? host?.tailnet_url;
  const blocked = notifyBlocker();
  const serve = async () => {
    setBusy(true);
    const o = await post("/api/setup/tailscale");
    setOutcome(o);
    await refresh();
    setBusy(false);
  };
  return (
    <section class="start-step">
      <h2>Reach {name} from your phone</h2>
      <p class="start-lede">Tailscale puts this machine on your private network, with HTTPS. Your phone opens it like an app, and gets notified when a pane needs you.</p>
      {manual ? (
        <>
          <p>On this machine:</p>
          <CopyText text={serveCommand()} data-serve-command />
        </>
      ) : (
        <>
          <ul class="start-checks">
            <Check state={!ts ? "todo" : ts.state === "running" ? "done" : ts.state === "missing" ? "fail" : "wait"}>
              {!ts ? "Tailscale" : ts.state === "running" ? `On the tailnet as ${ts.host ?? name}` : ts.state === "missing" ? "Tailscale isn't installed" : ts.state === "needs-login" ? "Tailscale needs you to sign in" : "Tailscale isn't running"}
            </Check>
            <Check state={ts?.https ? "done" : "todo"}>HTTPS certificates for the tailnet</Check>
            <Check state={ts?.serving ? "done" : "todo"}>Served at its tailnet address</Check>
            <Check state={ts?.seen || host?.tailnet_seen ? "done" : "todo"}>Opened from another device</Check>
          </ul>
          {!ts?.serving && (
            <button class="start-btn primary big" disabled={busy || !setup} onClick={serve} data-start-serve>
              {busy ? "Putting it on the tailnet…" : "Put it on my tailnet"}
            </button>
          )}
          <Said outcome={outcome} />
          {ts?.state === "missing" && (
            <p class="start-dim" data-start-no-tailscale>
              No Tailscale? The next step, the cloud, reaches this machine from your phone too.
            </p>
          )}
        </>
      )}
      {url && (ts?.serving ?? true) && (
        <div class="start-reach">
          <Qr text={url} />
          <div>
            <p>Scan it, or open it on the phone (signed in to Tailscale as you):</p>
            <CopyText text={url} share data-tailnet-url />
            <p class="start-dim">
              Then add it to the Home Screen, open it from there, and turn on <em>Notify this device</em> in its menu (☰).
              {blocked && pushNow() !== "insecure" && <> This device: {blocked}.</>}
            </p>
          </div>
        </div>
      )}
    </section>
  );
}

function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

function Cloud({
  setup,
  early,
  host,
  manual,
  refresh,
  setSetup,
  onConfirmed,
}: {
  setup: Setup | null;
  early: Setup["control"] | null;
  host: HostInfo | null;
  manual: boolean;
  refresh: () => Promise<void>;
  setSetup: (fn: (s: Setup | null) => Setup | null) => void;
  onConfirmed: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const c = setup?.control;
  const joined = c?.joined ?? host?.control;
  const team = c?.team ?? host?.team;
  // The control this daemon joins by default: its own, if it has one.
  const control = c?.url || early?.url || CONTROL;
  const ours = control === CONTROL;
  const connect = async () => {
    setBusy(true);
    setError(null);
    const r = await post("/api/setup/control", { url: control });
    if (r.error) setError(r.error);
    else if (r.pending) setSetup((s) => (s ? { ...s, control: { ...s.control, pending: r.pending as NonNullable<Setup["control"]["pending"]> } } : s));
    await refresh();
    setBusy(false);
  };
  // The person compared the account's fingerprints: keep the join, or drop it.
  const confirm = async (same: boolean) => {
    setBusy(true);
    const r = await post("/api/setup/control/confirm", { same });
    const got = (r as { control?: Setup["control"] }).control;
    if (got) setSetup((s) => (s ? { ...s, control: got } : s));
    if (same) onConfirmed();
    setBusy(false);
  };
  return (
    <section class="start-step">
      <h2>Use it from anywhere</h2>
      <p class="start-lede">
        illogical cloud reaches this machine from any browser, with no tailnet, and lets your team in on the sessions you share. Your terminals stay here:
        the cloud passes encrypted traffic, and only your devices hold the keys.
      </p>
      {manual ? (
        <>
          <p>On this machine:</p>
          <CopyText text={`illogicald join ${control}`} data-join-command />
        </>
      ) : joined ? (
        <ul class="start-checks">
          <Check state="done">
            <span data-start-joined>Joined{team ? ` to the team ${team}` : " to your account"}</span> ·{" "}
            <a href={joined} target="_blank" rel="noreferrer">
              open the cloud ↗
            </a>
          </Check>
        </ul>
      ) : c?.confirm ? (
        <div class="start-code" data-start-confirm>
          <div class="start-kicker">Approved on {c.confirm.approver || "your device"}. Is this your account?</div>
          <div class="start-code-big start-fp" data-start-account={c.confirm.account}>
            {c.confirm.account}
          </div>
          <p class="start-dim">
            The device you approved on shows your account's fingerprint in the approval and under Devices and machines… Check they're the same before this
            machine trusts the account.
          </p>
          <div class="start-confirm">
            <button class="start-btn primary" disabled={busy} onClick={() => void confirm(true)} data-start-same>
              They match
            </button>
            <button class="start-btn" disabled={busy} onClick={() => void confirm(false)} data-start-different>
              They don't
            </button>
          </div>
        </div>
      ) : c?.pending ? (
        <div class="start-code" data-start-pending>
          <div class="start-kicker">Approve this code on a signed-in device</div>
          <div class="start-code-big" data-start-code>
            {c.pending.code}
          </div>
          <a class="start-btn primary big" href={c.pending.approve} target="_blank" rel="noreferrer" data-start-approve>
            Approve in illogical cloud ↗
          </a>
          <p class="start-dim">
            <span class="start-pulse" /> Waiting for the approval. New here? It signs you up first (a passkey or GitHub), then asks: your account, or a team you own.
          </p>
        </div>
      ) : (
        <>
          <button class="start-btn primary big" disabled={busy || !setup} onClick={connect} data-start-connect>
            {busy ? "Asking the cloud…" : ours ? "Connect to illogical cloud" : `Connect to ${hostOf(control)}`}
          </button>
          {(error ?? c?.error) && (
            <div class="start-said" data-start-error>
              <p>{error ?? c?.error}</p>
            </div>
          )}
        </>
      )}
    </section>
  );
}

function Agents({ client, setup, manual, refresh, close }: { client: Client | null; setup: Setup | null; manual: boolean; refresh: () => Promise<void>; close: () => void }) {
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const session = client?.session ?? null;
  const cl = setup?.claude;
  const add = async () => {
    setBusy(true);
    setOutcome(await post("/api/setup/claude"));
    await refresh();
    setBusy(false);
  };
  return (
    <section class="start-step">
      <h2>Put agents to work</h2>
      <p class="start-lede">Claude and Codex run here as agent blocks. When one asks something, it's a card you answer from here or the phone.</p>
      <ul class="start-cards">
        <li>
          <span class="start-glyph agent">✦</span>
          <div>
            <h3>Start an agent</h3>
            <p>Give it a task in this session; watch it work in a pane of its own.</p>
            {client && session !== null && (
              <button
                class="start-btn"
                data-start-agent
                onClick={() => {
                  close();
                  startAgent(client, { session, from: client.active() });
                }}
              >
                Start an agent…
              </button>
            )}
          </div>
        </li>
        <li>
          <span class="start-glyph agent">⚙</span>
          <div>
            <h3>Claude Code, with illogical's tools</h3>
            <p>Builds and servers in panes you can watch and take over, and its questions on cards.</p>
            {manual ? (
              <CopyText text="claude mcp add illogical -- illogical mcp" data-mcp-command />
            ) : cl?.tools ? (
              <ul class="start-checks">
                <Check state="done">
                  <span data-start-tools>Claude Code has illogical's tools</span>
                </Check>
              </ul>
            ) : (
              <button class="start-btn" disabled={busy || !setup} onClick={add} data-start-claude>
                {busy ? "Adding…" : cl && !cl.installed ? "Claude Code isn't installed" : "Add illogical to Claude Code"}
              </button>
            )}
            <Said outcome={outcome} />
            <p class="start-dim">
              <a href={`${DOCS}/cli.md#mcp`} target="_blank" rel="noreferrer">
                Which tools to allow
              </a>{" "}
              ·{" "}
              <a href={`${DOCS}/cli.md#claude-code-in-a-pane`} target="_blank" rel="noreferrer">
                Claude Code in a pane
              </a>
            </p>
          </div>
        </li>
      </ul>
    </section>
  );
}

function Ready({ done, go }: { done: Record<StepId, boolean>; go: (i: number) => void }) {
  const rows: [StepId, string, string][] = [
    ["phone", "On your phone", "Not yet: Tailscale"],
    ["cloud", "In illogical cloud", "Not yet: connect"],
    ["agents", "Claude Code has illogical's tools", "Not yet: add them"],
  ];
  const all = rows.every(([id]) => done[id]);
  return (
    <section class="start-step">
      <h2>{all ? "You're set" : "Almost there"}</h2>
      <ul class="start-checks">
        {rows.map(([id, yes, no]) => (
          <Check key={id} state={done[id] ? "done" : "todo"}>
            {done[id] ? (
              yes
            ) : (
              <button class="start-linkish" onClick={() => go(STEPS.findIndex((s) => s.id === id))}>
                {no} ›
              </button>
            )}
          </Check>
        ))}
      </ul>
      <p class="start-lede">
        Anything you skipped is a click away: this is <b>Getting started</b> in the session menu (and the ☰ sheet on a phone), any time.
      </p>
    </section>
  );
}
