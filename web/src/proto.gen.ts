// Generated from crates/proto by `just proto-ts`: don't edit. Change the
// Rust types and run it again; CI checks this file is current.

export type PaneId = number;
export type TabId = number;
export type SessionId = number;
export type NodeId = number;
export type ClientId = number;
export type MachineId = number;

export const CALL_MAX = 5;

/**
 * `POST /api/attention/act`: do something about one pane's reason, or
 * several at once ("allow all 3", "dismiss all 11"). Each pane needs
 * editor on its session.
 */
export type ActRequest = { action: Action, pane?: number, panes?: Array<number>, 
/**
 * The ask it answers (`AskRef::id`); without one, whatever the pane
 * asks now.
 */
id?: string, 
/**
 * `answer`: the card's fields.
 */
content?: Record<string, unknown>, 
/**
 * `allow`: `once` (default) or `always`.
 */
option?: string, 
/**
 * `allow` `always` for Claude Code in a terminal (M29): which of its
 * suggestions to keep (default the first).
 */
suggestion?: number, 
/**
 * `deny`: why, for the agent.
 */
message?: string, 
/**
 * `accept` (M28): the file as it should be saved, when someone
 * changed the proposal first.
 */
text?: string, };

/**
 * Something done about a reason (`POST /api/attention/act`).
 */
export type Action = "allow" | "deny" | "answer" | "dismiss" | "continue" | "accept" | "reject" | "rerun";

/**
 * How much a pane prints (M23).
 */
export type Activity = { 
/**
 * Bytes of output a second, over the last second or so.
 */
bps: number, 
/**
 * When it last printed anything (ms since the epoch); 0: not since
 * the daemon started.
 */
last_ms: number, };

/**
 * The open question or approval behind an `ask` reason.
 */
export type AskRef = { 
/**
 * What `allow`, `deny` and `answer` name (a permission request's id, or
 * a question's).
 */
id: string, 
/**
 * `approve` (allow or deny it) or `question` (answer or skip it).
 */
what: AskWhat, 
/**
 * Who asks: the agent (`claude`, `fountain`, …).
 */
agent: string, };

export type AskWhat = "approve" | "question";

export type AttachPane = { pane: number, 
/**
 * Offset just past the last byte the client has, or `None` for a fresh
 * view.
 */
offset: number | null, 
/**
 * At most this many rows of scrollback in a snapshot: what the client
 * keeps (`None`: all of it). After a [`ServerMsg::Resync`], `0`: the
 * client keeps what it has and needs only the screen.
 */
history?: number, };

/**
 * Whether a pane wants you: the cheap version of an "agent block".
 */
export type Attention = "idle" | "working" | "needs_input" | "done";

/**
 * What a block is; where one runs is its `host`, not its type. All types
 * share one id space (`%N`) and one place in the layout tree.
 */
export type BlockType = "terminal" | "browser" | "agent" | "editor" | "diff" | "file" | "remote" | "workspace" | "app" | "forge" | "fountain" | "invite";

/**
 * A huddle: a voice call on a session (M63), peer to peer between its
 * members, signaled through this daemon.
 */
export type Call = { session: number, 
/**
 * New each time a huddle starts on the session, so what's signed for
 * one can't be used in the next.
 */
id: string, 
/**
 * When it started (ms since the epoch).
 */
started: number, 
/**
 * In the order they joined.
 */
members: Array<CallMember>, };

export type CallMember = { client: number, 
/**
 * Their principal id.
 */
who: string, name: string, pic?: string, muted?: boolean, 
/**
 * When they joined (ms since the epoch).
 */
joined: number, 
/**
 * Their device's id, when they connected with a device key (through
 * control): their fingerprints are signed. Absent for a tailnet or
 * local connection.
 */
device?: string, };

/**
 * What one huddle member sends another through the daemon (M63), which
 * passes it on untouched: [`ClientMsg::CallSignal`]'s and
 * [`ServerMsg::CallSignal`]'s `signal`.
 */
export type CallSignal = { type: SdpKind, sdp: string, 
/**
 * Hex Ed25519 signature of [`call_fingerprint_body`] by the sender's
 * device key, when it has one.
 */
sig?: string, };

export type Child = { weight: number, node: Node, };

/**
 * Control messages from a client.
 */
export type ClientMsg = { "type": "attach", panes: Array<AttachPane>, zstd?: boolean, acks?: boolean, 
/**
 * The client encodes keys for the kitty keyboard protocol, so
 * programs asking may be told it's there (M31).
 */
kitty_keys?: boolean, } | { "type": "ack", pane: number, offset: number, } | { "type": "detach", panes: Array<number>, } | { "type": "view", tab: number, cols: number, rows: number, zoom: number | null, claim: boolean, } | { "type": "intent", id: number | null, intent: Intent, } | { "type": "pane", pane: number, op: PaneOp, } | { "type": "focus", pane: number | null, } | { "type": "ping", id: number, } | { "type": "subscribe", summary: boolean, } | { "type": "follow", pane: number, on: boolean, } | { "type": "call_join", session: number, } | { "type": "call_leave", session: number, } | { "type": "call_mute", session: number, muted: boolean, } | { "type": "call_signal", session: number, to: number, signal: CallSignal, } | { "type": "hand", tools: Array<HandTool>, name?: string, } | { "type": "hand_reply", id: number, result?: unknown, error?: string, };

/**
 * A command the shell integration reported.
 */
export type CommandInfo = { text: string | null, cwd: string | null, exit: number | null, started_ms: number, ended_ms: number | null, 
/**
 * Stream offsets of its output: `tail --from start`.
 */
start: number, end: number | null, 
/**
 * Who started it (M13), when someone other than the owner might have.
 */
by?: string, };

/**
 * An editor's debug session.
 */
export type DebugState = { 
/**
 * `running` or `paused`.
 */
state: "running" | "paused", 
/**
 * Why it stopped: `breakpoint`, `exception`, `step`, ...
 */
reason?: string | null, file?: string | null, line?: number | null, };

/**
 * Changes to the last [`State`]: each pane in `panes` is `{id, ...}` with
 * only the fields that changed (a field set to `null` went back to its
 * default, absent); `gone` panes left this client's view. `machines` and
 * `presence` (and `threads`) are whole when present. Anything else (sessions, tabs,
 * options, roles) changes with a new `State`.
 */
export type Delta = { panes?: Array<{ id: PaneId } & Partial<PaneInfo>>, gone?: Array<number>, machines?: Array<Machine> | null, presence?: Array<Presence> | null, threads?: Array<ThreadSummary> | null, calls?: Array<Call> | null, };

/**
 * Diagnostic counts: errors, warnings, information.
 */
export type Diag = { e: number, w: number, i: number, };

/**
 * An edit an agent proposes, waiting as a diff (M28).
 */
export type DiffInfo = { 
/**
 * What `accept` and `reject` name.
 */
id: string, 
/**
 * The file it changes (whole path), and relative to the pane's
 * directory when it's inside.
 */
file: string, 
/**
 * Lines added and removed.
 */
added: number, removed: number, 
/**
 * The change as a unified diff, cut short when it's long.
 */
text: string, 
/**
 * It makes a new file.
 */
new?: boolean, at_ms: number, 
/**
 * Which IDE shows it: `illogical`, or the one diffs go to.
 */
ide: string, };

export type Dir = "row" | "column";

/**
 * A pane's driver (M13).
 */
export type Driver = { 
/**
 * Principal id (`owner`, `tailnet:<login>`, `account:<id>`).
 */
who: string, name: string, };

/**
 * Where to put something relative to a pane.
 */
export type Edge = "left" | "right" | "top" | "bottom" | "center";

/**
 * What an editor says about itself in summaries (M28, S17's schema):
 * what changes about once in ten seconds. The cursor and the file's text
 * are content and go only to followers ([`ServerMsg::Follow`]).
 */
export type EditorInfo = { 
/**
 * `vscode`, `cursor`, `code-server`, `nvim`, ...
 */
app: string, 
/**
 * VS Code's remote: `ssh-remote`, `dev-container`, ... (`None`: local).
 */
remote?: string | null, 
/**
 * The remote's authority (`ssh-remote+geek`), to open the same file
 * from a desktop editor.
 */
authority?: string | null, 
/**
 * The machine it runs on, as it names itself.
 */
hostname?: string | null, 
/**
 * Diagnostics across the workspace.
 */
diag: Diag, 
/**
 * Files with unsaved changes.
 */
dirty: number, debug?: DebugState | null, 
/**
 * A file with merge conflict markers that's open.
 */
conflict?: string | null, 
/**
 * How many people follow it now: the editor says so.
 */
followers: number, };

export type FollowChange = { range: [number, number, number, number], text: string, };

export type FollowCursor = { file: string, line: number, col: number, sel?: [number, number, number, number] | null, 
/**
 * The first and last lines in view.
 */
view?: [number, number] | null, 
/**
 * nvim's mode.
 */
mode?: string, };

export type FollowDiagnostic = { range: [number, number, number, number], 
/**
 * `error`, `warning`, `information`, `hint`.
 */
severity: string, message: string, };

export type FollowDiagnostics = { file: string, items: Array<FollowDiagnostic>, };

export type FollowEdit = { file: string, version: number, changes: Array<FollowChange>, };

export type FollowMsg = FollowCursor | { open: FollowOpen, } | { edit: FollowEdit, } | { diagnostics: FollowDiagnostics, } | { gone: true, };

export type FollowOpen = { file: string, version: number, text: string | null, lang?: string, too_big?: boolean, };

/**
 * A gate that waits for someone (M34): an op stopped before a step until
 * a person approves it. The `gate` reason, its bundle on the swarm's rail,
 * the card and the phone's sheet are all made from this, whichever reader
 * found it (`source`).
 */
export type Gate = { 
/**
 * The member (of a workspace) whose op waits.
 */
member: string, op: string, gate: string, env?: string, 
/**
 * When it started waiting, and when it stops (RFC 3339).
 */
since?: string, expires?: string, 
/**
 * Approvals so far, of how many it needs.
 */
approvals: number, needed: number, 
/**
 * The source's own command for approving it, to show.
 */
command?: string, source: GateSource, };

/**
 * Where a gate was read, which is how it's approved.
 */
export type GateSource = { "kind": "chant", 
/**
 * The workspace's root, and the member's directory, on that host.
 */
root: string, dir: string, 
/**
 * The machine (sprite) it's on; none for this host.
 */
machine?: string, } | { "kind": "hud", 
/**
 * The box's origin, and the app's name in studio.
 */
box_url: string, app: string, } | { "kind": "forge", 
/**
 * The forge's API base (`https://git.example/api/v1`) and its web
 * address for the PR.
 */
api: string, url: string, number: number, };

/**
 * A tool a hand offers (S33).
 */
export type HandTool = { name: string, description: string, 
/**
 * Its arguments, as a JSON Schema object.
 */
schema: unknown, };

/**
 * The optional parts of a machine, as `GET /api/host` reports them.
 */
export type HostFeatures = { 
/**
 * The machine has a `labs` file in its state dir (see [`labs`]): what a
 * stranger doesn't get is on. Absent from older daemons, and pages
 * treat that as off.
 */
labs: boolean, 
/**
 * Browser blocks on ports and editor blocks: block sites are on
 * (`--block-listen`).
 */
blocks: boolean, 
/**
 * VM tabs and panes and *Sandboxes…*: a sandbox provider (wisp).
 */
vms: boolean, 
/**
 * A Fountain login here: `FOUNTAIN_API_KEY`, or the CLI's
 * credentials file.
 */
fountain: boolean, 
/**
 * A studio is linked (`illogical studio login`).
 */
studio: boolean, 
/**
 * Threads on panes and sessions: with `labs`. Older daemons leave it
 * out, and pages hide threads there.
 */
threads?: boolean, 
/**
 * Huddles on sessions, likewise.
 */
calls?: boolean, };

export type Intent = { "op": "new_session", name: string | null, from_pane: number | null, } | { "op": "rename_session", session: number, name: string, } | { "op": "close_session", session: number, } | { "op": "new_tab", session: number, from_pane: number | null, cwd?: string, } | { "op": "rename_tab", tab: number, name: string | null, } | { "op": "close_tab", tab: number, } | { "op": "move_tab", tab: number, session: number, index: number, } | { "op": "split", pane: number, edge: Edge, local?: boolean, cwd?: string, } | { "op": "close_pane", pane: number, } | { "op": "move_pane", pane: number, target: number, edge: Edge, } | { "op": "break_pane", pane: number, session: number, index: number | null, } | { "op": "dock_tab", tab: number, target: number, edge: Edge, } | { "op": "resize_split", split: number, weights: Array<number>, } | { "op": "set_option", scope: OptionScope, name: string, value: string | null, };

export type Layout = { panes: Array<[number, Rect]>, splits: Array<SplitRect>, };

/**
 * A machine that blocks can run on instead of this host: today a
 * throwaway wisp sprite (a Firecracker microVM) owned by one pane, and
 * deleted when that pane closes.
 */
export type Machine = { id: number, 
/**
 * Who runs it: `wisp`.
 */
provider: string, 
/**
 * The provider's name for it.
 */
sprite: string, 
/**
 * What to call it ("drifting cedar", M7): a display name for the
 * machines we make. The sprite keeps its own name.
 */
name?: string | null, image: string | null, 
/**
 * What it belongs to; the machine goes when that closes.
 */
owner: Owner, state: MachineState, 
/**
 * Someone else's sandbox, borrowed for a shell (M4b): never created or
 * deleted by us; closing its owner only ends our sessions on it.
 */
borrowed?: boolean, 
/**
 * Made for a guest (M14): their principal id, for their quota.
 */
by?: string | null, };

export type MachineState = "starting" | "running" | "gone";

export type Node = { "type": "pane", pane: number, } | { "type": "split", id: number, dir: Dir, children: Array<Child>, };

/**
 * Where an option lives, as in tmux: the server, a session, a window (tab)
 * or a pane.
 */
export type OptionScope = { "kind": "global" } | { "kind": "session", "id": number } | { "kind": "tab", "id": number } | { "kind": "pane", "id": number };

/**
 * Opaque named strings that clients keep with the layout (tmux's `@user`
 * options: iTerm2's tab grouping and attach guard, `@affinities`). Saved
 * with the layout; an entry goes when what it belongs to does.
 */
export type Options = { global?: { [key in string]: string }, sessions?: Array<[number, { [key in string]: string }]>, tabs?: Array<[number, { [key in string]: string }]>, panes?: Array<[number, { [key in string]: string }]>, };

/**
 * A machine's owner: one pane (M3b), or a tab whose panes share it (M3c).
 * JSON `{"pane": 3}` or `{"tab": 2}`; a bare number (M3b's layout.json) is
 * a pane.
 */
export type Owner = { "pane": number } | { "tab": number };

export type PaneInfo = { id: number, 
/**
 * Identifies this pane's output stream. Offsets are only meaningful
 * within one epoch; a client holding an offset from another epoch (an
 * earlier daemon) must attach with `None`.
 */
epoch: number, 
/**
 * The pane process's working directory, when known.
 */
cwd: string | null, 
/**
 * The foreground command, when it isn't the shell itself.
 */
command: string | null, 
/**
 * Whether a process is running (false while a restored pane waits for
 * Enter).
 */
running: boolean, policy: Policy, 
/**
 * Running now, per the shell integration.
 */
current: CommandInfo | null, 
/**
 * The last command that finished.
 */
last: CommandInfo | null, attention: Attention, 
/**
 * Why it wants you (M24), when it does.
 */
reason?: Reason | null, 
/**
 * Shell integration for shells started in this pane.
 */
integration: boolean, type: BlockType, 
/**
 * The machine it runs on; `None` is this host.
 */
host: number | null, 
/**
 * A question open in a terminal (Claude Code's AskUserQuestion, through
 * its hook), drawn as a card beside it (M6c).
 */
ask?: import("./blocks/ask").Ask | null, 
/**
 * Who answered its last question or approval, and how (M29), until
 * it asks again. For agent blocks too.
 */
answered?: import("./blocks/ask").Answered | null, 
/**
 * An edit Claude Code in this terminal proposes, waiting as a diff
 * (M28: illogicald as its IDE).
 */
diff?: DiffInfo | null, 
/**
 * Claude Code in this terminal is connected to illogicald as its IDE
 * (M28): lines can be mentioned to it from a followed editor.
 */
claude_ide?: boolean, 
/**
 * Claude Code in this terminal waits for a follow-up (its `illogical
 * inbox` hook, M29): one sent now goes straight in.
 */
inbox?: boolean, 
/**
 * Who is driving it (M13): only their typing reaches it, unless it's
 * in pair mode. `None`: nobody yet (the next to type drives).
 */
driver?: Driver | null, 
/**
 * Its driver typed in it in the last few seconds (#118).
 */
typing?: boolean, 
/**
 * Pair mode: every editor types at once.
 */
pair?: boolean, 
/**
 * Never shown to anyone but the owner (M14).
 */
private?: boolean, 
/**
 * Guests trusted to drive this pane, though it runs on the owner's
 * machine (M14): principal id and until when (ms).
 */
trusted?: Array<[string, number]>, 
/**
 * What it's busy with (M23); `None` for blocks other than terminals
 * and agents.
 */
kind?: WorkKind | null, 
/**
 * What a restart resumes (#146): the agent conversation running in
 * it, as "Claude Code conversation <title>", when its policy says to.
 */
resumes?: string | null, 
/**
 * The git repository it works in, if any (M23).
 */
project?: Project | null, 
/**
 * Output rate (M23).
 */
activity?: Activity | null, 
/**
 * The title its program set (OSC 0/2), if any.
 */
title?: string | null, 
/**
 * The file an editor block shows (M27), relative to its folder.
 */
file?: string | null, 
/**
 * What started it, when that wasn't you: an MCP client (M16).
 */
started_by?: StartedBy | null, 
/**
 * An editor's own report (M28): an editor block's, or someone's
 * editor elsewhere (VS Code, Cursor, nvim) that joined the swarm.
 */
editor?: EditorInfo | null, };

export type PaneOp = { "op": "set_policy", policy: Policy, } | { "op": "purge" } | { "op": "set_integration", on: boolean, } | { "op": "attention", state: Attention, } | { "op": "take_control" } | { "op": "request_control" } | { "op": "give_control", to: string, } | { "op": "release_control" } | { "op": "set_pair", on: boolean, } | { "op": "request_trust" } | { "op": "grant_trust", to: string, minutes: number, } | { "op": "revoke_trust", to: string, } | { "op": "set_private", on: boolean, };

/**
 * What a pane does when the daemon restores it. Its scrollback always comes
 * back; this decides what runs in it.
 */
export type Policy = { "kind": "none" } | { "kind": "shell" } | { "kind": "rerun", confirm: boolean, } | { "kind": "hook", command: string, } | { "kind": "resume" };

/**
 * Someone looking at the daemon (M13): one per connected client.
 */
export type Presence = { client: number, 
/**
 * Principal id: one person's clients share it.
 */
who: string, name: string, pic?: string, 
/**
 * The tab it shows, and the pane it's focused on.
 */
tab?: number, pane?: number, };

/**
 * The git repository a pane's working directory is in (M23).
 */
export type Project = { 
/**
 * The repository's top directory.
 */
root: string, 
/**
 * Its last path component.
 */
name: string, };

/**
 * Output quoted in a thread message: kept as text, so it stays readable
 * after the pane scrolls or closes.
 */
export type Quote = { pane: number, text: string, };

/**
 * Why a pane wants you (M24): what happened, not just "needs you", so a
 * client can explain it, bundle it with others and act on it. Every
 * `needs_input` and `done` pane has one.
 */
export type Reason = { kind: ReasonKind, 
/**
 * When it started wanting you.
 */
since_ms: number, 
/**
 * One line: the question, the command that failed, what finished.
 */
headline: string, command?: string, exit?: number, 
/**
 * `done` and `failed`: how long the command ran.
 */
duration_ms?: number, 
/**
 * Reasons with the same key are one card on a "needs you" rail ("11
 * failed on build-03"): `failed:<machine>`, `exited:<machine>`,
 * `ask:<project>:<agent>`. `None` never bundles.
 */
bundle?: string, 
/**
 * `ask`: what is asked, and how to answer it.
 */
ask?: AskRef, 
/**
 * `gate`: the gate that waits (the first, if several do), which
 * `allow` approves (M34).
 */
gate?: Gate, 
/**
 * What [`api::ActRequest`] can do about it here.
 */
actions: Array<Action>, };

export type ReasonKind = "ask" | "input" | "failed" | "exited" | "done" | "paused" | "errors" | "conflict" | "diff" | "gate";

export type Rect = { x: number, y: number, cols: number, rows: number, };

/**
 * Where a remote block's pane lives (#17): a host in the home daemon's
 * list, and the pane's id there.
 */
export type RemoteRef = { host: string, pane: number, };

/**
 * Ordered: an owner can do anything an editor can, and so on.
 */
export type Role = "viewer" | "editor" | "owner";

export type SdpKind = "offer" | "answer";

/**
 * Control messages from the server.
 */
export type ServerMsg = { "type": "hello", version: string, client: number, state: State, } | { "type": "state", state: State, } | { "type": "size", pane: number, cols: number, rows: number, } | { "type": "resync", pane: number, } | { "type": "error", id: number | null, message: string, } | { "type": "block", block: number, state: unknown, } | { "type": "pong", id: number, } | { "type": "notice", message: string, } | { "type": "control_request", pane: number, who: string, name: string, } | { "type": "trust_request", pane: number, who: string, name: string, } | { "type": "delta", delta: Delta, } | { "type": "follow", pane: number, msg: FollowMsg, } | { "type": "thread", target: ThreadTarget, msg: ThreadMsg, } | { "type": "call_signal", session: number, from: number, signal: CallSignal, cert?: unknown, } | { "type": "hand_call", id: number, tool: string, args: Record<string, unknown>, from: string, };

export type Session = { id: number, name: string, tabs: Array<number>, };

/**
 * A split's area and how long each child is along the split's direction,
 * so a client can turn a divider drag into new weights.
 */
export type SplitRect = { id: number, dir: Dir, rect: Rect, extents: Array<number>, };

/**
 * Who started a pane or block through MCP (M16).
 */
export type StartedBy = { 
/**
 * `mcp:<client>`, as the pane and history show it.
 */
by: string, 
/**
 * The agent block whose token it came with, if any: that block may
 * drive and close it.
 */
block?: number, };

/**
 * Everything a client needs to draw: sessions in order, each tab's tree
 * and the cell rectangles the server computed for it, and pane details.
 */
export type State = { rev: number, sessions: Array<Session>, tabs: Array<TabView>, panes: Array<PaneInfo>, 
/**
 * Machines that blocks run on, other than this host.
 */
machines: Array<Machine>, 
/**
 * Clients' named options (tmux `@` options), per scope.
 */
options: Options, 
/**
 * For someone who isn't the daemon's owner (M12): their role in each
 * session they see, as `[[session, role], ...]` (JSON object keys
 * can't come back as numbers inside a tagged message). Absent for the
 * owner, who owns everything.
 */
roles?: Array<[number, Role]> | null, 
/**
 * Who else is here and where they're looking (M13), within what this
 * client sees.
 */
presence?: Array<Presence>, 
/**
 * The threads (M61) this person may read that have messages, with how
 * many they haven't read.
 */
threads?: Array<ThreadSummary>, 
/**
 * Huddles (M63) on the sessions this person has a role in.
 */
calls?: Array<Call>, };

export type TabView = { id: number, name: string | null, root: Node, cols: number, rows: number, owner: number | null, zoom: number | null, layout: Layout, };

/**
 * One message in a thread (M61).
 */
export type ThreadMsg = { 
/**
 * 1, 2, 3, ... within its thread.
 */
id: number, 
/**
 * When it was posted (ms since the epoch).
 */
at: number, 
/**
 * Who posted it: a principal id (`owner`, `tailnet:…`, `account:…`),
 * or `mcp:…` for an agent.
 */
who: string, name: string, 
/**
 * M74: the poster's picture when they posted, if they have one.
 */
pic?: string, text: string, 
/**
 * Terminal output it quotes.
 */
quote?: Quote, 
/**
 * Principal ids it @mentions.
 */
mentions?: Array<string>, 
/**
 * The `@` tokens (lowercase) that reached someone: each one naming a
 * person in `mentions`, and the agent's when `to_agent`. The page
 * marks only these; an `@word` that reached no one stays plain.
 */
landed?: Array<string>, 
/**
 * It @mentioned the pane's agent, and went to it as a follow-up.
 */
to_agent?: boolean, 
/**
 * An agent posted it (through MCP).
 */
agent?: boolean, };

/**
 * A thread as one person has it (M61).
 */
export type ThreadSummary = { target: ThreadTarget, 
/**
 * The newest message's id and time.
 */
last: number, at: number, 
/**
 * Messages from others they haven't read.
 */
unread?: number, 
/**
 * One of those mentions them.
 */
mention?: boolean, };

/**
 * What a thread (M61) is about: a pane, or a session.
 */
export type ThreadTarget = { "pane": number } | { "session": number };

/**
 * What a pane is busy with (M23), for drawing and grouping it without
 * attaching: from the foreground process's command line, else the
 * command the shell integration reported.
 */
export type WorkKind = "shell" | "build" | "test" | "agent" | "server" | "logs" | "editor" | "app" | "pr" | "issue" | "fountain";
