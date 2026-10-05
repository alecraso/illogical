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
  | { kind: "hook"; command: string }
  /** #146: the agent conversation that was running, by its session id. */
  | { kind: "resume" };

export type Attention = "idle" | "working" | "needs_input" | "done";

/** M24: why a pane wants you. */
export type ReasonKind = "ask" | "input" | "failed" | "exited" | "done" | "paused" | "errors" | "conflict" | "diff" | "gate";
export type Action = "allow" | "deny" | "answer" | "dismiss" | "continue" | "accept" | "reject" | "rerun";

export interface Reason {
  kind: ReasonKind;
  since_ms: number;
  headline: string;
  command?: string;
  exit?: number;
  duration_ms?: number;
  /** Same key, one card on a rail: `failed:<machine>`, `ask:<project>:<agent>`. */
  bundle?: string;
  ask?: { id: string; what: "approve" | "question"; agent: string };
  /** `gate` (M34): the gate that waits (the first of several); `allow`
   * approves it. */
  gate?: Gate;
  actions: Action[];
}

/** M34: a gate waiting for a person, whichever reader found it. */
export interface Gate {
  member: string;
  op: string;
  gate: string;
  env?: string;
  since?: string;
  expires?: string;
  approvals: number;
  needed: number;
  /** The source's own command, to show. */
  command?: string;
  source:
    | { kind: "chant"; root: string; dir: string; machine?: string }
    | { kind: "hud"; box_url: string; app: string }
    // M36: a review asked of you on a pull request.
    | { kind: "forge"; api: string; url: string; number: number };
}

/** What names a gate among its block's: `member/op/gate`. */
export function gateKey(g: Gate): string {
  return `${g.member}/${g.op}/${g.gate}`;
}

/** `POST /api/attention/act`. */
export interface ActRequest {
  action: Action;
  pane?: PaneId;
  panes?: PaneId[];
  id?: string;
  content?: Record<string, unknown>;
  option?: string;
  /** `allow` `always` in a terminal: which of Claude Code's suggestions. */
  suggestion?: number;
  message?: string;
  /** M28 `accept`: the file as it should be saved, changed first. */
  text?: string;
}

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
  /** #146: what a restart resumes ("Claude Code conversation 0f3c2a9e"). */
  resumes?: string;
  current: CommandInfo | null;
  last: CommandInfo | null;
  attention: Attention;
  /** M24: why it wants you, when it does. */
  reason?: Reason | null;
  /** Shell integration for shells started in this pane. */
  integration: boolean;
  type: BlockType;
  /** The machine it runs on; null is the daemon's host. */
  host: MachineId | null;
  /** A question Claude Code asks in this terminal (through its hook), drawn
   * as a card beside it (M6c). */
  ask?: import("./blocks/ask").Ask | null;
  /** M29: who answered its last card, and how, until it asks again. */
  answered?: import("./blocks/ask").Answered | null;
  /** M29: Claude Code here waits for a follow-up (its inbox hook). */
  inbox?: boolean;
  /** M13: who drives it; absent when nobody does yet. */
  driver?: Driver;
  /** Its driver typed in it in the last few seconds (#118). */
  typing?: boolean;
  /** Pair mode: every editor types at once. */
  pair?: boolean;
  /** M14: never shown to anyone but the owner. */
  private?: boolean;
  /** M14: guests trusted to drive it on the owner's machine, until (ms). */
  trusted?: [string, number][];
  /** M23: what it's busy with; absent for browser blocks. */
  kind?: WorkKind | null;
  /** M23: the git repository it works in. */
  project?: Project | null;
  /** M23: how much it prints. */
  activity?: Activity | null;
  /** M23: the title its program set (OSC 0/2). */
  title?: string | null;
  /** M27: the file an editor block shows, relative to its folder. */
  file?: string | null;
  /** Started by an MCP client (M16): `mcp:<client>`, and the agent block whose token it came with. */
  started_by?: StartedBy | null;
  /** M28: an editor's own report: an editor block's window, or an editor
   * that joined the swarm (no tab). */
  editor?: EditorInfo | null;
  /** M28: an edit Claude Code here proposes, waiting as a diff. */
  diff?: DiffInfo | null;
  /** M28: Claude Code here is connected to illogical as its IDE. */
  claude_ide?: boolean;
}

/** M28: what an editor says in summaries. */
export interface EditorInfo {
  /** `vscode`, `cursor`, `code-server`, `nvim`. */
  app: string;
  remote?: string | null;
  /** `ssh-remote+geek`: for opening the same file from a desktop editor. */
  authority?: string | null;
  hostname?: string | null;
  diag: { e: number; w: number; i: number };
  dirty: number;
  debug?: { state: "running" | "paused"; reason?: string | null; file?: string | null; line?: number | null } | null;
  conflict?: string | null;
  followers: number;
}

/** M28: an edit waiting as a diff. */
export interface DiffInfo {
  id: string;
  file: string;
  added: number;
  removed: number;
  /** A unified diff (hunks only), cut short when long. */
  text: string;
  new?: boolean;
  at_ms: number;
  ide: string;
}

/** M28: what a followed editor sends: lines from 1, columns from 0. */
export type FollowMsg =
  | { file: string; line: number; col: number; sel?: [number, number, number, number] | null; view?: [number, number] | null; mode?: string }
  | { open: { file: string; version: number; text: string | null; lang?: string; too_big?: boolean } }
  | { edit: { file: string; version: number; changes: { range: [number, number, number, number]; text: string }[] } }
  | { diagnostics: { file: string; items: { range: [number, number, number, number]; severity: string; message: string }[] } }
  | { gone: true };

export interface StartedBy {
  by: string;
  block?: PaneId;
}

export type WorkKind = "shell" | "build" | "test" | "agent" | "server" | "logs" | "editor" | "app" | "pr" | "issue" | "fountain";

export interface Project {
  root: string;
  name: string;
}

export interface Activity {
  /** Bytes of output a second, lately. */
  bps: number;
  /** When it last printed (ms since the epoch). */
  last_ms: number;
}

/** M23: what changed since the last State. Each pane is `{id, ...}` with
 * only the fields that changed (`null`: back to absent); `gone` panes left
 * this client's view; `machines` and `presence` are whole when present.
 * Sessions, tabs, options and roles change with a new State. */
export interface Delta {
  panes?: ({ id: PaneId } & Partial<PaneInfo>)[];
  gone?: PaneId[];
  machines?: Machine[];
  presence?: Presence[];
  threads?: ThreadSummary[];
}

export type BlockType = "terminal" | "browser" | "agent" | "editor" | "diff" | "file" | "remote" | "workspace" | "app" | "forge" | "fountain";

/** A remote block's config and state (#17): a pane on another host in the
 * home daemon's list, shown in this layout. */
export interface RemoteRef {
  host: string;
  pane: PaneId;
}
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
  | { op: "set_pair"; on: boolean }
  | { op: "request_trust" }
  | { op: "grant_trust"; to: string; minutes: number }
  | { op: "revoke_trust"; to: string }
  | { op: "set_private"; on: boolean };

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
  /** M61: the threads this person may read that have messages. */
  threads?: ThreadSummary[];
}

/** M61: what a thread is about. */
export type ThreadTarget = { pane: PaneId } | { session: SessionId };

/** M61: one message in a pane's or session's thread. */
export interface ThreadMsg {
  id: number;
  at: number;
  who: string;
  name: string;
  text: string;
  /** Terminal output it quotes, kept as text. */
  quote?: { pane: PaneId; text: string };
  /** Principal ids it @mentions. */
  mentions?: string[];
  /** It went to the pane's agent as a follow-up. */
  to_agent?: boolean;
  /** An agent posted it. */
  agent?: boolean;
}

/** M61: a thread as this person has it. */
export interface ThreadSummary {
  target: ThreadTarget;
  last: number;
  at: number;
  unread?: number;
  mention?: boolean;
}

/** `pane-7` / `session-2`: a thread's name in its API path. */
export function threadKey(t: ThreadTarget): string {
  return "pane" in t ? `pane-${t.pane}` : `session-${t.session}`;
}

export type Role = "viewer" | "editor" | "owner";

/** `GET /api/host`'s `features` (#171, #180): what this machine is set up
 * for. */
export interface HostFeatures {
  /** Browser blocks on ports and editor blocks (`--block-listen`). */
  blocks: boolean;
  /** VM tabs and panes, and sandboxes (wisp). */
  vms: boolean;
  /** A Fountain login. */
  fountain: boolean;
  /** A linked studio. */
  studio: boolean;
  /** M61: threads on panes and sessions (absent: an older daemon). */
  threads?: boolean;
}

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
  | { type: "attach"; panes: AttachPane[]; zstd?: boolean; acks?: boolean }
  /** Everything of `pane` before `offset` is drawn (for an attach with
   * `acks`, #52). */
  | { type: "ack"; pane: PaneId; offset: number }
  | { type: "detach"; panes: PaneId[] }
  | { type: "view"; tab: TabId; cols: number; rows: number; zoom: PaneId | null; claim: boolean }
  | { type: "intent"; id: number | null; intent: Intent }
  | { type: "pane"; pane: PaneId; op: PaneOp }
  | { type: "focus"; pane: PaneId | null }
  | { type: "ping"; id: number }
  /** M23: summaries only (no pane output; panes leave out epoch, policy
   * and integration). Answered with a fresh State. */
  | { type: "subscribe"; summary: boolean }
  /** M28: follow an editor's cursor and file (viewer access). */
  | { type: "follow"; pane: PaneId; on: boolean };

export type ServerMsg =
  | { type: "hello"; version: string; client: ClientId; state: State }
  | { type: "state"; state: State }
  | { type: "size"; pane: PaneId; cols: number; rows: number }
  | { type: "resync"; pane: PaneId }
  | { type: "error"; id: number | null; message: string }
  | { type: "block"; block: PaneId; state: unknown }
  | { type: "pong"; id: number }
  | { type: "notice"; message: string }
  | { type: "control_request"; pane: PaneId; who: string; name: string }
  | { type: "trust_request"; pane: PaneId; who: string; name: string }
  | { type: "delta"; delta: Delta }
  | { type: "follow"; pane: PaneId; msg: FollowMsg }
  | { type: "thread"; target: ThreadTarget; msg: ThreadMsg };

export const enum FrameKind {
  Output = 1,
  Snapshot = 2,
  Input = 3,
  /** A snapshot compressed with zstd (for an attach with `zstd`). */
  SnapshotZstd = 4,
}

export interface AttachPane {
  pane: PaneId;
  offset: number | null;
  /** At most this many rows of scrollback in a snapshot (absent: all). `0`
   * after a resync: we keep ours and need only the screen. */
  history?: number;
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
