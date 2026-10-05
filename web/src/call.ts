// Huddles (M63): a voice call on a session, peer to peer.
//
// The daemon keeps who's in each huddle (State.calls) and passes WebRTC
// descriptions between members; audio goes straight between devices,
// encrypted end to end by DTLS-SRTP, through TURN when it has to.
//
// - One huddle at a time per page. It belongs to one Client (one daemon),
//   so it keeps going while the page shows other tabs, sessions or the
//   chat view.
// - Whoever joins later offers to everyone already in; the others answer.
//   Descriptions are sent whole once ICE has gathered (no trickle).
// - Each description carries a signature of its `a=fingerprint:` lines by
//   the sender's device key, and the daemon passes the sender's device
//   certificate with it. A peer that has a device but whose signature
//   doesn't check is refused: that's a daemon (or anyone in the path)
//   trying to sit in the middle. A peer with no device key (a tailnet or
//   local connection) is let in and marked unverified.
// - Someone whose share is revoked is dropped from State.calls by the
//   daemon; everyone hangs up on them, and they hang up themselves.

import type { Client } from "./client";
import { checkForm, verify, type Cert } from "./e2e/cert.ts";
import { signText } from "./e2e/keys.ts";
import { callFingerprintBody, type Call, type CallMember, type CallSignal, type ClientId, type SessionId } from "./proto";
import { getControlSession } from "./ui/people";
import { native, nativeCalls, NativePeer } from "./call-native";

/** How sure we are a peer is who the daemon says.
 * - `verified`: its fingerprints are signed by a device this browser's
 *   account trusts itself (one of your own devices).
 * - `signed`: signed by the device the daemon vouches for (another
 *   account's).
 * - `unverified`: it has no device key (tailnet or local).
 * - `refused`: its signature didn't check; no audio either way. */
export type Trust = "verified" | "signed" | "unverified" | "refused";

export interface PeerView {
  client: ClientId;
  state: RTCPeerConnectionState | "waiting";
  trust?: Trust;
  speaking: boolean;
}

export type Status =
  | { kind: "joining" }
  | { kind: "live" }
  /** It ended for us: `why` says how, and we may join again. `rejoin`:
   * the connection dropped, and we join again when it's back. */
  | { kind: "ended"; why: string; rejoin?: boolean }
  | { kind: "error"; why: string };

/** Why huddles can't run here, if they can't. */
export function unsupported(): string | null {
  if (nativeCalls()) return null;
  if (typeof RTCPeerConnection === "undefined") {
    return /Linux/.test(navigator.userAgent) && document.documentElement.dataset.desktop === "linux"
      ? "The Linux app can't make calls yet (its web view has no WebRTC). Join from a browser for now."
      : "This browser has no WebRTC, so it can't join huddles.";
  }
  if (!navigator.mediaDevices?.getUserMedia) {
    return isSecureContext ? "This browser can't use a microphone." : "Huddles need a secure page (https) for the microphone.";
  }
  return null;
}

const SPEAKING_RMS = 0.02;
const SPEAKING_HOLD_MS = 300;
const GATHER_MS = 4000;

interface Peer {
  pc: RTCPeerConnection;
  /** In the Linux app, `pc` is this (its levels come from the app). */
  native?: NativePeer;
  audio: HTMLAudioElement;
  analyser?: AnalyserNode;
  trust?: Trust;
  speakingUntil: number;
  /** We offer to them (we joined later). */
  offerer: boolean;
  /** When we last (re)started the connection, for restarts after a failure. */
  started: number;
}

type Listener = () => void;

export class Huddle {
  status: Status = { kind: "joining" };
  muted = false;
  /** iOS stops the mic while the page is in the background (S30). */
  background = false;
  private callId: string | null = null;
  private stream: MediaStream | null = null;
  private ice: RTCIceServer[] = [];
  private peers = new Map<ClientId, Peer>();
  private ctx: AudioContext | null = null;
  private mine?: AnalyserNode;
  private speakingSelfUntil = 0;
  private listeners = new Set<Listener>();
  private offs: (() => void)[] = [];
  private timer = 0;
  private seenSelf = false;
  private members: CallMember[] = [];
  private sink: HTMLElement;
  /** The Linux app runs the call (call-native.ts). */
  private readonly native = nativeCalls();
  private nativeMe = 0;
  private polling = false;

  constructor(
    readonly client: Client,
    readonly session: SessionId,
  ) {
    this.sink = document.createElement("div");
    this.sink.hidden = true;
    this.sink.className = "huddle-audio";
    document.body.append(this.sink);
  }

  // ---- what the UI reads

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private emit() {
    for (const fn of this.listeners) fn();
  }

  /** The huddle as the daemon has it. */
  call(): Call | undefined {
    return this.client.call(this.session);
  }

  peer(client: ClientId): PeerView | undefined {
    const p = this.peers.get(client);
    if (!p) return undefined;
    return { client, state: p.pc.connectionState, trust: p.trust, speaking: p.speakingUntil > performance.now() };
  }

  /** Whether this page's own mic is picking up speech. */
  speakingSelf(): boolean {
    return !this.muted && this.speakingSelfUntil > performance.now();
  }

  live(): boolean {
    return this.status.kind === "joining" || this.status.kind === "live";
  }

  // ---- joining and leaving

  async join() {
    this.status = { kind: "joining" };
    this.seenSelf = false;
    this.emit();
    const why = unsupported();
    if (why) return this.fail(why);
    try {
      if (this.native) await native.start();
      else this.stream = await this.mic();
    } catch (e) {
      if (this.native) return this.fail(`The app couldn't start the call: ${(e as Error)?.message ?? e}`);
      const name = (e as DOMException)?.name;
      return this.fail(
        name === "NotAllowedError"
          ? "The microphone was refused. Allow it for this page and join again."
          : name === "NotFoundError"
            ? "There's no microphone."
            : `The microphone didn't start: ${(e as Error).message}`,
      );
    }
    this.ctx = new AudioContext();
    void this.ctx.resume();
    this.watchMine();
    this.ice = await this.iceServers();
    if (!this.live()) return; // left meanwhile
    this.offs.push(this.client.subscribe(() => this.reconcile()));
    this.offs.push(this.client.onCallSignal((m) => m.session === this.session && void this.onSignal(m.from, m.signal, m.cert)));
    const visible = () => this.onVisibility();
    document.addEventListener("visibilitychange", visible);
    this.offs.push(() => document.removeEventListener("visibilitychange", visible));
    this.timer = window.setInterval(() => this.tick(), 100);
    this.client.send({ type: "call_join", session: this.session });
    // The daemon answers with a state that has us in it, or an error.
    window.setTimeout(() => {
      if (this.status.kind === "joining" && !this.seenSelf) this.fail("The machine didn't let you into the huddle.");
    }, 8000);
  }

  leave() {
    if (this.live()) this.client.send({ type: "call_leave", session: this.session });
    this.end({ kind: "ended", why: "You left the huddle." });
  }

  setMuted(muted: boolean) {
    this.muted = muted;
    if (this.native) void native.mute(muted);
    for (const t of this.stream?.getAudioTracks() ?? []) t.enabled = !muted;
    this.client.send({ type: "call_mute", session: this.session, muted });
    this.emit();
  }

  private fail(why: string) {
    if (this.live() && this.seenSelf) this.client.send({ type: "call_leave", session: this.session });
    this.end({ kind: "error", why });
  }

  private end(status: Status) {
    this.status = status;
    window.clearInterval(this.timer);
    for (const off of this.offs.splice(0)) off();
    for (const id of [...this.peers.keys()]) this.drop(id);
    for (const t of this.stream?.getTracks() ?? []) t.stop();
    this.stream = null;
    if (this.native) void native.stop();
    void this.ctx?.close().catch(() => {});
    this.ctx = null;
    this.callId = null;
    this.members = [];
    this.emit();
  }

  dispose() {
    if (this.live()) this.leave();
    this.sink.remove();
  }

  private async mic(): Promise<MediaStream> {
    return navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
    });
  }

  private async iceServers(): Promise<RTCIceServer[]> {
    try {
      const r = await this.client.request("GET", "/api/turn");
      if (r.ok) return (await r.json<{ ice_servers: RTCIceServer[] }>()).ice_servers ?? [];
    } catch {
      // STUN-less still works on one network.
    }
    return [];
  }

  // ---- keeping in step with the daemon

  private reconcile() {
    if (!this.live()) return;
    if (!this.client.connected) {
      // The daemon forgets us when we disconnect (and all huddles when it
      // restarts): this one's over, and we come back when it is.
      if (!this.seenSelf) return this.fail("The connection to the machine dropped.");
      chime(this.ctx, "leave");
      return this.end({ kind: "ended", why: "The connection to the machine dropped. Rejoining when it's back…", rejoin: true });
    }
    const me = this.client.clientId;
    const call = this.call();
    const in_ = call?.members.some((m) => m.client === me);
    if (!call || !in_) {
      if (!this.seenSelf) {
        return; // not there yet

      }
      chime(this.ctx, "leave");
      const why = call ? "You were taken out of the huddle (your access changed)." : "The huddle ended.";
      return this.end({ kind: "ended", why });
    }
    if (!this.seenSelf) {
      this.seenSelf = true;
      this.status = { kind: "live" };
      chime(this.ctx, "join");
    }
    if (this.callId !== call.id) {
      for (const id of [...this.peers.keys()]) this.drop(id);
      this.callId = call.id;
    }
    const mine = call.members.findIndex((m) => m.client === me);
    const before = new Set(this.members.map((m) => m.client));
    this.members = call.members;
    for (const [i, m] of call.members.entries()) {
      if (m.client === me || this.peers.has(m.client)) continue;
      if (before.size && !before.has(m.client)) chime(this.ctx, "join");
      // Whoever joined later offers.
      void this.connect(m.client, i < mine);
    }
    for (const id of [...this.peers.keys()]) {
      if (!call.members.some((m) => m.client === id)) {
        chime(this.ctx, "leave");
        this.drop(id);
      }
    }
    this.emit();
  }

  private member(client: ClientId): CallMember | undefined {
    return this.members.find((m) => m.client === client);
  }

  // ---- peers

  private async connect(to: ClientId, offerer: boolean) {
    const nat = this.native ? new NativePeer(to, this.ice) : undefined;
    const pc = nat ? (nat as unknown as RTCPeerConnection) : new RTCPeerConnection({ iceServers: this.ice });
    const audio = document.createElement("audio");
    audio.autoplay = true;
    audio.setAttribute("playsinline", "");
    this.sink.append(audio);
    const peer: Peer = { pc, native: nat, audio, speakingUntil: 0, offerer, started: performance.now() };
    this.peers.set(to, peer);
    for (const t of this.stream?.getAudioTracks() ?? []) pc.addTrack(t, this.stream!);
    pc.ontrack = (e) => {
      const stream = e.streams[0] ?? new MediaStream([e.track]);
      audio.srcObject = stream;
      void audio.play().catch(() => {});
      if (this.ctx) {
        peer.analyser = this.ctx.createAnalyser();
        peer.analyser.fftSize = 512;
        this.ctx.createMediaStreamSource(stream).connect(peer.analyser);
      }
    };
    pc.onconnectionstatechange = () => {
      if (pc.connectionState === "failed" && peer.offerer && this.peers.get(to) === peer) {
        // Try once more from scratch (a network change, a TURN hiccup).
        if (performance.now() - peer.started > 5000) {
          this.drop(to);
          void this.connect(to, true);
        }
      }
      this.emit();
    };
    if (offerer) {
      const offer = await pc.createOffer();
      await this.describe(to, pc, offer);
    }
  }

  private drop(id: ClientId) {
    const p = this.peers.get(id);
    if (!p) return;
    this.peers.delete(id);
    p.pc.close();
    p.audio.srcObject = null;
    p.audio.remove();
  }

  /** Set and send our description, signed, once ICE has gathered. */
  private async describe(to: ClientId, pc: RTCPeerConnection, desc: RTCSessionDescriptionInit) {
    await pc.setLocalDescription(desc);
    if (pc.iceGatheringState !== "complete") {
      await new Promise<void>((done) => {
        const t = window.setTimeout(done, GATHER_MS);
        pc.addEventListener("icegatheringstatechange", () => {
          if (pc.iceGatheringState === "complete") {
            window.clearTimeout(t);
            done();
          }
        });
      });
    }
    const local = pc.localDescription;
    const me = this.client.clientId;
    if (!local || me === null || !this.callId || this.peers.get(to)?.pc !== pc) return;
    const signal: CallSignal = { type: local.type as "offer" | "answer", sdp: local.sdp };
    const keys = this.client.deviceKeys();
    if (keys) signal.sig = await signText(keys, callFingerprintBody(this.callId, me, to, local.sdp));
    this.client.send({ type: "call_signal", session: this.session, to, signal });
  }

  private async onSignal(from: ClientId, signal: CallSignal, cert: unknown) {
    if (!this.live() || !this.callId) return;
    const trust = await this.check(from, signal, cert as Cert | undefined);
    let peer = this.peers.get(from);
    if (trust === "refused") {
      if (peer) peer.trust = "refused";
      const name = this.member(from)?.name ?? "someone";
      this.client.toast(`Refused ${name}'s audio: its signature didn't match their device. The machine may be tampering with the huddle.`);
      if (peer) peer.pc.close();
      this.emit();
      return;
    }
    if (signal.type === "offer") {
      // They offer: answer on a fresh connection (theirs restarted).
      if (peer && (peer.offerer || peer.pc.signalingState !== "stable" || peer.pc.remoteDescription)) this.drop(from);
      if (!this.peers.has(from)) await this.connect(from, false);
      peer = this.peers.get(from)!;
      peer.trust = trust;
      await peer.pc.setRemoteDescription({ type: "offer", sdp: signal.sdp });
      await this.describe(from, peer.pc, await peer.pc.createAnswer());
    } else if (peer && peer.pc.signalingState === "have-local-offer") {
      peer.trust = trust;
      await peer.pc.setRemoteDescription({ type: "answer", sdp: signal.sdp });
    }
    this.emit();
  }

  /** Check a description against the member the daemon listed: its device
   * must have signed these fingerprints, for this call, from them to us. */
  private async check(from: ClientId, signal: CallSignal, cert: Cert | undefined): Promise<Trust> {
    const m = this.member(from);
    const me = this.client.clientId;
    if (!m || me === null || !this.callId) return "refused";
    if (!m.device) return cert || signal.sig ? "refused" : "unverified";
    if (!cert || !signal.sig || cert.device !== m.device || !(await checkForm(cert))) return "refused";
    if (!(await verify(cert.sign, callFingerprintBody(this.callId, from, me, signal.sdp), signal.sig))) return "refused";
    // One of our own account's devices, as this browser checked them itself.
    const own = getControlSession()?.trusted.get(cert.device);
    return own && own.sign === cert.sign ? "verified" : "signed";
  }

  // ---- levels, the background, restarts

  private watchMine() {
    if (!this.ctx || !this.stream) return;
    this.mine = this.ctx.createAnalyser();
    this.mine.fftSize = 512;
    this.ctx.createMediaStreamSource(this.stream).connect(this.mine);
  }

  private tick() {
    const now = performance.now();
    let changed = false;
    if (this.native) this.pollNative();
    const loud = (p?: Peer) =>
      !!p && (p.native ? (this.nativeLevels.get(p.native.id) ?? 0) : p.analyser ? rms(p.analyser) : 0) > SPEAKING_RMS;
    for (const p of this.peers.values()) {
      const was = p.speakingUntil > now;
      if (loud(p)) p.speakingUntil = now + SPEAKING_HOLD_MS;
      changed ||= was !== p.speakingUntil > now;
    }
    const was = this.speakingSelfUntil > now;
    const me = this.native ? this.nativeMe : this.mine ? rms(this.mine) : 0;
    if (!this.muted && me > SPEAKING_RMS) this.speakingSelfUntil = now + SPEAKING_HOLD_MS;
    changed ||= was !== this.speakingSelfUntil > now;
    // iOS ends or mutes the mic track while the page is hidden.
    const track = this.stream?.getAudioTracks()[0];
    const background = !this.native && document.hidden && (!track || track.readyState === "ended" || track.muted);
    if (background !== this.background) {
      this.background = background;
      changed = true;
    }
    if (changed) this.emit();
  }

  private nativeLevels = new Map<ClientId, number>();

  /** The app's levels and connection states (one request in flight). */
  private pollNative() {
    if (this.polling) return;
    this.polling = true;
    native
      .status()
      .then((st) => {
        this.nativeMe = st.me;
        this.nativeLevels = new Map(st.peers.map((p) => [p.id, p.level]));
        for (const p of st.peers) this.peers.get(p.id)?.native?.update(p.state);
      })
      .catch(() => {})
      .finally(() => (this.polling = false));
  }

  /** Back in the foreground: a fresh mic track if the old one died (iOS
   * gives it back without its echo cancellation otherwise, S30). */
  private async onVisibility() {
    if (document.hidden || !this.live() || !this.stream) return;
    void this.ctx?.resume();
    const old = this.stream.getAudioTracks()[0];
    if (old && old.readyState === "live" && !old.muted) return;
    try {
      const fresh = await this.mic();
      const track = fresh.getAudioTracks()[0];
      track.enabled = !this.muted;
      for (const p of this.peers.values()) {
        for (const s of p.pc.getSenders()) if (s.track?.kind === "audio" || !s.track) await s.replaceTrack(track);
      }
      for (const t of this.stream.getTracks()) t.stop();
      this.stream = fresh;
      this.watchMine();
      this.background = false;
      this.emit();
    } catch {
      this.client.toast("The microphone didn't come back. Leave and join the huddle again.");
    }
  }
}

function rms(a: AnalyserNode): number {
  const buf = new Float32Array(a.fftSize);
  a.getFloatTimeDomainData(buf);
  let s = 0;
  for (const x of buf) s += x * x;
  return Math.sqrt(s / buf.length);
}

/** A soft two-note sound when someone joins (up) or leaves (down). */
function chime(ctx: AudioContext | null, kind: "join" | "leave") {
  if (!ctx || ctx.state !== "running") return;
  const notes = kind === "join" ? [660, 880] : [660, 440];
  const t0 = ctx.currentTime;
  notes.forEach((f, i) => {
    const o = ctx.createOscillator();
    const g = ctx.createGain();
    o.type = "sine";
    o.frequency.value = f;
    const t = t0 + i * 0.12;
    g.gain.setValueAtTime(0, t);
    g.gain.linearRampToValueAtTime(0.06, t + 0.02);
    g.gain.exponentialRampToValueAtTime(0.0001, t + 0.18);
    o.connect(g).connect(ctx.destination);
    o.start(t);
    o.stop(t + 0.2);
  });
}

// ---- the one huddle this page is in

let active: Huddle | null = null;
const listeners = new Set<Listener>();

export function activeHuddle(): Huddle | null {
  return active;
}

/** Changes to which huddle is active (and to it). */
export function onHuddle(fn: Listener): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

const changed = () => listeners.forEach((fn) => fn());

/** Join the huddle on `session` through `client` (leaving any other). */
export function joinHuddle(client: Client, session: SessionId) {
  if (active && active.client === client && active.session === session && active.live()) return;
  active?.dispose();
  const h = new Huddle(client, session);
  active = h;
  h.subscribe(changed);
  h.subscribe(() => rejoinLater(h));
  changed();
  void h.join();
}

const REJOIN_MS = 2 * 60_000;

/** After a dropped connection: join again once the daemon's back (a
 * restart, a network change), if it's soon and nobody moved on. */
function rejoinLater(h: Huddle) {
  const st = h.status;
  if (st.kind !== "ended" || !st.rejoin || (h as { rejoining?: boolean }).rejoining) return;
  (h as { rejoining?: boolean }).rejoining = true;
  const until = Date.now() + REJOIN_MS;
  const off = h.client.subscribe(() => {
    if (active !== h || Date.now() > until) return off();
    if (!h.client.connected || !h.client.state?.sessions.some((s) => s.id === h.session)) return;
    off();
    joinHuddle(h.client, h.session);
  });
}

export function leaveHuddle() {
  active?.leave();
  changed();
}

/** Put away an ended huddle's bar. */
export function dismissHuddle() {
  if (active?.live()) return;
  active?.dispose();
  active = null;
  changed();
}
