// Mirror of crates/proto/src/lib.rs and the types it re-exports from
// crates/core. Keep in step.

export type PaneId = number;
export type TabId = number;
export type SessionId = number;
export type NodeId = number;
export type ClientId = number;

export type Dir = "row" | "column";
export type Edge = "left" | "right" | "top" | "bottom" | "center";

export type Node =
  | { type: "pane"; pane: PaneId }
  | { type: "split"; id: NodeId; dir: Dir; children: { weight: number; node: Node }[] };

export interface Rect {
  x: number;
  y: number;
  cols: number;
  rows: number;
}

export interface SplitRect {
  id: NodeId;
  dir: Dir;
  rect: Rect;
  extents: number[];
}

export interface Layout {
  panes: [PaneId, Rect][];
  splits: SplitRect[];
}

export interface Session {
  id: SessionId;
  name: string;
  tabs: TabId[];
}

export interface TabView {
  id: TabId;
  name: string | null;
  root: Node;
  cols: number;
  rows: number;
  owner: ClientId | null;
  zoom: PaneId | null;
  layout: Layout;
}

export interface PaneInfo {
  id: PaneId;
  epoch: number;
  cwd: string | null;
}

export interface State {
  rev: number;
  sessions: Session[];
  tabs: TabView[];
  panes: PaneInfo[];
}

export type Intent =
  | { op: "new_session"; name: string | null; from_pane: PaneId | null }
  | { op: "rename_session"; session: SessionId; name: string }
  | { op: "close_session"; session: SessionId }
  | { op: "new_tab"; session: SessionId; from_pane: PaneId | null }
  | { op: "rename_tab"; tab: TabId; name: string | null }
  | { op: "close_tab"; tab: TabId }
  | { op: "move_tab"; tab: TabId; session: SessionId; index: number }
  | { op: "split"; pane: PaneId; edge: Edge }
  | { op: "close_pane"; pane: PaneId }
  | { op: "move_pane"; pane: PaneId; target: PaneId; edge: Edge }
  | { op: "break_pane"; pane: PaneId; session: SessionId; index: number | null }
  | { op: "dock_tab"; tab: TabId; target: PaneId; edge: Edge }
  | { op: "resize_split"; split: NodeId; weights: number[] };

export type ClientMsg =
  | { type: "attach"; panes: { pane: PaneId; offset: number | null }[] }
  | { type: "detach"; panes: PaneId[] }
  | { type: "view"; tab: TabId; cols: number; rows: number; zoom: PaneId | null; claim: boolean }
  | { type: "intent"; id: number | null; intent: Intent };

export type ServerMsg =
  | { type: "hello"; version: string; client: ClientId; state: State }
  | { type: "state"; state: State }
  | { type: "size"; pane: PaneId; cols: number; rows: number }
  | { type: "resync"; pane: PaneId }
  | { type: "error"; id: number | null; message: string };

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
