// The swarm view (M26, `/#swarm`): every pane the page can see, on every
// host (M25), as one field of tiles that cluster by project, machine, kind,
// session or person, with what needs you lifted out to a rail of cards
// (M24's reasons, bundled by their bundle keys) that anyone who may answer
// can act on: allow, deny, answer, dismiss, and send the agent a follow-up
// (M29). The look and the motion are the prototype's
// (spikes/s16-swarm/canvas.html). On a phone the rail is a strip of cards
// along the bottom, and the field pinches and pans.

import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import type { Fleet, FleetPane } from "../fleet";
import type { Action, Reason } from "../proto";
import { AskCard, type Answered } from "../blocks/ask";
import { answeredLine, FollowUpBox, PermissionBody, PermissionButtons, VIEWER_NOTE, type Requester } from "../ui/answer-card";
import { Avatar } from "../ui/people";
import { usePhone, useSubscribe } from "../ui/hooks";
import { Field, type FieldPane } from "./field";
import { activityOf, bundleOf, cardTitle, GROUPINGS, groupOf, kindOf, KINDS, REASON_COL, reasonOf, type GroupBy } from "./model";

const BY_KEY = "illogical.swarm.by";
/** A done card leaves the rail by itself after this long. */
export const DONE_MS = 15_000;
/** An answered card (with its follow-up box) stays this long. */
const ANSWERED_MS = 60_000;

function savedBy(): GroupBy {
  try {
    const v = localStorage.getItem(BY_KEY) as GroupBy | null;
    return v && GROUPINGS.includes(v) ? v : "project";
  } catch {
    return "project";
  }
}

/** One card on the rail: a reason, and the panes it bundles. */
export interface Bundle {
  key: string;
  reason: Reason;
  panes: FleetPane[];
  since: number;
}

/** What's on the rail: the bundles that fit, and the rest waiting. */
export function bundlesOf(panes: FleetPane[], hidden: Set<string>): Bundle[] {
  const out = new Map<string, Bundle>();
  for (const p of panes) {
    const r = reasonOf(p);
    if (!r || hidden.has(`${p.key}@${r.since_ms}`)) continue;
    const key = bundleOf(p, r);
    const b = out.get(key);
    if (b) {
      b.panes.push(p);
      b.since = Math.min(b.since, r.since_ms);
    } else out.set(key, { key, reason: r, panes: [p], since: r.since_ms });
  }
  return [...out.values()].sort((a, b) => a.since - b.since || a.key.localeCompare(b.key));
}

/** A card answered by someone, kept a while for its follow-up box. */
interface Done {
  key: string;
  pane: FleetPane;
  answered: Answered;
  until: number;
}

export function SwarmView({
  fleet,
  back,
  focus,
}: {
  fleet: Fleet;
  back: () => void;
  /** Opened from a notification: the pane to show, and its card. */
  focus?: { host: string; pane: number } | null;
}) {
  useSubscribe((fn) => fleet.subscribe(fn));
  const phone = usePhone();
  const [by, setBy] = useState<GroupBy>(savedBy);
  const [peek, setPeek] = useState<{ key: string; x: number; y: number; text: string } | null>(null);
  const [hidden, setHidden] = useState<Set<string>>(new Set());
  const [answered, setAnswered] = useState<Done[]>([]);
  const [, tick] = useState(0);
  const canvas = useRef<HTMLCanvasElement>(null);
  const field = useRef<Field | null>(null);
  const rail = useRef<HTMLDivElement>(null);
  const bar = useRef<HTMLDivElement>(null);
  const prev = useRef<Map<string, FleetPane>>(new Map());
  const firstSeen = useRef<Map<string, number>>(new Map());
  const peekCache = useRef<Map<string, { at: number; text: string }>>(new Map());
  // The field calls these; they see this render's panes.
  const handlers = useRef<{ open(key: string): void; hover(key: string | null, x: number, y: number): Promise<void> | void }>({
    open: () => {},
    hover: () => {},
  });

  const panes = fleet.panes;
  const bundles = bundlesOf(panes, hidden);
  const cap = phone ? 4 : Math.max(2, Math.floor((innerHeight - 120) / 170));
  const shown = bundles.slice(0, cap);
  const waiting = bundles.slice(cap).reduce((n, b) => n + b.panes.length, 0);
  const onRail = new Map<string, string>();
  for (const b of shown) for (const p of b.panes) onRail.set(p.key, b.key);

  // The field: made once, fed on every change.
  useLayoutEffect(() => {
    const f = new Field(canvas.current!, {
      railW: () => (innerWidth < 760 ? 0 : 330),
      railH: () => (innerWidth < 760 ? 196 : 0),
      top: () => (bar.current?.getBoundingClientRect().bottom ?? 60) + 10,
      cardRect: (b) => rail.current?.querySelector<HTMLElement>(`[data-bundle="${CSS.escape(b)}"]`)?.getBoundingClientRect() ?? null,
      open: (key) => handlers.current.open(key),
      hover: (key, x, y) => void handlers.current.hover(key, x, y),
    });
    field.current = f;
    f.start();
    const resize = () => f.resize();
    addEventListener("resize", resize);
    (window as unknown as { __swarm?: Field }).__swarm = f;
    return () => {
      f.stop();
      removeEventListener("resize", resize);
    };
  }, []);

  useEffect(() => {
    field.current?.set(
      panes.map((p): FieldPane => {
        const r = reasonOf(p);
        return {
          key: p.key,
          kind: kindOf(p),
          group: groupOf(p, by),
          act: activityOf(p),
          stale: p.stale,
          label: `%${p.id} ${p.info.current?.text ?? p.info.command ?? p.info.title ?? kindOf(p)}`,
          where: p.host,
          lastOut: p.info.activity?.last_ms ?? 0,
          att: r ? { col: REASON_COL[r.kind], bundle: onRail.get(p.key) ?? null } : null,
        };
      }),
    );
  });

  useEffect(() => {
    field.current?.regroup();
  }, [by]);
  const choose = (g: GroupBy) => {
    setBy(g);
    try {
      localStorage.setItem(BY_KEY, g);
    } catch {
      // not remembered
    }
  };

  // Cards that were answered (by anyone): kept a while, saying who, with
  // the follow-up box. Done cards clear themselves after a while.
  useEffect(() => {
    const now = Date.now();
    const next: Done[] = [];
    const byKey = new Map(panes.map((p) => [p.key, p]));
    for (const [key, before] of prev.current) {
      const p = byKey.get(key);
      const r = before.info.reason;
      if (!p || !r || r.kind !== "ask" || reasonOf(p)) continue;
      const a = p.info.answered;
      // Worth keeping a card for: someone else answered, or the agent can
      // take a follow-up.
      const worth = a && (a.who !== fleet.me(p.host) || p.info.inbox || p.info.type === "agent");
      if (a && worth && (!r.ask || a.id === r.ask.id) && !answered.some((d) => d.key === key && d.answered.at_ms === a.at_ms)) {
        next.push({ key, pane: p, answered: a, until: now + ANSWERED_MS });
      }
    }
    prev.current = new Map(panes.filter((p) => reasonOf(p)).map((p) => [p.key, p]));
    if (next.length) setAnswered((d) => [...d.filter((x) => x.until > now), ...next]);
    for (const b of bundles) {
      if (b.reason.kind !== "done") continue;
      if (!firstSeen.current.has(b.key)) firstSeen.current.set(b.key, now);
    }
  });
  const live = useRef(bundles);
  live.current = bundles;
  useEffect(() => {
    const t = setInterval(() => {
      const now = Date.now();
      const gone = live.current.filter((b) => b.reason.kind === "done" && now - (firstSeen.current.get(b.key) ?? now) > DONE_MS);
      if (gone.length) {
        setHidden((h) => {
          const n = new Set(h);
          for (const b of gone) for (const p of b.panes) n.add(`${p.key}@${p.info.reason!.since_ms}`);
          return n;
        });
      }
      setAnswered((d) => (d.some((x) => x.until <= now) ? d.filter((x) => x.until > now) : d));
      tick((n) => n + 1);
    }, 1000);
    return () => clearInterval(t);
  }, []);

  // A notification's deep link: the pane, and its card.
  useEffect(() => {
    if (!focus) return;
    const key = `${focus.host}:${focus.pane}`;
    const t = setTimeout(() => field.current?.diveTo(key), 600);
    rail.current?.querySelector(`[data-panes~="${CSS.escape(key)}"]`)?.scrollIntoView({ block: "nearest", inline: "nearest" });
    return () => clearTimeout(t);
  }, [focus?.host, focus?.pane, !!rail.current?.querySelector(`[data-panes~="${CSS.escape(`${focus?.host}:${focus?.pane}`)}"]`)]);

  const byKey = (key: string) => panes.find((p) => p.key === key);
  const openKey = (key: string) => {
    const p = byKey(key);
    if (!p) return;
    back();
    fleet.open(p.host, p.id);
  };

  const hoverPane = async (key: string | null, x: number, y: number) => {
    if (!key) return setPeek(null);
    const p = byKey(key);
    if (!p) return setPeek(null);
    const cached = peekCache.current.get(key);
    setPeek({ key, x, y, text: cached?.text ?? "" });
    if (cached && Date.now() - cached.at < 2000) return;
    peekCache.current.set(key, { at: Date.now(), text: cached?.text ?? "" });
    try {
      const res = await fleet.request(p.host, "GET", `/api/panes/${p.id}/capture?format=text`);
      const text = res.ok && res.text ? await res.text() : "";
      const lines = text.split("\n").map((l) => l.trimEnd()).filter(Boolean).slice(-6).join("\n");
      peekCache.current.set(key, { at: Date.now(), text: lines });
      setPeek((pk) => (pk?.key === key ? { ...pk, text: lines } : pk));
    } catch {
      // stale host: no lines
    }
  };

  handlers.current = { open: openKey, hover: hoverPane };
  const machines = new Set(panes.map((p) => p.host)).size;
  const busy = panes.filter((p) => activityOf(p) > 0.3).length;
  const need = panes.filter((p) => reasonOf(p)).length;
  const clusters = field.current?.clusters ?? [];

  return (
    <div class={`swarm${phone ? " phone" : ""}`} data-swarm={by}>
      <canvas ref={canvas} class="swarm-field" aria-label="Every pane, clustered" />
      <div class="swarm-bar" ref={bar}>
        <div class="swarm-brand">
          <h1>
            Swarm <span>/ illogical</span>
          </h1>
        </div>
        <div class="swarm-stats">
          <div>
            <b data-stat="panes">{panes.length}</b>panes
          </div>
          <div>
            <b data-stat="machines">{machines}</b>machines
          </div>
          <div>
            <b data-stat="busy">{busy}</b>busy
          </div>
          <div class="needs">
            <b data-stat="need">{need}</b>need you
          </div>
        </div>
        <div class="swarm-spacer" />
        <div>
          <div class="swarm-seg-l">Cluster by</div>
          <div class="swarm-seg" role="group" aria-label="Cluster by">
            {GROUPINGS.map((g) => (
              <button key={g} data-g={g} aria-pressed={g === by} onClick={() => choose(g)}>
                {g}
              </button>
            ))}
          </div>
        </div>
        <div class="swarm-tools">
          <button data-fit onClick={() => field.current?.fitAll()}>
            Fit
          </button>
          <button data-swarm-back onClick={back}>
            Tabs
          </button>
        </div>
      </div>
      {fleet.notice && <div class="swarm-notice">{fleet.notice}</div>}

      <aside class="swarm-rail" aria-label="Needs you" ref={rail}>
        <header>
          <h2>Needs you</h2>
          <span>bundled by cause</span>
        </header>
        <div class="swarm-cards">
          {shown.length === 0 && answered.length === 0 && (
            <p class="swarm-empty">Nothing needs you. Panes that ask, fail, finish or wait for input lift out of the swarm and land here.</p>
          )}
          {shown.map((b) => (
            <Card key={b.key} b={b} fleet={fleet} focus={focus} show={() => field.current?.diveTo(b.panes[0].key)} open={() => openKey(b.panes[0].key)} />
          ))}
          {answered.map((d) => (
            <AnsweredCard key={`${d.key}@${d.answered.at_ms}`} d={d} fleet={fleet} close={() => setAnswered((x) => x.filter((y) => y !== d))} />
          ))}
        </div>
        {waiting > 0 && (
          <div class="swarm-waiting" data-waiting={waiting}>
            <b>{waiting}</b> more pulsing in place until there's room
          </div>
        )}
      </aside>

      <div class="swarm-foot">
        <div>
          <div class="swarm-legend">
            {Object.entries(KINDS).map(([k, c]) => (
              <span key={k}>
                <i style={{ background: `rgb(${c})` }} />
                {k}
              </span>
            ))}
          </div>
          <div class="swarm-hint">
            {phone ? "Pinch to zoom, drag to pan. Tap a pane to open it." : "Scroll to zoom. Drag to pan. Click a cluster name to dive in, a pane to open it."}
          </div>
        </div>
        <div class="swarm-clusters" hidden>
          {clusters.map((c) => (
            <span key={c.name} data-cluster={c.name} data-n={c.n} />
          ))}
        </div>
      </div>

      {peek && (
        <div class="swarm-peek" style={{ left: `${Math.min(peek.x + 16, innerWidth - 700)}px`, top: `${Math.min(peek.y + 16, innerHeight - 160)}px` }}>
          <div class="ph">
            <b>{byKey(peek.key) ? `%${byKey(peek.key)!.id}  ${byKey(peek.key)!.info.current?.text ?? byKey(peek.key)!.info.command ?? ""}` : ""}</b>
            <span>{byKey(peek.key) ? `${byKey(peek.key)!.host} · ${groupOf(byKey(peek.key)!, by)}` : ""}</span>
          </div>
          <pre>{peek.text}</pre>
        </div>
      )}
    </div>
  );
}

/** Act on a bundle: one request per host, naming its panes. */
async function act(fleet: Fleet, panes: FleetPane[], action: Action, extra: Record<string, unknown> = {}): Promise<string | null> {
  const hosts = new Map<string, FleetPane[]>();
  for (const p of panes) hosts.set(p.host, [...(hosts.get(p.host) ?? []), p]);
  let err: string | null = null;
  await Promise.all(
    [...hosts].map(async ([host, ps]) => {
      const body = ps.length === 1 ? { action, pane: ps[0].id, id: ps[0].info.reason?.ask?.id, ...extra } : { action, panes: ps.map((p) => p.id), ...extra };
      try {
        const res = await fleet.request(host, "POST", "/api/attention/act", body);
        if (!res.ok) err = (await res.json<{ error?: string; results?: { error?: string }[] }>().catch(() => null))?.error ?? `couldn't (${res.status})`;
      } catch (e) {
        err = String(e);
      }
    }),
  );
  return err;
}

function requester(fleet: Fleet, host: string): Requester {
  return (m, p, b) => fleet.request(host, m, p, b) as ReturnType<Requester>;
}

/** Teammates who have one of these panes open (M13 presence). */
function Watching({ fleet, panes }: { fleet: Fleet; panes: FleetPane[] }) {
  const seen = new Set<string>();
  const people = panes.flatMap((p) => fleet.presence(p.host).filter((x) => x.pane === p.id && x.who !== fleet.me(p.host)));
  const unique = people.filter((x) => !seen.has(x.who) && seen.add(x.who));
  if (!unique.length) return null;
  return (
    <span class="ask-watching" title={`${unique.map((p) => p.name).join(", ")} ${unique.length > 1 ? "have" : "has"} this open`}>
      {unique.map((p) => (
        <Avatar key={p.who} p={p} />
      ))}
    </span>
  );
}

function Card({ b, fleet, focus, show, open }: { b: Bundle; fleet: Fleet; focus?: { host: string; pane: number } | null; show: () => void; open: () => void }) {
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const r = b.reason;
  const n = b.panes.length;
  const first = b.panes[0];
  const machines = [...new Set(b.panes.map((p) => p.host))];
  const can = b.panes.every((p) => fleet.role(p) !== "viewer");
  const ids = b.panes.slice(0, 4).map((p) => `%${p.id}`).join(" ") + (n > 4 ? " …" : "");
  const project = first.info.project?.name ?? groupOf(first, "project");
  const ask = n === 1 ? first.info.ask : null;
  const run = async (action: Action, extra: Record<string, unknown> = {}) => {
    setBusy(true);
    const e = await act(fleet, b.panes, action, extra);
    setBusy(false);
    setErr(e);
  };
  const all = (label: string) => (n > 1 ? `${label} all ${n}` : label);
  const kind = r.kind === "exited" ? "failed" : r.kind;
  const focused = focus && b.panes.some((p) => p.host === focus.host && p.id === focus.pane);
  return (
    <div
      class={`swarm-card in t-${kind}${focused ? " focus" : ""}`}
      data-bundle={b.key}
      data-panes={b.panes.map((p) => p.key).join(" ")}
      data-kind={r.kind}
      style={{ "--c": `rgb(${REASON_COL[r.kind]})` }}
    >
      <div class="ch">
        <b>{cardTitle(r, n, machines)}</b>
        <Watching fleet={fleet} panes={b.panes} />
        {n > 1 && <span class="n">×{n}</span>}
      </div>
      <div class="cm">
        {ids} · {machines.length > 1 ? `${machines.length} machines` : machines[0]} · {project}
      </div>
      {ask?.kind === "permission" ? (
        <PermissionBody ask={ask} />
      ) : (
        <div class="cq">
          {r.headline}
          {r.command && r.kind !== "ask" && !r.headline.includes(r.command) ? <code> {r.command}</code> : null}
        </div>
      )}
      {!can ? (
        <p class="ask-viewer">{VIEWER_NOTE}</p>
      ) : ask?.kind === "permission" ? (
        <PermissionButtons ask={ask} act={(action, extra) => void run(action, { ...extra, id: ask.id })} />
      ) : ask && r.ask?.what === "question" ? (
        <AskCard
          ask={ask}
          actions={{
            answer: (content) => void run("answer", { content }),
            decline: () => void run("deny"),
          }}
        />
      ) : (
        <div class="ca">
          {r.actions.includes("allow") && (
            <button class="pri" disabled={busy} onClick={() => void run("allow")}>
              {all("Allow")}
            </button>
          )}
          {r.actions.includes("deny") && r.ask?.what === "approve" && (
            <button disabled={busy} onClick={() => void run("deny")}>
              {all("Deny")}
            </button>
          )}
          {r.actions.includes("dismiss") && r.kind !== "ask" && (
            <button class={r.actions.length === 1 ? "pri" : ""} disabled={busy} onClick={() => void run("dismiss")}>
              {all("Dismiss")}
            </button>
          )}
          {r.ask?.what === "question" && <button onClick={open}>Answer…</button>}
        </div>
      )}
      <div class="ca ca-nav">
        <button class="ghost" onClick={open}>
          Open
        </button>
        <button class="ghost" onClick={show}>
          Show
        </button>
        {can && r.kind === "ask" && (
          <button class="ghost" disabled={busy} onClick={() => void run("dismiss")}>
            {all("Dismiss")}
          </button>
        )}
      </div>
      {err && <div class="swarm-card-err">{err}</div>}
    </div>
  );
}

function AnsweredCard({ d, fleet, close }: { d: Done; fleet: Fleet; close: () => void }) {
  const p = d.pane;
  const can = fleet.role(p) !== "viewer";
  return (
    <div class="swarm-card in t-answered" data-answered={d.answered.id} data-panes={p.key} style={{ "--c": "rgb(124,134,152)" }}>
      <div class="ch">
        <b class="answered-by">{answeredLine(d.answered)}</b>
        <button class="link swarm-close" title="Close" onClick={close}>
          ✕
        </button>
      </div>
      <div class="cm">
        %{p.id} · {p.host}
      </div>
      <div class="cq">{d.answered.headline}</div>
      {can && (p.info.inbox || p.info.type === "agent") && (
        <FollowUpBox
          pane={p.id}
          request={requester(fleet, p.host)}
          paneOp={(op) => fleet.paneOp(p.host, p.id, op)}
          toast={() => {}}
        />
      )}
    </div>
  );
}
