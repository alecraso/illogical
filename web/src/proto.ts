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

export type Policy =
  | { kind: "none" }
  | { kind: "shell" }
  | { kind: "rerun"; confirm: boolean }
  | { kind: "hook"; command: string };

export type Attention = "idle" | "working" | "needs_input" | "done";

export interface CommandInfo {
  text: string | null;
  cwd: string | null;
  exit: number | null;
  started_ms: number;
  ended_ms: number | null;
  start: number;
  end: number | null;
  /** Who typed it (M13). */
  by?: string;
}

/** Who drives a pane (M13). */
export interface Driver {
  /** Principal id: `owner`, `tailnet:<login>`, `account:<id>`. */
  who: string;
  name: string;
}

/** Someone connected (M13): one per client. */
export interface Presence {
  client: ClientId;
  who: string;
  name: string;
  pic?: string;
  tab?: TabId;
  pane?: PaneId;
}

export interface PaneInfo {
  id: PaneId;
  epoch: number;
  cwd: string | null;
  /** The foreground command, when it isn't the shell. */
  command: string | null;
  /** False while a restored pane waits for Enter. */
  running: boolean;
  policy: Policy;
  current: CommandInfo | null;
  last: CommandInfo | null;
  attention: Attention;
  /** Shell integration for shells started in this pane. */
  integration: boolean;
  type: BlockType;
  /** The machine it runs on; null is the daemon's host. */
  host: MachineId | null;
  /** A question Claude Code asks in this terminal (through its hook), drawn
   * as a card beside it (M6c). */
  ask?: import("./blocks/ask").Ask | null;
  /** M13: who drives it; absent when nobody does yet. */
  driver?: Driver;
  /** Pair mode: every editor types at once. */
  pair?: boolean;
}

export type BlockType = "terminal" | "browser" | "agent";
export type MachineId = number;
export type MachineState = "starting" | "running" | "gone";

/** A throwaway VM a pane runs on, deleted when the pane closes. */
export interface Machine {
  id: MachineId;
  provider: string;
  sprite: string;
  /** What to call it ("drifting cedar"): ours have one, a borrowed
   * sandbox goes by its sprite's name. */
  name?: string | null;
  image: string | null;
  owner: { pane: PaneId } | { tab: TabId };
  state: MachineState;
  /** Someone else's sandbox, borrowed for a shell with no daemon there
   * (M4b): disposable, and left alone when the pane closes. */
  borrowed?: boolean;
}

export type PaneOp =
  | { op: "set_policy"; policy: Policy }
  | { op: "purge" }
  | { op: "set_integration"; on: boolean }
  | { op: "attention"; state: Attention }
  | { op: "take_control" }
  | { op: "request_control" }
  | { op: "give_control"; to: string }
  | { op: "release_control" }
  | { op: "set_pair"; on: boolean };

/** Clients' named options (tmux `@` options), per scope. */
export interface Options {
  global?: Record<string, string>;
  sessions?: [SessionId, Record<string, string>][];
  tabs?: [TabId, Record<string, string>][];
  panes?: [PaneId, Record<string, string>][];
}

export type OptionScope =
  | { kind: "global" }
  | { kind: "session"; id: SessionId }
  | { kind: "tab"; id: TabId }
  | { kind: "pane"; id: PaneId };

export interface State {
  rev: number;
  sessions: Session[];
  tabs: TabView[];
  panes: PaneInfo[];
  machines: Machine[];
  options?: Options;
  /** M12: for someone who isn't the daemon's owner, their role in each
   * session they see, as [session, role] pairs. Absent for the owner. */
  roles?: [SessionId, Role][];
  /** M13: who else is here, and where they look. */
  presence?: Presence[];
}

export type Role = "viewer" | "editor" | "owner";

export type Intent =
  | { op: "new_session"; name: string | null; from_pane: PaneId | null }
  | { op: "rename_session"; session: SessionId; name: string }
  | { op: "close_session"; session: SessionId }
  | { op: "new_tab"; session: SessionId; from_pane: PaneId | null; cwd?: string }
  | { op: "rename_tab"; tab: TabId; name: string | null }
  | { op: "close_tab"; tab: TabId }
  | { op: "move_tab"; tab: TabId; session: SessionId; index: number }
  | { op: "split"; pane: PaneId; edge: Edge; local?: boolean; cwd?: string }
  | { op: "close_pane"; pane: PaneId }
  | { op: "move_pane"; pane: PaneId; target: PaneId; edge: Edge }
  | { op: "break_pane"; pane: PaneId; session: SessionId; index: number | null }
  | { op: "dock_tab"; tab: TabId; target: PaneId; edge: Edge }
  | { op: "resize_split"; split: NodeId; weights: number[] }
  | { op: "set_option"; scope: OptionScope; name: string; value: string | null };

export type ClientMsg =
  | { type: "attach"; panes: { pane: PaneId; offset: number | null }[] }
  | { type: "detach"; panes: PaneId[] }
  | { type: "view"; tab: TabId; cols: number; rows: number; zoom: PaneId | null; claim: boolean }
  | { type: "intent"; id: number | null; intent: Intent }
  | { type: "pane"; pane: PaneId; op: PaneOp }
  | { type: "focus"; pane: PaneId | null }
  | { type: "ping"; id: number };

export type ServerMsg =
  | { type: "hello"; version: string; client: ClientId; state: State }
  | { type: "state"; state: State }
  | { type: "size"; pane: PaneId; cols: number; rows: number }
  | { type: "resync"; pane: PaneId }
  | { type: "error"; id: number | null; message: string }
  | { type: "block"; block: PaneId; state: unknown }
  | { type: "pong"; id: number }
  | { type: "notice"; message: string }
  | { type: "control_request"; pane: PaneId; who: string; name: string };

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
