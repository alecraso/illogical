// The swarm view (M26, `/#swarm`): every pane the page can see, on every
// host (M25), as one field of tiles that cluster by project, machine, kind,
// session or person, with what needs you lifted out to a rail of cards
// (M24's reasons, bundled by their bundle keys) that anyone who may answer
// can act on: allow, deny, answer, dismiss, and send the agent a follow-up
// (M29). Editors (M28) are tiles too: clicking one that joined from VS
// Code, Cursor or nvim follows its cursor (`follow.tsx`); its debugger
// stopping, errors after a save, a merge conflict and Claude Code's diffs
// are cards. The look and the motion are the prototype's
// (archive/spikes:spikes/s16-swarm/canvas.html). On a phone the rail is a strip of cards
// along the bottom, and the field pinches and pans.

import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import type { Fleet, FleetPane } from "../fleet";
import { gateKey, type Action, type ActRequest, type ActResponse, type OpenRequest, type OpenResponse, type Reason } from "../proto";
import { AskCard, type Answered } from "../blocks/ask";
import { ANSWERED_MS, answeredLine, FollowUpBox, PermissionBody, PermissionButtons, VIEWER_NOTE, type Requester } from "../ui/answer-card";
import { Avatar } from "../ui/people";
import { MenuLayer, openMenu } from "../ui/menu";
import { usePhone, useSubscribe } from "../ui/hooks";
import { Field, type FieldHooks, type FieldPane, type HistoryRun, type SwarmScene } from "./field";
import { FollowView, appName } from "./follow";
import { DiffCard } from "../ui/diff-card";
import { runnerLabel, runners, subscribeRunners, watchRunners } from "../runners";
import { activityOf, bundleOf, cardTitle, followable, GROUPINGS, groupOf, isPresence, kindOf, KINDS, REASON_COL, reasonOf, type GroupBy } from "./model";

const BY_KEY = "illogical.swarm.by";
const THEME_KEY = "illogical.swarm.theme";
/** How the swarm is drawn. Blocks is the field; the city is 3D; the hive is a
 * cell per pane and the timeline a lane per pane over time. */
export type Theme = "blocks" | "city" | "hive" | "timeline";
/** The themes on offer without labs. The others are built but only a machine
 * with labs offers them. With one, there's no picker. */
export const THEMES: Theme[] = ["blocks"];
const ALL_THEMES: Theme[] = ["blocks", "city", "hive", "timeline"];

/** The themes to offer: `THEMES`, or all of them where the home machine has
 * labs. */
export function offeredThemes(labs: boolean): Theme[] {
  return labs ? ALL_THEMES : THEMES;
}

/** A done card leaves the rail by itself after this long. */
export const DONE_MS = 15_000;

function savedBy(): GroupBy {
  try {
    const v = localStorage.getItem(BY_KEY) as GroupBy | null;
    return v && GROUPINGS.includes(v) ? v : "project";
  } catch {
    return "project";
  }
}

/** The theme this browser picked last. Whether it's on offer waits for the
 * machine's features, which arrive after the first draw: it's kept until
 * then, not thrown away. */
function savedTheme(): Theme {
  try {
    const v = localStorage.getItem(THEME_KEY) as Theme | null;
    return v && ALL_THEMES.includes(v) ? v : "blocks";
  } catch {
    return "blocks";
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
  home,
}: {
  fleet: Fleet;
  back: () => void;
  /** The machine this page is of: its labs say whether the extra themes
   * are offered. */
  home: string | null;
  /** Opened from a notification: the pane to show, and its card. */
  focus?: { host: string; pane: number } | null;
}) {
  useSubscribe((fn) => fleet.subscribe(fn));
  useSubscribe((fn) => subscribeRunners(fn));
  useEffect(() => watchRunners(fleet), [fleet]);
  const phone = usePhone();
  const [by, setBy] = useState<GroupBy>(savedBy);
  const [chosen, setChosen] = useState<Theme>(savedTheme);
  // Read on each render: the features arrive after the first one.
  const themes = offeredThemes(!!home && !!fleet.clientOf(home)?.hasLabs());
  const theme = themes.includes(chosen) ? chosen : "blocks";
  const [peek, setPeek] = useState<{ key: string; x: number; y: number; text: string } | null>(null);
  const [hidden, setHidden] = useState<Set<string>>(new Set());
  const [answered, setAnswered] = useState<Done[]>([]);
  /** M28: the editor being followed, by its key. */
  const [following, setFollowing] = useState<string | null>(null);
  const [, tick] = useState(0);
  const canvas = useRef<HTMLCanvasElement>(null);
  const field = useRef<SwarmScene | null>(null);
  /** What the scene was last fed, for a city that loads after. */
  const fed = useRef<FieldPane[]>([]);
  const rail = useRef<HTMLDivElement>(null);
  const bar = useRef<HTMLDivElement>(null);
  const prev = useRef<Map<string, FleetPane>>(new Map());
  const firstSeen = useRef<Map<string, number>>(new Map());
  const peekCache = useRef<Map<string, { at: number; text: string }>>(new Map());
  // The field calls these; they see this render's panes.
  const handlers = useRef<{
    open(key: string): void;
    hover(key: string | null, x: number, y: number): Promise<void> | void;
    menu(key: string, e: MouseEvent): void;
  }>({
    open: () => {},
    hover: () => {},
    menu: () => {},
  });

  const panes = fleet.panes;
  const bundles = bundlesOf(panes, hidden);
  const cap = phone ? 4 : Math.max(2, Math.floor((innerHeight - 120) / 170));
  const shown = bundles.slice(0, cap);
  const waiting = bundles.slice(cap).reduce((n, b) => n + b.panes.length, 0);
  const onRail = new Map<string, string>();
  for (const b of shown) for (const p of b.panes) onRail.set(p.key, b.key);

  // The scene: the field (blocks) or the city (M41, loaded when picked),
  // made again when the theme changes, fed on every change.
  useLayoutEffect(() => {
    const hooks: FieldHooks = {
      railW: () => (innerWidth < 760 ? 0 : 330),
      railH: () => (innerWidth < 760 ? 196 : 0),
      top: () => (bar.current?.getBoundingClientRect().bottom ?? 60) + 10,
      cardRect: (b) => rail.current?.querySelector<HTMLElement>(`[data-bundle="${CSS.escape(b)}"]`)?.getBoundingClientRect() ?? null,
      open: (key) => {
        swallowClick();
        handlers.current.open(key);
      },
      hover: (key, x, y) => void handlers.current.hover(key, x, y),
      menu: (key, e) => handlers.current.menu(key, e),
      history: (sinceS) => historyOf(fleet, sinceS),
    };
    let scene: SwarmScene | null = null;
    let gone = false;
    const resize = () => scene?.resize();
    const begin = (s: SwarmScene) => {
      scene = s;
      field.current = s;
      s.set(fed.current);
      s.start();
      (window as unknown as { __swarm?: SwarmScene }).__swarm = s;
      tick((n) => n + 1);
    };
    // Everything but blocks is its own chunk, fetched when it's picked.
    const make: Promise<(cv: HTMLCanvasElement, h: FieldHooks) => SwarmScene> =
      theme === "city"
        ? import("./city").then(({ City }) => (cv, h) => new City(cv, h))
        : theme === "hive"
          ? import("./hive").then(({ Hive }) => (cv, h) => new Hive(cv, h))
          : theme === "timeline"
            ? import("./timeline").then(({ Timeline }) => (cv, h) => new Timeline(cv, h))
            : Promise.resolve((cv, h) => new Field(cv, h));
    void make
      .then((mk) => {
        if (!gone) begin(mk(canvas.current!, hooks));
      })
      .catch((e) => {
        console.error(`the ${theme} theme didn't load`, e);
        if (!gone) setChosen("blocks");
      });
    addEventListener("resize", resize);
    return () => {
      gone = true;
      scene?.stop();
      if (field.current === scene) field.current = null;
      removeEventListener("resize", resize);
    };
  }, [theme]);

  useEffect(() => {
    fed.current = panes.map((p): FieldPane => {
        const r = reasonOf(p);
        const me = fleet.meOn(p.host);
        const typing = !!(p.driver && p.info.typing);
        const people = (p.watchers ?? []).filter((w) => w.who !== me).map((w) => {
          const driving = p.driver?.who === w.who;
          return { name: w.name, driving, typing: driving && typing };
        });
        // #118: a driver who isn't looking shows only while they type.
        if (typing && p.driver!.who !== me && !people.some((x) => x.name === p.driver!.name)) people.push({ name: p.driver!.name, driving: true, typing: true });
        people.sort((a, b) => +b.typing - +a.typing || +b.driving - +a.driving);
        return {
          key: p.key,
          kind: kindOf(p),
          group: groupOf(p, by),
          act: activityOf(p),
          stale: p.stale,
          label: p.info.editor
            ? `%${p.id} ${p.info.file?.split("/").pop() ?? p.info.title ?? "editor"} · ${appName(p.info.editor.app)}`
            : `%${p.id} ${p.info.current?.text ?? p.info.command ?? p.info.title ?? kindOf(p)}`,
          where: p.host,
          lastOut: p.info.activity?.last_ms ?? 0,
          att: r ? { col: REASON_COL[r.kind], bundle: onRail.get(p.key) ?? null, since: r.since_ms } : null,
          id: p.id,
          sub: by === "machine" ? (p.info.project?.name ?? groupOf(p, "project")) : p.host,
          bps: p.stale ? 0 : (p.info.activity?.bps ?? 0),
          started: p.info.current && p.info.current.ended_ms == null ? p.info.current.started_ms : null,
          lastDur: p.info.last?.ended_ms != null ? p.info.last.ended_ms - p.info.last.started_ms : null,
          lastExit: p.info.last?.exit ?? null,
          lastEnded: p.info.last?.ended_ms ?? null,
          people,
          unread: p.unread,
          mention: p.mention,
        };
      });
    field.current?.set(fed.current);
  });

  useEffect(() => {
    field.current?.regroup();
  }, [by]);
  const pickTheme = (t: Theme) => {
    setChosen(t);
    setPeek(null);
    try {
      localStorage.setItem(THEME_KEY, t);
    } catch {
      // not remembered
    }
  };
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
      // take a follow-up. An agent in a terminal waits on its inbox only
      // once it stops, maybe after this update: its box shows then.
      const worth = a && (a.who !== fleet.meOn(p.host) || p.info.inbox || p.info.type === "agent" || p.info.kind === "agent");
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
    // An editor that joined has no tab: follow it.
    if (isPresence(p)) return setFollowing(key);
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
      const all = text.split("\n").map((l) => l.trimEnd()).filter(Boolean);
      // An editor's text is its file and the lines around its cursor (M27).
      const lines = (p.info.type === "editor" ? all.slice(0, 7) : all.slice(-6)).join("\n");
      peekCache.current.set(key, { at: Date.now(), text: lines });
      setPeek((pk) => (pk?.key === key ? { ...pk, text: lines } : pk));
    } catch {
      // stale host: no lines
    }
  };

  // Right-click a pane: open it, or (M27) VS Code where it runs.
  const paneMenu = (key: string, e: MouseEvent) => {
    const p = byKey(key);
    if (!p) return e.preventDefault();
    openMenu(e, [
      ...(isPresence(p) ? [] : [{ label: "Open", run: () => openKey(key) }]),
      ...(followable(p) ? [{ label: "Follow", run: () => setFollowing(key) }] : []),
      ...(canEdit(fleet, p) ? [{ label: "Open in editor", run: () => void editIn(fleet, p, back) }] : []),
      // M11: what changed in its project, beside it.
      ...(p.info.project && fleet.role(p) === "owner" && !p.stale && !isPresence(p) ? [{ label: "Changes", run: () => void changesOf(fleet, p, back) }] : []),
    ]);
  };

  handlers.current = { open: openKey, hover: hoverPane, menu: paneMenu };
  const machines = new Set(panes.map((p) => p.host)).size;
  const busy = panes.filter((p) => activityOf(p) > 0.3).length;
  const need = panes.filter((p) => reasonOf(p)).length;
  const clusters = field.current?.clusters ?? [];
  // M45b: a machine that is the account's Fountain runner says so.
  const fountainRunners = runners();

  return (
    <div class={`swarm${phone ? " phone" : ""}`} data-swarm={by} data-theme={theme}>
      <canvas
        key={theme}
        ref={canvas}
        class={`swarm-field swarm-${theme}`}
        aria-label={ARIA[theme]}
      />
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
          {fountainRunners.map(([host, r]) => (
            <div key={host} class={`swarm-runner${r.problem ? " needs" : ""}`} data-stat="fountain-runner" data-runner={r.name} title={r.problem ?? `${host}: ${runnerLabel(r)}`}>
              <b>{r.name}</b>Fountain runner {r.online === true ? "online" : r.online === false ? "offline" : "…"}
            </div>
          ))}
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
        {themes.length > 1 && (
          <div>
            <div class="swarm-seg-l">Theme</div>
            <div class="swarm-seg" role="group" aria-label="Theme">
              {themes.map((t) => (
                <button key={t} data-theme-pick={t} aria-pressed={t === theme} onClick={() => pickTheme(t)}>
                  {t}
                </button>
              ))}
            </div>
          </div>
        )}
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
            <Card
              key={b.key}
              b={b}
              fleet={fleet}
              focus={focus}
              show={() => field.current?.diveTo(b.panes[0].key)}
              open={() => openKey(b.panes[0].key)}
              follow={() => setFollowing(b.panes[0].key)}
              back={back}
            />
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
          {theme !== "blocks" && <ThemeKey theme={theme} />}
          <div class="swarm-hint">{HINT[theme][phone ? 1 : 0]}</div>
        </div>
        <div class="swarm-clusters" hidden>
          {clusters.map((c) => (
            <span key={c.name} data-cluster={c.name} data-n={c.n} />
          ))}
        </div>
      </div>

      {following && <FollowView fleet={fleet} pkey={following} close={() => setFollowing(null)} back={back} />}
      <MenuLayer />
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

const ARIA: Record<Theme, string> = {
  blocks: "Every pane, clustered",
  city: "Every pane as a building, each cluster a block",
  hive: "Every pane as a cell, each cluster a comb",
  timeline: "Every pane as a lane of its commands over the last 40 minutes",
};

/** What to do with each theme: [laptop, phone]. */
const HINT: Record<Theme, [string, string]> = {
  blocks: ["Scroll to zoom. Drag to pan. Click a cluster name to dive in, a pane to open it.", "Pinch to zoom, drag to pan. Tap a pane to open it."],
  city: [
    "Drag to orbit, right-drag to pan, scroll to zoom. Click a block's name to fly to it, a building to open it.",
    "Drag to orbit, pinch to zoom. Tap a building to open it.",
  ],
  hive: ["Scroll to zoom. Drag to pan. Click a cell to open its pane.", "Pinch to zoom, drag to pan. Tap a cell to open its pane."],
  timeline: ["Drag to scroll back or down, scroll to zoom. Click a lane to open its pane.", "Drag to scroll, pinch to zoom. Tap a lane to open its pane."],
};

/** How to read a theme (M41, M42): what each thing it draws means. */
const KEYS: Record<Exclude<Theme, "blocks">, [string, string][]> = {
  city: [
    ["Height", "How long its command has run (log scale). Keeps the last command's height when done."],
    ["Lit roof", "Still running. Only things that finish get one."],
    ["Windows", "Scrolling: printing now, faster for more. Still: printed lately. Dark: quiet."],
    ["Shape", "Box finishes · drum runs until stopped · hexagon agent · pentagon editor · slab pull request or issue"],
    ["Red roof", "Its last command failed."],
    ["Beam", "Needs you. Taller the longer it waits."],
    ["Marker", "A teammate has it open; a cone while they type."],
    ["Lots", "One per pane; rows are machines, in pane order. Only a regroup moves them."],
    ["Greyed", "Its machine isn't connected."],
  ],
  hive: [
    ["Comb", "A cluster. Cells spiral out from the middle by machine, in pane order. Only a regroup moves them."],
    ["Fill", "How long its command has run (log scale, full at an hour). Bright while running, faded once done."],
    ["Hatched", "Runs until stopped: a server, log tail, studio app or editor (or a pull request or issue)."],
    ["Edge", "Pulsing: printing now, faster for more. Faint: printed lately. Dark: quiet."],
    ["Red rim", "Its last command failed."],
    ["Overflow", "Needs you. The cell takes the reason's colour and says how long; its glow spills wider the longer it waits."],
    ["Ring", "A teammate has it open; dashed and turning while they type."],
    ["Greyed", "Its machine isn't connected."],
  ],
  timeline: [
    ["Lane", "One pane, under its cluster. Now is the right edge; the last 40 minutes fit."],
    ["Bar", "A command: as long as it ran, coloured by kind. From each machine's history."],
    ["Stripes", "What it printed: denser stripes, more bytes a second."],
    ["Lit edge", "Still running: the bar is still growing. Its last four minutes are shaded by its output."],
    ["Red cap", "That command failed."],
    ["Thin line", "Runs until stopped: a server, log tail, studio app or editor."],
    ["Band", "Needs you: from when it started waiting until now, with how long past the now edge."],
    ["Dot", "A teammate has it open; ringed while they type, faintly while they drive it. A grey lane: its machine isn't connected."],
  ],
};

function ThemeKey({ theme }: { theme: Exclude<Theme, "blocks"> }) {
  return (
    <details class="swarm-key" data-city-key={theme === "city" ? "" : undefined} data-theme-key={theme}>
      <summary>How to read the {theme}</summary>
      <dl>
        {KEYS[theme].map(([t, d]) => [<dt key={`t${t}`}>{t}</dt>, <dd key={`d${t}`}>{d}</dd>])}
      </dl>
    </details>
  );
}

/** A daemon's history entry (`GET /api/history`). */
interface HistoryEntry {
  pane: number;
  open: boolean;
  text: string | null;
  exit: number | null;
  started_ms: number;
  ended_ms: number | null;
  start: number;
  end: number | null;
  host?: string;
}

/** Commands that finished in the last `sinceS` seconds on every connected
 * host, for the timeline (M42). */
async function historyOf(fleet: Fleet, sinceS: number): Promise<HistoryRun[]> {
  const out: HistoryRun[] = [];
  await Promise.all(
    fleet.list
      .filter((h) => h.state === "connected")
      .map(async (h) => {
        try {
          const res = await fleet.request(h.name, "GET", `/api/history?since=${Math.round(sinceS)}&limit=2000`);
          if (!res.ok) return;
          for (const e of await res.json<HistoryEntry[]>()) {
            if (!e.open || e.host || e.ended_ms == null) continue;
            out.push({ key: `${h.name}:${e.pane}`, text: e.text, started: e.started_ms, ended: e.ended_ms, exit: e.exit, bytes: e.end != null ? Math.max(0, e.end - e.start) : 0 });
          }
        } catch {
          // gone since: its lanes keep what they had
        }
      }),
  );
  return out;
}

/** Whether VS Code can be opened where a pane runs (M27): a terminal's, and
 * only by the host's owner (editor blocks are, like ports). */
function canEdit(fleet: Fleet, p: FleetPane) {
  return (p.info.type ?? "terminal") === "terminal" && fleet.role(p) === "owner" && !p.stale;
}

/** Open VS Code in a pane's directory, on its machine, and show it. */
async function editIn(fleet: Fleet, p: FleetPane, back: () => void): Promise<string | null> {
  try {
    const res = await fleet.request(p.host, "POST", "/api/blocks", { type: "editor", config: {}, from_pane: p.id } satisfies OpenRequest);
    const v = await res.json<Partial<OpenResponse> & { error?: string }>().catch(() => null);
    if (!res.ok || v?.block === undefined) return v?.error ?? `couldn't (${res.status})`;
    back();
    fleet.open(p.host, v.block);
    return null;
  } catch (e) {
    return String(e);
  }
}

/** What changed in a pane's project (M11): a diff block beside it, shown. */
async function changesOf(fleet: Fleet, p: FleetPane, back: () => void): Promise<string | null> {
  try {
    const res = await fleet.request(p.host, "POST", "/api/blocks", { type: "diff", config: {}, from_pane: p.id, split: p.id } satisfies OpenRequest);
    const v = await res.json<Partial<OpenResponse> & { error?: string }>().catch(() => null);
    if (!res.ok || v?.block === undefined) return v?.error ?? `couldn't (${res.status})`;
    back();
    fleet.open(p.host, v.block);
    return null;
  } catch (e) {
    return String(e);
  }
}

/** Act on a bundle: one request per host, naming its panes. */
async function act(fleet: Fleet, panes: FleetPane[], action: Action, extra: Partial<ActRequest> = {}): Promise<string | null> {
  const hosts = new Map<string, FleetPane[]>();
  for (const p of panes) hosts.set(p.host, [...(hosts.get(p.host) ?? []), p]);
  let err: string | null = null;
  await Promise.all(
    [...hosts].map(async ([host, ps]) => {
      const r = ps[0].info.reason;
      // What it answers: a question or approval's id, or a gate's key (M34).
      const id = r?.ask?.id ?? (r?.gate ? gateKey(r.gate) : undefined);
      const body: ActRequest = ps.length === 1 ? { action, pane: ps[0].id, id, ...extra } : { action, panes: ps.map((p) => p.id), ...extra };
      try {
        const res = await fleet.request(host, "POST", "/api/attention/act", body);
        if (!res.ok) err = (await res.json<Partial<ActResponse> & { error?: string }>().catch(() => null))?.error ?? `couldn't (${res.status})`;
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
  // M30: who has each pane open.
  const people = panes.flatMap((p) => (p.watchers ?? []).filter((x) => x.who !== fleet.meOn(p.host)));
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

function Card({
  b,
  fleet,
  focus,
  show,
  open,
  follow,
  back,
}: {
  b: Bundle;
  fleet: Fleet;
  focus?: { host: string; pane: number } | null;
  show: () => void;
  open: () => void;
  follow: () => void;
  back: () => void;
}) {
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
  const kind = r.kind === "exited" || r.kind === "errors" ? "failed" : r.kind === "diff" ? "ask" : r.kind;
  const diff = n === 1 && r.kind === "diff" ? first.info.diff : null;
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
      {diff ? null : ask?.kind === "permission" ? (
        <PermissionBody ask={ask} />
      ) : (
        <div class="cq">
          {r.headline}
          {r.command && r.kind !== "ask" && r.kind !== "gate" && !r.headline.includes(r.command) ? <code> {r.command}</code> : null}
        </div>
      )}
      {diff ? (
        <DiffCard
          key={diff.id}
          diff={diff}
          can={can}
          act={(action, extra) => run(action, { ...extra, id: diff.id })}
          full={async () => {
            const res = await fleet.request(first.host, "GET", `/api/panes/${first.id}/diff`);
            return res.ok ? (await res.json<{ new: string }>()).new : null;
          }}
        />
      ) : !can ? (
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
          {r.actions.includes("rerun") && (
            <button class="pri" data-rerun disabled={busy} onClick={() => void run("rerun")}>
              {all("Rerun")}
            </button>
          )}
          {r.actions.includes("continue") && (
            <button class="pri" data-continue disabled={busy} onClick={() => void run("continue")}>
              {all("Continue")}
            </button>
          )}
          {r.actions.includes("allow") && (
            <button class="pri" data-approve-gate={r.kind === "gate" ? "" : undefined} disabled={busy} onClick={() => void run("allow")}>
              {all(r.kind === "gate" ? "Approve" : "Allow")}
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
        {!isPresence(first) && (
          <button class="ghost" onClick={open}>
            Open
          </button>
        )}
        {n === 1 && followable(first) && (
          <button class="ghost" data-follow-card onClick={follow}>
            Follow
          </button>
        )}
        <button class="ghost" onClick={show}>
          Show
        </button>
        {n === 1 && canEdit(fleet, first) && (
          <button class="ghost" data-edit onClick={() => void editIn(fleet, first, back).then(setErr)}>
            Edit
          </button>
        )}
        {can && (r.kind === "ask" || r.kind === "diff") && (
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

/** A tap on a tile opens its pane and closes the swarm on pointerup; the
 * browser's click for that tap comes after. WebKit (iOS Safari) sends it to
 * whatever is under the finger by then, such as the tab view's "Switch to"
 * button for the host being left. Drop that one click. */
function swallowClick() {
  const drop = (e: Event) => {
    e.preventDefault();
    e.stopPropagation();
  };
  document.addEventListener("click", drop, { capture: true, once: true });
  setTimeout(() => document.removeEventListener("click", drop, { capture: true }), 500);
}
