// Other people in a session (M13): who's here and where they look, who
// drives each pane, handing control over, following someone, and sharing a
// session.

import { useEffect, useState } from "preact/hooks";
import type { Client } from "../client";
import type { PaneId, Presence, Role, SessionId } from "../proto";
import type { MenuItem } from "./menu";

/** A steady colour per person. */
export function colorOf(who: string): string {
  let h = 0;
  for (const c of who) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return `hsl(${h % 360} 70% 68%)`;
}

function initials(name: string): string {
  const base = name.split("@")[0];
  const parts = base.split(/[._\s-]+/).filter(Boolean);
  return ((parts[0]?.[0] ?? "?") + (parts[1]?.[0] ?? "")).toUpperCase();
}

export function Avatar({ p, onClick, title }: { p: Pick<Presence, "who" | "name" | "pic">; onClick?: () => void; title?: string }) {
  return (
    <span class="avatar" style={{ "--who": colorOf(p.who) }} title={title ?? p.name} data-who={p.who} onClick={onClick}>
      {p.pic ? <img src={p.pic} alt="" referrerpolicy="no-referrer" /> : initials(p.name)}
    </span>
  );
}

/** One entry per person (their first client), not per window. */
function people(list: Presence[]): Presence[] {
  const seen = new Set<string>();
  return list.filter((p) => !seen.has(p.who) && seen.add(p.who));
}

/** Top bar: everyone else in this session. Click to follow them. */
export function PeopleBar({ client }: { client: Client }) {
  const tabs = new Set(client.state?.sessions.find((s) => s.id === client.session)?.tabs ?? []);
  const here = people(client.others().filter((p) => p.tab !== undefined && tabs.has(p.tab)));
  if (!here.length) return null;
  const following = client.state?.presence?.find((p) => p.client === client.following);
  return (
    <div class="people">
      {here.map((p) => (
        <Avatar
          key={p.who}
          p={p}
          title={following?.who === p.who ? `Following ${p.name} (click to stop)` : `${p.name}: click to follow`}
          onClick={() => client.follow(following?.who === p.who ? null : p.client)}
        />
      ))}
      {following ? <span class="following">following {following.name}</span> : null}
    </div>
  );
}

/** On a tab: who else is looking at it. */
export function TabPeople({ client, tab }: { client: Client; tab: number }) {
  const here = people(client.others().filter((p) => p.tab === tab));
  if (!here.length) return null;
  return (
    <span class="tab-people">
      {here.map((p) => (
        <span key={p.who} class="tab-dot" style={{ background: colorOf(p.who) }} title={p.name} />
      ))}
    </span>
  );
}

/** On a pane: an outline in the colour of each person focused on it, and
 * who drives it if that's someone else. */
export function PaneMarks({ client, pane }: { client: Client; pane: PaneId }) {
  const focused = people(client.others().filter((p) => p.pane === pane));
  const driver = client.drivenBy(pane);
  const pair = client.info(pane)?.pair;
  if (!focused.length && !driver && !pair) return null;
  return (
    <>
      {focused.length ? <div class="pane-outline" style={{ "--who": colorOf(focused[0].who) }} /> : null}
      <div class="pane-people">
        {focused.map((p) => (
          <span key={p.who} class="pane-person" style={{ "--who": colorOf(p.who) }}>
            {p.name.split("@")[0]}
          </span>
        ))}
        {driver ? (
          <span class="pane-driver" data-driver={driver.who} title={`${driver.name} is driving: only their typing reaches it`}>
            ✎ {driver.name.split("@")[0]}
          </span>
        ) : pair ? (
          <span class="pane-driver" title="Pair mode: everyone types">
            ✎ pair
          </span>
        ) : null}
      </div>
    </>
  );
}

/** Pane menu: driving it. */
export function driveItems(client: Client, pane: PaneId): MenuItem[] {
  const info = client.info(pane);
  if (!info || info.type !== "terminal") return [];
  const others = client.others().length > 0;
  const driver = client.drivenBy(pane);
  const mine = info.driver?.who === client.me();
  const items: MenuItem[] = [];
  if (driver) {
    items.push({ label: `Take control (from ${driver.name.split("@")[0]})`, run: () => client.paneOp(pane, { op: "take_control" }) });
    items.push({ label: "Ask for control", run: () => client.paneOp(pane, { op: "request_control" }) });
  } else if (mine && others) {
    items.push({ label: "Let go of control", run: () => client.paneOp(pane, { op: "release_control" }) });
  }
  if (others || info.pair) {
    items.push({ label: "Pair mode (everyone types)", checked: !!info.pair, run: () => client.paneOp(pane, { op: "set_pair", on: !info.pair }) });
  }
  return items.length ? ["separator", ...items] : [];
}

/** Someone asks to drive a pane you drive. */
export function ControlRequests({ client }: { client: Client }) {
  const r = client.requests[0];
  if (!r) return null;
  return (
    <div class="prompt-backdrop">
      <div class="prompt" data-control-request={r.pane}>
        <p>
          <b>{r.name}</b> asks to drive %{r.pane}.
        </p>
        <div class="prompt-buttons">
          <button onClick={() => client.answerRequest(r.pane, false)}>Not now</button>
          <button class="primary" data-give onClick={() => client.answerRequest(r.pane, true)}>
            Hand over
          </button>
        </div>
      </div>
    </div>
  );
}

interface Grant {
  session: SessionId;
  principal: string;
  name: string;
  role: Role;
  from?: Record<string, number>;
}

let openShare: ((s: SessionId) => void) | null = null;

export function shareSession(session: SessionId) {
  openShare?.(session);
}

/** Share a session (M13): who has access, add someone, revoke. */
export function ShareDialog({ client }: { client: Client }) {
  const [session, setSession] = useState<SessionId | null>(null);
  const [grants, setGrants] = useState<Grant[]>([]);
  const [who, setWho] = useState("");
  const [role, setRole] = useState<Role>("viewer");
  const [history, setHistory] = useState(false);
  const [err, setErr] = useState("");
  const load = async () => {
    const r = await client.request("GET", "/api/acl");
    if (r.ok) setGrants((await r.json<{ grants: Grant[] }>()).grants);
  };
  useEffect(() => {
    openShare = (s) => {
      setSession(s);
      setErr("");
      void load();
    };
    return () => {
      openShare = null;
    };
  }, [client]);
  if (session === null) return null;
  const name = client.state?.sessions.find((s) => s.id === session)?.name ?? `$${session}`;
  const mine = grants.filter((g) => g.session === session);
  const set = async (principal: string, r: Role | null, withHistory = true) => {
    const res = await client.request("POST", "/api/acl", { session, principal, role: r, history: withHistory });
    if (!res.ok) setErr((await res.json<{ error?: string }>().catch(() => null))?.error ?? `HTTP ${res.status}`);
    await load();
  };
  return (
    <div class="prompt-backdrop" onClick={(e) => e.target === e.currentTarget && setSession(null)}>
      <div class="prompt share-dialog" data-share={session}>
        <h2>Share {name}</h2>
        {mine.length ? (
          <ul class="share-list">
            {mine.map((g) => (
              <li key={g.principal} data-grant={g.principal}>
                <Avatar p={{ who: g.principal, name: g.name }} />
                <span>{g.name}</span>
                <select value={g.role} onChange={(e) => void set(g.principal, (e.target as HTMLSelectElement).value as Role)}>
                  <option value="viewer">can watch</option>
                  <option value="editor">can drive</option>
                  <option value="owner">owner</option>
                </select>
                <span class="dim">{g.from ? "from now" : "with history"}</span>
                <button class="control-revoke" onClick={() => void set(g.principal, null)}>
                  Remove
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <p class="dim">Only you can reach it.</p>
        )}
        <form
          class="share-add"
          onSubmit={(e) => {
            e.preventDefault();
            const p = who.trim();
            if (!p) return;
            void set(p.includes(":") ? p : `tailnet:${p}`, role, history).then(() => setWho(""));
          }}
        >
          <input placeholder="their tailnet login" value={who} onInput={(e) => setWho((e.target as HTMLInputElement).value)} aria-label="Who" />
          <select value={role} onChange={(e) => setRole((e.target as HTMLSelectElement).value as Role)} aria-label="Role">
            <option value="viewer">can watch</option>
            <option value="editor">can drive</option>
          </select>
          <label class="share-history">
            <input type="checkbox" checked={history} onChange={(e) => setHistory((e.target as HTMLInputElement).checked)} /> with history
          </label>
          <button type="submit" class="primary">
            Share
          </button>
        </form>
        {err ? <p class="control-error">{err}</p> : null}
        <div class="prompt-buttons">
          <button onClick={() => setSession(null)}>Done</button>
        </div>
      </div>
    </div>
  );
}
