// Huddles in the Linux desktop app (M63): its WebKitGTK has no WebRTC, so
// the app runs each call in Rust (crates/desktop/src/calls.rs) and this
// stands in for the parts of RTCPeerConnection that call.ts uses. The page
// still signals, signs and checks fingerprints; descriptions come back
// from Rust already gathered.

import { desktopApp } from "./desktop";

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const t = (globalThis as { __TAURI__?: { core?: { invoke?: Invoke } } }).__TAURI__;
  if (!t?.core?.invoke) return Promise.reject(new Error("the app's calls aren't reachable from this page"));
  return t.core.invoke<T>(cmd, args);
}

// One at a time, in order: a peer dropped and made again must not have its
// drop land after the new one.
let chain: Promise<unknown> = Promise.resolve();
function serial<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const next = chain.then(
    () => invoke<T>(cmd, args),
    () => invoke<T>(cmd, args),
  );
  chain = next.catch(() => {});
  return next;
}

/** Whether this page runs huddles through the app (no WebRTC of its own). */
export function nativeCalls(): boolean {
  return typeof RTCPeerConnection === "undefined" && !!(desktopApp() as { nativeCalls?: boolean } | null)?.nativeCalls;
}

export interface NativeStatus {
  me: number;
  peers: { id: number; state: string; level: number }[];
}

export const native = {
  start: () => serial<void>("call_native_start"),
  mute: (muted: boolean) => invoke<void>("call_native_mute", { muted }).catch(() => {}),
  stop: () => serial<void>("call_native_stop").catch(() => {}),
  status: () => invoke<NativeStatus>("call_native_status"),
};

/** One huddle member's connection, run by the app. */
export class NativePeer {
  connectionState: RTCPeerConnectionState = "new";
  iceGatheringState: RTCIceGatheringState = "complete";
  signalingState: RTCSignalingState = "stable";
  localDescription: RTCSessionDescriptionInit | null = null;
  remoteDescription: RTCSessionDescriptionInit | null = null;
  onconnectionstatechange: (() => void) | null = null;
  ontrack: unknown = null;
  private created = false;
  private answer: string | null = null;
  private closed = false;

  constructor(
    readonly id: number,
    private ice: RTCIceServer[],
  ) {}

  addTrack() {}
  getSenders(): RTCRtpSender[] {
    return [];
  }
  addEventListener() {}

  async createOffer(): Promise<RTCSessionDescriptionInit> {
    const sdp = await serial<string | null>("call_native_peer", { id: this.id, ice: this.ice, offer: true });
    this.created = true;
    return { type: "offer", sdp: sdp ?? "" };
  }

  async setLocalDescription(d: RTCSessionDescriptionInit) {
    this.localDescription = d;
    this.signalingState = d.type === "offer" ? "have-local-offer" : "stable";
  }

  async setRemoteDescription(d: RTCSessionDescriptionInit) {
    if (!this.created) {
      await serial("call_native_peer", { id: this.id, ice: this.ice, offer: false });
      this.created = true;
    }
    this.answer = await serial<string | null>("call_native_remote", { id: this.id, kind: d.type, sdp: d.sdp });
    this.remoteDescription = d;
    this.signalingState = d.type === "offer" ? "have-remote-offer" : "stable";
  }

  async createAnswer(): Promise<RTCSessionDescriptionInit> {
    if (!this.answer) throw new Error("no offer to answer");
    return { type: "answer", sdp: this.answer };
  }

  /** From the app's status: its connection state. */
  update(state: string) {
    if (this.closed || state === this.connectionState) return;
    this.connectionState = state as RTCPeerConnectionState;
    this.onconnectionstatechange?.();
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.connectionState = "closed";
    void serial("call_native_drop", { id: this.id }).catch(() => {});
  }
}
