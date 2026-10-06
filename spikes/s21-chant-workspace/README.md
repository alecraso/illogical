# S21: a chant workspace as blocks (#70)
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s21-chant-workspace/<file>`.

**Verdict: go.** A `workspace` block that reads chant's read contract and nothing else is enough to make a chant workspace something you work *in*, not just look at. It shows members as cards you open shells, agents and diffs on. Records show with their blocked and drift state. A gate waiting in any member becomes illogical attention, and *Approve* resolves it with chant's own `approve`. None of it needed a change to chant.

There are two conditions. Reads cost about 7.5 CPU-seconds each, so the block must re-read only when something changed (a fingerprint, below). The block's host needs node and the workspace's own chant, so it has to resolve the shell environment the way VS Code does.

Checked 2026-10-02 against chant `6fcb9f34` (0.87.0).

## What was built

- **This crate:** the reader and composer, plus a CLI to try them.
  - `src/model.rs` is the shared half:
    - [`SCRIPT`] + [`READER`] are one `sh -c` on the block's host. It finds a chant, has node run `workspace ls --json`, `check --format json`, `records --current --json` and `status <env> --json` in parallel, and prints one JSON document.
    - `compose` (pure) turns that document into block state: member cards, records, pending gates, and why the block wants you.
    - `FINGERPRINT` is the cheap change check.
  - `src/main.rs` (`cargo run -- DIR [--env E] [--json] [--raw] [--times N]`) prints what the block would show.
  - `fixtures/` holds two raw documents and tests of the composer.
- **The throwaway block,** on branch `s21-workspace-block` only, not to merge:
  - `BlockType::Workspace` and `crates/daemon/src/workspace.rs`. It includes `model.rs` by path, so the block and the CLI can't disagree.
  - `illogical workspace [DIR] [--env E]`.
  - A web renderer (`web/src/blocks/workspace.tsx`) and a TUI note.
  - Methods: `refresh`, `approve {member, op, gate}`, `member {name}`, `state`.

Tried on a dev daemon against:
- chant's own repo: 25 members, a fresh clone with `npm ci`;
- a standalone copy of `reference-workspace` (4 members, its own git repo, a toy `ship.op.ts` and `release.op.ts` with a gate each);
- the nested `reference-workspace` inside chant;
- `~/dev/intentius/chant` as it is, with no `node_modules`.

![The reference workspace: a gate waiting, four member cards, records](reference.png)

![chant's 25 members, with the nested reference-workspace opened beside it](nested.png)

## Numbers

| | reference (4 members) | chant (25 members) |
|---|---|---|
| each read, alone | ls 1.0 s, check 1.0 s, records 1.2 s, status 1.0 s | ls 1.2 s, check 1.2 s, records 1.1 s, status 1.4 s |
| all four in parallel (wall) | 1.20–1.29 s | 1.39–1.46 s |
| CPU per full read | — | **6.3–6.7 s user + 1.1–1.2 s sys**, 285 MB peak RSS |
| fingerprint (`git rev-parse` ×2, `status --porcelain`, `diff HEAD`) | ~0 s | ~0.1 s CPU |
| daemon while the block is drawn and nothing changes | | 0.13 CPU-s per 30 s (0.4% of a core) |
| a gate written outside (`chant run` exits 3) → attention | 1.0 s | |

Each chant process loads chant's TypeScript source through tsx. That cost is the same per process whatever it reads, so four reads cost four startups. Polling full reads every 15 s would hold about half a core while drawn. Polling the fingerprint every 3 s costs almost nothing, and gives a worst case of about 4.5 s from change to screen.

## What the block shows, and where each field comes from

| Shown | From |
|---|---|
| name, root, chant version | `ls`: `workspace.name`, `chant` |
| member cards (name, dir, kind, roles, `because` as the tooltip) | `ls.members[]` |
| nested workspace (*Open* makes another workspace block beside it) | `ls.members[].kind == "workspace"` |
| unreadable member | `ls.members[].readable/reason` |
| errors and warnings per member | `check --format json`: `declaration.diagnostics[]` and `findings[]`, by `entity` (`WSP009`, "kind other, which chant does not read", is dropped: that's the card's `because`) |
| releases per member | `status <env>`: `members[].environments[].releases` |
| **gates waiting** (op, gate, since, expires, approvals/needed, chant's command) | `status <env>`: `members[].gates[]` with `state == "pending"` |
| records (id, state, title, blocked by, drift warnings) | `records --current --json`: `kinds[].records[]` (`blockedBy`, `warnings`, `data.title`) |
| attention headline | composed: an error, else the first gate, else an unreadable or erroring member, else a drifted record |

Not shown, and not needed for a first milestone: `graph`, which only runs members of kind `chant` (on chant itself, 24 of 25 are `skipped`, `kind-not-run`), and `lineage`, which needs `.chant/workspace.lock.json`.

## Answers

1. **Can a useful block be drawn from the read contract alone? Yes.** The gate read I thought was missing is there. `workspace status <env> --json` carries each member's `gateLedger` and `gates[]`, with chant's `approve` command (#70 said otherwise; `operator status --json` and `run … --json` aren't needed). The action loop works too:
   - *Approve* runs `chant approve <op> <gate>` in the member's directory, and the gate and the attention clear.
   - *Run op* opens a pane running `chant run <op>`.
   - chant records the approval as `resolvedBy: "jake"`, `kind: human`, `origin: cli`.
2. **Which chant:**
   - `$CHANT` if set;
   - else `node_modules/.bin/chant` in the root (`workspace`) or the nearest parent (`above`, for a nested workspace inside its outer one);
   - else `chant` on PATH;
   - else the block says so: "no chant here: not in node_modules/.bin (run npm install) and not on PATH". That's what `~/dev/intentius/chant` shows today.

   Separately, **the daemon's own PATH had no node.** mise sets it up only in `.bashrc`, which the daemon's `sh -c` never reads, so the first try said "node is not on PATH". When node is missing, the script now takes PATH from `$SHELL -ic`. A milestone should resolve the shell environment once per host and cache it, for any block that runs the user's tools. On a VM tab it runs on that machine through the provider (`Runner`), so the VM needs node and the workspace installed (untested).
3. **Freshness:** use a fingerprint, not a timer.
   - A full read runs on open, on `refresh` and after `approve`.
   - While drawn, the fingerprint is checked every 3 s, and a full read runs only when it changes. Gates and releases move the `chant/lifecycle` ref, while records, the declaration and sources move the working tree.
   - `status` never fetches, so a gate reached in CI shows only after someone fetches `chant/lifecycle`. A milestone could fetch that one ref on a slow timer, or not at all.
   - Better still, chant could read everything in one process (a draft below), which would cut the cost about 4×.
4. **Gates:** read them from `status <env>`. No new chant command is needed, and the block reads no ledger itself. Two rough edges, drafted for chant below:
   - `status` wants an env even though gates have `env: null`;
   - in a nested workspace, a run writes its gate under the outer workspace's prefix while the nested workspace's `status` reads its own, so the gate never shows.
5. **One block with cards, or a layout of member blocks? One block with cards.**
   - On chant, 25 members as blocks would be 25 tiles, nearly all lexicons with nothing live.
   - A card is a launcher plus status. *Shell*, *Agent* and *Changes* open real blocks beside the workspace, in the member's directory, and that is where member blocks belong: made when you work on a member, not for every member.
   - Cards scale. The 25-member grid scrolls fine, though on chant it's mostly a directory launcher until members become kind `chant` or chant declares its record kinds (below).
6. **Where illogical stops and hud starts (`ws-052`):**
   - illogical **shows** status, gates and records read-only, and **acts operationally**: approve a gate, run an op, open shells, agents and diffs on a member.
   - It doesn't write or review records, show quorum, run comment mode or hold review sessions. Those are hud's.
   - A record row could later link to hud for those, when hud runs.

Not tried:
- the *Agent* button, which would start a Claude Code session in the member;
- a VM tab;
- the phone sheet;
- `graph --intent` in a file block;
- who may approve in a shared session. Approve is any caller today. chant's trust is local, so the approval is recorded under the daemon host's user, and a guest's would need `--approver <their name>` and a rule about whether guests may approve at all.

## For a milestone

- **`workspace` block type**, the shape here: cards, gates, records, attention from the composed headline, and nested workspaces as blocks.
- **Freshness:** the fingerprint poll while drawn, a full read on change, and a full read on `refresh` and after `approve`.
- **Shell environment:** resolve it once per host and cache it, for blocks that run the user's tools. It's the same problem VS Code solves.
- **Approve:** owner only at first. Pass `--approver` with the caller's name once guests may approve, and decide that rule (Jake).
- **Opening one:**
  - `illogical workspace [DIR]`;
  - *Open as workspace* where a pane's directory has a `chant.workspace.json` (the pane menu, the picker);
  - the MCP `open_workspace`, so an agent can show it.
- **Agents on a member:** start in the member's directory, with `ws-048`'s scope later (the spec query as context).
- **Tests:** the composer's fixtures, an e2e spec against a fixture workspace with a toy gated op (needs node and chant in CI), and approve clearing attention.

## Drafts for chant (for Jake to file; not filed)

1. **A nested workspace's gates never show in its `status`.**
   - Repro: in `reference-workspace` inside chant's repo, `chant run ship` in `delivery` writes `_members/reference-workspace/_members/delivery/_gates/ship.jsonl`. But `chant workspace status local --json` from `reference-workspace` reads `_members/delivery/_gates` and reports no gate ledger.
   - A standalone copy (its own repo) writes and reads `_members/delivery/_gates`, and works.
   - One of the two prefixes is wrong for nested workspaces (`ws-007`).
2. **Declare chant's own record kinds.** chant's `chant.workspace.json` names no record kinds, so `workspace records --json` at its root fails with `--kind <kind file> is required`. With `--kind docs/design/decisions/decision.kind.mjs` it reads all 53 decisions. A `records` entry in the declaration, as `reference-workspace` has, would make them part of the read contract.
3. **`workspace status --gates` without an env.** Gates have `env: null`, but `status` needs `<env>` to show them. Either make env optional (gates only), or add a `gates` section that doesn't depend on it.
4. **One process for a workspace read.** Each `chant` startup costs about 1.6–1.9 CPU-seconds, mostly loading TypeScript through tsx, and a reader needs four documents. A `chant workspace read --json` (ls + check + records + status, one document, each part keeping its own schema), or precompiled `dist/` for the published CLI, would cut a reader's cost about 4×.
5. **`--json` consistency.**
   - `workspace check --json` prints `{lock, ok, findings, declaration}` with no `$schema`, while `--format json` prints the `check/v1` read-contract document.
   - `run status`, `run list` and `run log` ignore `--json`.
   - `operator status --json` prints only a warning, and no JSON, when there are no ConvergeOps and no pending gates.
