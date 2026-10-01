// Mirror of crates/proto/src/lib.rs. Keep in step.

export type PaneId = number;
export type ClientId = number;

export interface PaneInfo {
  id: PaneId;
  epoch: number;
  cols: number;
  rows: number;
}

export type ClientMsg =
  | { type: "attach"; panes: { pane: PaneId; offset: number | null }[] }
  | { type: "resize"; pane: PaneId; cols: number; rows: number };

export type ServerMsg =
  | { type: "hello"; version: string; client: ClientId; panes: PaneInfo[] }
  | { type: "size"; pane: PaneId; cols: number; rows: number; owner: ClientId | null }
  | { type: "resync"; pane: PaneId }
  | { type: "exit"; pane: PaneId; code: number | null };

export const enum FrameKind {
  Output = 1,
  Snapshot = 2,
  Input = 3,
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
