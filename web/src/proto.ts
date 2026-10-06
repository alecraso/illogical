// The wire types are generated from crates/proto (proto.gen.ts, `just
// proto-ts`). This file adds the code that goes with them, and the HTTP
// API's types that aren't in proto yet (#387).

import type { ClientId, Gate, PaneId, ThreadTarget } from "./proto.gen";

export type * from "./proto.gen";
export { CALL_MAX } from "./proto.gen";

/** What names a gate among its block's: `member/op/gate`. */
export function gateKey(g: Gate): string {
  return `${g.member}/${g.op}/${g.gate}`;
}

/** What a huddle member signs with its device key: the call, who from and
 * to, and the SDP's `a=fingerprint:` lines (the daemon's
 * `call_fingerprint_body`). */
export function callFingerprintBody(call: string, from: ClientId, to: ClientId, sdp: string): string {
  const fps = sdp
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l.startsWith("a=fingerprint:"));
  return `illogical call v1\ncall ${call}\nfrom ${from}\nto ${to}\n${fps.join("\n")}\n`;
}

/** `pane-7` / `session-2`: a thread's name in its API path. */
export function threadKey(t: ThreadTarget): string {
  return "pane" in t ? `pane-${t.pane}` : `session-${t.session}`;
}

/** An `@` in a post that reached no one: told to the poster alone. */
export interface Unreached {
  token: string;
  why: "agent_needs_pane" | "may_not_drive" | "nobody";
}

/** Someone the owner's `@token` named who can't read the thread (#297):
 *  theirs to invite. Only ever in the owner's own response. */
export interface Invitable {
  token: string;
  /** `tailnet:<login>` or `account:<id>`. */
  who: string;
  name: string;
  /** Another principal taken to be them (a login by their name). */
  merged?: string | null;
}

export const enum FrameKind {
  Output = 1,
  Snapshot = 2,
  Input = 3,
  /** A snapshot compressed with zstd (for an attach with `zstd`). */
  SnapshotZstd = 4,
}

export interface Frame {
  kind: FrameKind;
  pane: PaneId;
  offset: number;
  data: Uint8Array;
}

const HEADER_LEN = 13;

export function encodeFrame(kind: FrameKind, pane: PaneId, data: Uint8Array, offset = 0): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(HEADER_LEN + data.length);
  const view = new DataView(out.buffer);
  view.setUint8(0, kind);
  view.setUint32(1, pane);
  view.setBigUint64(5, BigInt(offset));
  out.set(data, HEADER_LEN);
  return out;
}

export function decodeFrame(buf: ArrayBuffer): Frame {
  if (buf.byteLength < HEADER_LEN) throw new Error(`frame too short: ${buf.byteLength}`);
  const view = new DataView(buf);
  return {
    kind: view.getUint8(0) as FrameKind,
    pane: view.getUint32(1),
    // Offsets stay far below 2^53 (8 PB of output), so Number is exact.
    offset: Number(view.getBigUint64(5)),
    data: new Uint8Array(buf, HEADER_LEN),
  };
}
