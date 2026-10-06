// Other people in a session (M13): who's here and where they look, who
// drives each pane, handing control over, following someone, and sharing a
// session.

import { useEffect, useState } from "preact/hooks";
import type { Client } from "../client";
import type { InviteRequest, Invited, PaneId, Presence, Role, SessionId } from "../proto";
import { roleLabel } from "./roles";
import type { MenuItem } from "./menu";
import type { ControlSession } from "../control";
import { fingerprint } from "../e2e/cert.ts";
import { CopyText } from "./copy";

/** Control mode (M19): people are accounts there, and links go through it. */
let control: ControlSession | null = null;
export const getControlSession = () => control;
export function setControlSession(s: ControlSession | null) {
  control = s;
}

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
  if (client.info(pane)?.private && client.state?.roles) {
    return <div class="pane-private">Private: only its owner sees it</div>;
  }
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

/** Pane menu: driving it, trusting, privacy (M13, M14). */
export function driveItems(client: Client, pane: PaneId): MenuItem[] {
  const info = client.info(pane);
  if (!info || info.type !== "terminal") return [];
  const owner = !client.state?.roles;
  const extra: MenuItem[] = [];
  if (owner) {
    extra.push({ label: "Private (only you see it)", checked: !!info.private, run: () => client.paneOp(pane, { op: "set_private", on: !info.private }) });
    for (const [who] of info.trusted ?? []) {
      extra.push({ label: `Stop trusting ${who.split(":").pop()?.split("@")[0]}`, run: () => client.paneOp(pane, { op: "revoke_trust", to: who }) });
    }
  } else if (client.role() !== "viewer" && !client.mayType(pane)) {
    extra.push({ label: "Ask the owner to let me drive it", run: () => client.paneOp(pane, { op: "request_trust" }) });
  }
  return [...driveItemsInner(client, pane, info), ...(extra.length ? ["separator" as const, ...extra] : [])];
}

function driveItemsInner(client: Client, pane: PaneId, info: NonNullable<ReturnType<Client["info"]>>): MenuItem[] {
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

/** A guest asks the owner to trust them with a pane on this machine. */
function TrustRequest({ client }: { client: Client }) {
  const r = client.trustRequests[0];
  const [minutes, setMinutes] = useState(30);
  if (!r) return null;
  return (
    <div class="prompt-backdrop">
      <div class="prompt" data-trust-request={r.pane}>
        <p>
          <b>{r.name}</b> asks to drive %{r.pane}. It runs on <b>this machine</b>, as you: they could do anything you can here.
        </p>
        <label class="share-history">
          For
          <select value={minutes} onChange={(e) => setMinutes(Number((e.target as HTMLSelectElement).value))}>
            <option value={10}>10 minutes</option>
            <option value={30}>30 minutes</option>
            <option value={120}>2 hours</option>
          </select>
        </label>
        <div class="prompt-buttons">
          <button onClick={() => client.answerTrust(r.pane, r.who, null)}>Not now</button>
          <button class="primary" data-trust onClick={() => client.answerTrust(r.pane, r.who, minutes)}>
            Allow
          </button>
        </div>
      </div>
    </div>
  );
}

/** Someone asks to drive a pane you drive. */
export function ControlRequests({ client }: { client: Client }) {
  if (client.trustRequests.length) return <TrustRequest client={client} />;
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
  const [secrets, setSecrets] = useState<{ pane: PaneId; kinds: string[] }[]>([]);
  // Control mode: the person found, to confirm by fingerprint (and then
  // to notify, if that's what was asked).
  const [found, setFound] = useState<{ account: string; name: string; root: string; notify?: boolean } | null>(null);
  // An invite (#233): what the notification says, and how it went.
  const [note, setNote] = useState("");
  const [told, setTold] = useState<{ delivery: string; text: string } | null>(null);
  const [link, setLink] = useState<string | null>(null);
  const load = async (s: SessionId | null = session) => {
    const r = await client.request("GET", "/api/acl");
    if (r.ok) setGrants((await r.json<{ grants: Grant[] }>()).grants);
    if (s !== null) {
      const x = await client.request("GET", `/api/sessions/${s}/secrets`);
      if (x.ok) setSecrets(await x.json());
    }
  };
  useEffect(() => {
    openShare = (s) => {
      setSession(s);
      setErr("");
      void load(s);
    };
    return () => {
      openShare = null;
    };
  }, [client]);
  if (session === null) return null;
  const name = client.state?.sessions.find((s) => s.id === session)?.name ?? `$${session}`;
  const mine = grants.filter((g) => g.session === session);
  const set = async (principal: string, r: Role | null, withHistory = true, extra: Record<string, string> = {}) => {
    const res = await client.request("POST", "/api/acl", { session, principal, role: r, history: withHistory, ...extra });
    if (!res.ok) setErr((await res.json<{ error?: string }>().catch(() => null))?.error ?? `HTTP ${res.status}`);
    await load();
  };
  // Share and notify (#233): the machine grants and pushes them alone. A
  // name it doesn't know is looked up on control and checked by its
  // fingerprint first, as for a plain share.
  const invite = async (whom: string, extra: Record<string, string> = {}) => {
    setErr("");
    setTold(null);
    const res = await client.request("POST", "/api/invite", { session, who: whom, role, history, note, ...extra } satisfies InviteRequest);
    const body = await res.json<Partial<Invited> & { error?: string }>().catch(() => null);
    if (res.status === 404 && control && client.e2e && !extra.root) {
      control.person(whom).then((p) => setFound({ ...p, notify: true }), (x: Error) => setErr(x.message));
      return;
    }
    if (!res.ok || !body?.delivery) {
      setErr(body?.error ?? `HTTP ${res.status}`);
      return;
    }
    const n = body.grant?.name ?? whom;
    const why = body.reason ? `: ${body.reason}` : "";
    const text = body.delivery === "sent" ? `${n} was notified` : body.delivery === "pending" ? `${n} will be notified${why}` : `${n} wasn't notified${why}`;
    setTold({ delivery: body.delivery, text });
    setWho("");
    setNote("");
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
                  <option value="viewer">{roleLabel("viewer")}</option>
                  <option value="editor">{roleLabel("editor")}</option>
                  <option value="owner">{roleLabel("owner")}</option>
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
        {secrets
          .filter((x) => !client.info(x.pane)?.private)
          .map((x) => (
            <p key={x.pane} class="share-warning" data-secret={x.pane}>
              ⚠ %{x.pane} shows what looks like {x.kinds.join(" and ")} (a guess).{" "}
              <button
                class="control-linkish"
                onClick={() => {
                  client.paneOp(x.pane, { op: "set_private", on: true });
                  setSecrets(secrets.filter((y) => y.pane !== x.pane));
                }}
              >
                Make it private
              </button>
            </p>
          ))}
        <p class="dim" data-share-note>
          {client.has("vms")
            ? "People you share with open their own panes on throwaway VMs; they can't type on this machine unless you trust them with a pane."
            : "People you share with see its panes but can't open their own here, and can't type on this machine unless you trust them with a pane."}
        </p>
        {found ? (
          <div class="share-confirm" data-found={found.account}>
            <p>
              <b>{found.name}</b>'s first device is <span class="fingerprint">{fingerprint(found.root)}</span>. If you can, check it with them.
            </p>
            <div class="prompt-buttons">
              <button onClick={() => setFound(null)}>Cancel</button>
              <button
                class="primary"
                data-share-confirm
                onClick={() =>
                  void (
                    found.notify
                      ? invite(`account:${found.account}`, { root: found.root })
                      : set(`account:${found.account}`, role, history, { root: found.root, name: found.name })
                  ).then(() => {
                    setFound(null);
                    setWho("");
                  })
                }
              >
                Share with {found.name}
                {found.notify ? " and notify" : ""}
              </button>
            </div>
          </div>
        ) : null}
        {control && client.e2e
          ? control.teams
              .filter((t) => t.verified && t.role !== null && !mine.some((g) => g.principal === `team:${t.team}`))
              .map((t) => (
                <p key={t.team} class="share-team">
                  <button
                    data-share-team={t.team}
                    onClick={() =>
                      // Pinned to its founder, as this browser pinned the
                      // team: the machine checks every roster back to them.
                      void set(`team:${t.team}`, role, history, { root: `${t.pin.founder}.${t.pin.founder_root}`, name: t.roster.name })
                    }
                  >
                    Share with everyone in {t.roster.name}
                  </button>{" "}
                  <span class="dim">(as members come and go)</span>
                </p>
              ))
          : null}
        <form
          class="share-add"
          onSubmit={(e) => {
            e.preventDefault();
            const p = who.trim();
            if (!p) return;
            if (control && client.e2e) {
              control.person(p).then(setFound, (x: Error) => setErr(x.message));
              return;
            }
            void set(p.includes(":") ? p : `tailnet:${p}`, role, history).then(() => setWho(""));
          }}
        >
          <input
            placeholder={control && client.e2e ? "their name or login on illogical" : "their tailnet login"}
            value={who}
            onInput={(e) => setWho((e.target as HTMLInputElement).value)}
            aria-label="Who"
          />
          <select value={role} onChange={(e) => setRole((e.target as HTMLSelectElement).value as Role)} aria-label="Role">
            <option value="viewer">{roleLabel("viewer")}</option>
            <option value="editor">{roleLabel("editor")}</option>
          </select>
          <label class="share-history">
            <input type="checkbox" checked={history} onChange={(e) => setHistory((e.target as HTMLInputElement).checked)} /> with history
          </label>
          <button type="submit" class="primary">
            Share
          </button>
          <input
            class="share-note"
            placeholder="a note, to notify them"
            value={note}
            onInput={(e) => setNote((e.target as HTMLInputElement).value)}
            aria-label="Note"
            data-invite-note
          />
          <button
            type="button"
            data-invite
            onClick={() => {
              const p = who.trim();
              if (!p) return;
              // A login is a tailnet one here; a name, someone the machine knows.
              void invite(control && client.e2e ? p : p.includes("@") && !p.includes(":") ? `tailnet:${p}` : p);
            }}
          >
            Share and notify
          </button>
        </form>
        {told ? (
          <p class="dim" data-invite-delivery={told.delivery}>
            {told.text}
          </p>
        ) : null}
        {control && client.e2e ? (
          <p>
            <button
              class="control-linkish"
              data-make-link
              onClick={() =>
                control!
                  .makeLink((m, p, b) => client.request(m, p, b), client.e2e!.daemon.id, session, 3600, false)
                  .then(setLink, (x: Error) => setErr(x.message))
              }
            >
              Make a read-only link
            </button>{" "}
            <span class="dim">(anyone with it watches, from now on, for an hour; no account needed)</span>
          </p>
        ) : null}
        {link ? <CopyText text={link} share data-link /> : null}
        {err ? <p class="control-error">{err}</p> : null}
        <div class="prompt-buttons">
          <button onClick={() => setSession(null)}>Done</button>
        </div>
      </div>
    </div>
  );
}
