// Huddles (M63) on screen: a headphones button where a session is named
// (the bar, the chat view's channel header), a chip with who's in a huddle
// (the chat view's channel list), and the huddle bar, which stays put
// while you move around: who's in, who's talking, mute and leave.
//
// The call itself is ../call.ts.

import { useEffect, useState } from "preact/hooks";
import type { Client } from "../client";
import { activeHuddle, dismissHuddle, joinHuddle, leaveHuddle, onHuddle, unsupported, type Huddle, type Trust } from "../call";
import { CALL_MAX, type CallMember, type SessionId } from "../proto";
import { Avatar } from "./people";
import type { MenuItem } from "./menu";

function useHuddle(): Huddle | null {
  const [, setTick] = useState(0);
  useEffect(() => onHuddle(() => setTick((t) => t + 1)), []);
  return activeHuddle();
}

/** Whether this page is in the huddle on `session` of `client`'s daemon. */
function inHere(h: Huddle | null, client: Client, session: SessionId): boolean {
  return !!h && h.live() && h.client === client && h.session === session;
}

/** Why `client` can't join a huddle on `session`, if it can't. */
function cantJoin(client: Client, session: SessionId): string | null {
  if (!client.hasCalls()) return "This machine's illogical is too old for huddles.";
  const call = client.call(session);
  if (call && call.members.length >= CALL_MAX) return `This huddle is full (${CALL_MAX} people).`;
  return unsupported();
}

export function Headphones({ size = 14 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
      <path d="M2.5 10V8a5.5 5.5 0 0 1 11 0v2" />
      <rect x="1.75" y="9.5" width="3" height="4.5" rx="1" fill="currentColor" stroke="none" />
      <rect x="11.25" y="9.5" width="3" height="4.5" rx="1" fill="currentColor" stroke="none" />
    </svg>
  );
}

function Mic({ off }: { off?: boolean }) {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
      <rect x="5.5" y="1.5" width="5" height="8" rx="2.5" />
      <path d="M3 7.5a5 5 0 0 0 10 0M8 12.5v2" />
      {off && <path d="M2 2l12 12" />}
    </svg>
  );
}

/** Start, join or show the huddle on a session. `label` for a roomier
 * spot (the chat view's channel header); the bar has just the icon and
 * who's in. */
export function HuddleButton({ client, session, label }: { client: Client; session: SessionId; label?: boolean }) {
  const h = useHuddle();
  if (!client.hasCalls()) return null;
  const call = client.call(session);
  const here = inHere(h, client, session);
  const why = here ? null : cantJoin(client, session);
  const n = call?.members.length ?? 0;
  const title = here
    ? "You're in this huddle"
    : (why ?? (call ? `Join the huddle (${call.members.map((m) => m.name).join(", ")})` : "Start a huddle in this session"));
  return (
    <button
      class={`huddle-button${call ? " live" : ""}${here ? " here" : ""}`}
      title={title}
      disabled={!!why}
      data-huddle={session}
      onClick={() => !here && joinHuddle(client, session)}
    >
      <Headphones />
      {label && <span>{here ? "In huddle" : call ? "Join huddle" : "Huddle"}</span>}
      {n > 0 && <span class="huddle-count">{n}</span>}
    </button>
  );
}

/** Who's in the huddle on a session, small: for lists of sessions. */
export function HuddleChip({ client, session }: { client: Client; session: SessionId }) {
  const call = client.call(session);
  if (!call) return null;
  return (
    <span class="huddle-chip" title={`Huddle: ${call.members.map((m) => m.name).join(", ")}`}>
      <Headphones size={12} />
      {call.members.slice(0, 3).map((m) => (
        <Avatar key={m.client} p={m} />
      ))}
      {call.members.length > 3 && <span class="huddle-more">+{call.members.length - 3}</span>}
    </span>
  );
}

/** For a session's menu (and the palette). */
export function huddleItems(client: Client, session: SessionId): MenuItem[] {
  if (!client.hasCalls()) return [];
  const h = activeHuddle();
  if (inHere(h, client, session)) return [{ label: "Leave the huddle", run: leaveHuddle }];
  const call = client.call(session);
  const why = cantJoin(client, session);
  return [
    {
      label: call ? `Join the huddle (${call.members.length})` : "Start a huddle",
      run: () => (why ? client.toast(why) : joinHuddle(client, session)),
    },
  ];
}

const TRUST: Record<Trust, string> = {
  verified: "Verified: signed by one of your own devices",
  signed: "Signed by their device, as their machine vouches for it",
  unverified: "Not verified: they're connected without a device key (tailnet or local)",
  refused: "Refused: their signature didn't match their device",
};

function Member({ h, m }: { h: Huddle; m: CallMember }) {
  const me = m.client === h.client.clientId;
  const peer = me ? undefined : h.peer(m.client);
  const speaking = me ? h.speakingSelf() : !!peer?.speaking && !m.muted;
  const connecting = !me && peer && peer.state !== "connected";
  const trust = peer?.trust;
  const title = [
    me ? `${m.name} (you)` : m.name,
    m.muted ? "muted" : null,
    connecting ? (peer.state === "failed" ? "can't connect" : "connecting…") : null,
    trust ? TRUST[trust] : null,
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    <span
      class={`huddle-member${speaking ? " speaking" : ""}${connecting ? " connecting" : ""}${trust ? ` ${trust}` : ""}`}
      title={title}
      data-member={m.client}
      data-speaking={speaking ? "1" : undefined}
    >
      <Avatar p={m} />
      {m.muted && (
        <span class="huddle-muted">
          <Mic off />
        </span>
      )}
    </span>
  );
}

/** The huddle this page is in, wherever you are. */
export function HuddleBar() {
  const h = useHuddle();
  const [, setTick] = useState(0);
  useEffect(() => h?.subscribe(() => setTick((t) => t + 1)), [h]);
  useEffect(() => (h ? h.client.subscribe(() => setTick((t) => t + 1)) : undefined), [h]);
  // Ctrl/Cmd+Shift+Space mutes and unmutes, as in other huddles.
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      const a = activeHuddle();
      if (e.code === "Space" && e.shiftKey && (e.ctrlKey || e.metaKey) && a?.live()) {
        e.preventDefault();
        a.setMuted(!a.muted);
      }
    };
    addEventListener("keydown", key, true);
    return () => removeEventListener("keydown", key, true);
  }, []);
  if (!h) return null;
  const session = h.client.state?.sessions.find((s) => s.id === h.session);
  const call = h.call();
  const st = h.status;
  if (st.kind === "ended" || st.kind === "error") {
    return (
      <div class={`huddle-bar ended${st.kind === "error" ? " error" : ""}`} role="status">
        <Headphones />
        <span class="huddle-why">{st.why}</span>
        {session && st.kind === "ended" && (
          <button class="huddle-act" onClick={() => joinHuddle(h.client, h.session)}>
            {call ? "Join again" : "Start again"}
          </button>
        )}
        <button class="huddle-act" title="Close" onClick={dismissHuddle}>
          ✕
        </button>
      </div>
    );
  }
  const members = call?.members ?? [];
  return (
    <div class="huddle-bar" role="region" aria-label="Huddle">
      <span class="huddle-title">
        <Headphones />
        <span>{session?.name ?? "Huddle"}</span>
      </span>
      <span class="huddle-members">
        {st.kind === "joining" && !members.some((m) => m.client === h.client.clientId) ? (
          <span class="huddle-why">Joining…</span>
        ) : (
          members.map((m) => <Member key={m.client} h={h} m={m} />)
        )}
      </span>
      {h.background && <span class="huddle-why">Muted while the app is in the background</span>}
      <button
        class={`huddle-act${h.muted ? " on" : ""}`}
        title={h.muted ? "Unmute (Ctrl+Shift+Space)" : "Mute (Ctrl+Shift+Space)"}
        aria-pressed={h.muted}
        data-huddle-mute
        onClick={() => h.setMuted(!h.muted)}
      >
        <Mic off={h.muted} />
      </button>
      <button class="huddle-act leave" title="Leave the huddle" data-huddle-leave onClick={leaveHuddle}>
        Leave
      </button>
    </div>
  );
}
