# S30, MLS half: openmls for team channels

Run 2026-10-05 on geek (Ryzen AI Max+ 395, Linux). openmls 0.9.0, Rust 1.98.1, node 22.23.3,
headless Chrome 153, Firefox 155 and WebKit (Safari 26.6's engine) through Playwright.

**Result: go.** openmls builds for wasm32 with wasm-bindgen and runs in node, Chrome, Firefox and
WebKit. The whole demo runs in each of them. The bundle is 1.35 MB raw and 540 kB gzipped. Joining a
50-device group takes 8 ms in Chrome, and committing a roster change takes 11 ms. A message takes
well under a millisecond. Every roster event maps to one MLS commit. Control can't add a reader,
because every device checks each commit against the signed roster before it merges it. What control
can still do is drop, delay, or split a channel. It can't read it.

What's needed before M62 (details below):

- **The MLS signing key can't be the browser's WebCrypto device key.** openmls signs synchronously,
  and WebCrypto only signs asynchronously. Each device gets a separate MLS key, made in wasm, and the
  device key signs a binding to it.
- **The roster check is mandatory.** Anyone with the GroupInfo can make an external commit, and
  control holds the GroupInfo. Without the check, control could add itself.
- **Use `max_past_epochs` of 3 or more.** At the default of 0, a message that crosses a commit in
  flight is lost. That happened in the offline run even to a device that was online.

## Try it

From `spikes/s30-talk/mls` (on geek, export
`MISE_NODE_VERSION=22.23.3 PATH=$HOME/.local/share/mise/installs/node/22.23.3/bin:$HOME/.cargo/bin:$HOME/.local/share/mise/shims:$HOME/.local/bin:$PATH` first):

```sh
export CARGO_TARGET_DIR=$PWD/target
cargo run --release --bin demo                  # the done-when demo (accounts, roster events, removal)
cargo run --release --bin scenarios             # offline device + hostile delivery service
cargo run --release --bin bench -- 2,10,50 9    # native timings (N sizes, reps)

./build-wasm.sh wasm rustcrypto                 # opt-level z + wasm-opt -Oz, prints sizes
./build-wasm.sh wasm-fast rustcrypto            # opt-level 3 + wasm-opt -O3
./build-wasm.sh wasm libcrux                    # libcrux crypto provider instead
node run-wasm.mjs wasm-rustcrypto demo          # the same demo, in wasm
node run-wasm.mjs wasm-rustcrypto scenarios
node run-wasm.mjs wasm-rustcrypto bench 2,10,50 9
PLAYWRIGHT_DIR=<dir with playwright-core> node run-browser.mjs chrome wasm-rustcrypto   # or firefox, webkit
```

`run-browser.mjs` serves `bench.html`, which loads the wasm, runs the demo (it checks that bob reads
0 of 2 messages after his removal) and then the benchmark. The raw numbers are in `results/`.

Tools installed for this: `rustup target add wasm32-unknown-unknown`, `cargo install
wasm-bindgen-cli --version 0.2.129` (it must match the crate), and `twiggy` for the size breakdown.
`wasm-opt` is binaryen 116 from mise. playwright-core 1.63 went in the scratchpad, using the system
Chrome and the Firefox and WebKit builds already in `~/.cache/ms-playwright`.

## Files

| file | what |
|---|---|
| `src/ident.rs` | a cut-down copy of the device certificate, the MLS key binding, and revocations |
| `src/roster.rs` | a cut-down copy of the signed, hash-chained roster with presigned invites, and the commit AAD |
| `src/client.rs` | a device: openmls group, keys, and the roster check run on every commit |
| `src/ds.rs` | the stub delivery service (control): orders, stores and fans out ciphertext; first commit per epoch wins |
| `src/scenarios.rs` | the demo, the offline device, the hostile delivery service |
| `src/bench.rs` | timings, shared by native and wasm |
| `src/wasm.rs` | wasm-bindgen exports: `demo()`, `scenarios()`, `bench(sizes, reps)` |

## 1. openmls in wasm

**It builds and runs.** The crypto provider is `openmls_rust_crypto` (RustCrypto). `libcrux`
(`openmls_libcrux_crypto` 0.4) also builds for wasm and passes the demo, but it's bigger and slower
here (tables below). Use RustCrypto.

**getrandom needs three switches.** Three major versions end up in the tree, and each needs its own
feature on `wasm32-unknown-unknown`:

- 0.2 (from `rand_core` 0.6, via ed25519-dalek) needs `js`;
- 0.3 (from openmls' own `js` feature) needs `wasm_js`;
- 0.4 (from the RustCrypto pre-releases in hpke-rs) needs `wasm_js`.

They're listed as target dependencies in `Cargo.toml`. No `RUSTFLAGS` `--cfg getrandom_backend` was
needed with these versions. Turn on openmls' `js` feature for wasm too (it brings `web-time`).

**Size** (`./build-wasm.sh`; the `.wasm` after wasm-bindgen and wasm-opt, plus 14 kB of JS glue):

| build | raw | gzip -9 |
|---|---|---|
| RustCrypto, opt-level z, `-Oz` | 1,348,956 B | **540 kB** |
| RustCrypto, opt-level 3, `-O3` | 1,917,376 B | 686 kB |
| libcrux, opt-level z, `-Oz` | 1,568,657 B | 594 kB |
| libcrux, opt-level 3, `-O3` | 2,356,419 B | 814 kB |

Where the bytes go (twiggy, opt-level z, code and data only):

| part | share |
|---|---|
| openmls | 30% |
| serde_json (openmls' memory storage serializes state as JSON; also the spike's own roster code) | 15% |
| data segments | 10% |
| P-384, P-256, k256 (RustCrypto supports every ciphersuite; we use none of these curves) | 13% |
| ML-KEM, ML-DSA, X-Wing pieces (post-quantum suites, also unused) | about 3% |
| this spike's code | 4% |

So about a third can go: a crypto provider that only does our ciphersuite, and a storage provider
that doesn't use JSON. A rough guess is 350 kB gzipped. It's not needed to ship. Instantiating takes
3 ms in node and 20 ms in Chrome, including the fetch from localhost.

**Ciphersuite: `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (0x0001).**

- Ed25519 is what device keys already sign with, so the certificate, the binding and the MLS leaf
  all use one algorithm and one implementation.
- X25519 is the curve Noise already uses. MLS makes fresh HPKE keys for each leaf, so no Noise key
  is reused in a second protocol.
- It's the suite RFC 9420 requires every implementation to have, and openmls' default.
- ChaCha20 (0x0003) would only matter for bulk message encryption without AES hardware. Messages
  cost 0.1 to 0.3 ms in wasm either way, so it doesn't matter.

**opt-level z costs a lot of speed.** Natively, opt-level z was 10 to 40 times slower than
opt-level 3 (a decrypt took 1.5 ms instead of 0.04 ms). In wasm, the gap is about 2 times
(table below). The size difference is 146 kB gzipped. Ship z for now. Commits stay far under the
one-second budget. Consider O3 if team sizes grow.

## 2. Timings

N is the group size before the operation; the add makes it N+1. Medians of 9 runs. The tree is
"cold": one batch Add filled it, so most parent nodes are blank and commits encrypt to more nodes
than they would in a group that has been running for a while. That's close to the worst case.
Commit times include making and signing the GroupInfo that's uploaded with each commit.
Firefox and WebKit clamp `performance.now()` to 1 ms, so their figures are rounded to whole
milliseconds (0 means under 1 ms). Chrome's are 0.1 ms.

| N | operation | native | node z | node O3 | Chrome z | Chrome O3 | Firefox z | WebKit z |
|---|---|---|---|---|---|---|---|---|
| 2 | join from Welcome | 0.27 | 1.15 | 0.74 | 0.90 | 0.60 | 1 | 1 |
| 2 | external-commit join | 0.45 | 1.83 | 1.10 | 1.50 | 0.80 | 2 | 2 |
| 2 | add-one commit | 0.48 | 2.10 | 1.27 | 1.70 | 1.00 | 2 | 2 |
| 2 | receiver processes add | 0.28 | 1.17 | 0.73 | 1.00 | 0.50 | 1 | 1 |
| 2 | remove-one commit | 0.28 | 1.32 | 0.70 | 1.10 | 0.50 | 1 | 1 |
| 2 | receiver processes remove | 0.17 | 0.80 | 0.50 | 0.70 | 0.30 | 0 | 1 |
| 2 | self-update commit | 0.31 | 1.49 | 0.87 | 1.20 | 0.70 | 1 | 1 |
| 2 | receiver processes update | 0.21 | 0.92 | 0.57 | 0.80 | 0.40 | 1 | 1 |
| 2 | encrypt message | 0.03 | 0.14 | 0.09 | 0.20 | 0.10 | 0 | 0 |
| 2 | decrypt message | 0.04 | 0.18 | 0.11 | 0.20 | 0.10 | 0 | 0 |
| 10 | join from Welcome | 0.58 | 2.55 | 1.51 | 2.10 | 1.20 | 2 | 2 |
| 10 | external-commit join | 0.88 | 4.01 | 2.32 | 3.20 | 1.70 | 3 | 3 |
| 10 | add-one commit | 0.68 | 4.41 | 2.42 | 3.60 | 2.00 | 4 | 4 |
| 10 | receiver processes add | 0.38 | 1.64 | 0.90 | 1.40 | 0.70 | 1 | 1 |
| 10 | remove-one commit | 0.50 | 3.55 | 1.90 | 2.80 | 1.50 | 3 | 3 |
| 10 | receiver processes remove | 0.28 | 1.32 | 0.70 | 1.20 | 0.60 | 1 | 1 |
| 10 | self-update commit | 0.52 | 3.55 | 1.99 | 2.80 | 1.60 | 3 | 3 |
| 10 | receiver processes update | 0.30 | 1.39 | 0.70 | 1.10 | 0.60 | 1 | 1 |
| 10 | encrypt message | 0.03 | 0.15 | 0.09 | 0.20 | 0.10 | 0 | 0 |
| 10 | decrypt message | 0.04 | 0.19 | 0.11 | 0.20 | 0.10 | 0 | 0 |
| 50 | join from Welcome | 2.12 | 12.19 | 5.38 | 7.70 | 4.50 | 7 | 9 |
| 50 | external-commit join | 2.93 | 20.86 | 8.43 | 12.40 | 7.00 | 12 | 14 |
| 50 | add-one commit | 1.46 | 20.64 | 8.15 | 12.60 | 6.80 | 11 | 13 |
| 50 | receiver processes add | 0.66 | 4.69 | 1.60 | 3.10 | 1.40 | 3 | 2 |
| 50 | remove-one commit | 1.15 | 16.45 | 7.07 | 10.70 | 5.90 | 10 | 11 |
| 50 | receiver processes remove | 0.57 | 4.26 | 1.41 | 2.80 | 1.30 | 3 | 3 |
| 50 | self-update commit | 1.15 | 16.87 | 7.20 | 10.70 | 5.90 | 10 | 12 |
| 50 | receiver processes update | 0.59 | 4.68 | 1.45 | 2.80 | 1.30 | 2 | 3 |
| 50 | encrypt message | 0.04 | 0.27 | 0.09 | 0.20 | 0.10 | 0 | 0 |
| 50 | decrypt message | 0.05 | 0.32 | 0.11 | 0.20 | 0.10 | 0 | 0 |

libcrux, for comparison (N=50): native add-one commit 1.96 ms, receiver 0.98 ms, Welcome join 3.12
ms. In node with opt-level z: 30.6, 6.4 and 31.9 ms. It's slower than RustCrypto everywhere here.

Setup (N=50, native): the batch Add of 49 devices took 8 ms and made a 38 kB commit and a 41 kB
Welcome. In Chrome (z) it took 36 ms. A KeyPackage takes 0.1 ms to make (0.3 ms in wasm) and is 758
bytes.

**Sizes on the wire** (what control stores):

| N | add commit | Welcome | GroupInfo | remove commit | update commit | external commit | 58-byte message |
|---|---|---|---|---|---|---|---|
| 2 | 1,747 | 2,475 | 2,337 | 964 | 1,075 | 942 | 192 |
| 10 | 2,474 | 7,819 | 7,681 | 1,725 | 1,801 | 1,141 | 192 |
| 50 | 5,824 | 34,041 | 33,901 | 5,075 | 5,151 | 2,488 | 192 |

The Welcome and GroupInfo carry the whole ratchet tree, about 680 bytes per device. Most of that is
the credential, which here is JSON (certificate plus binding). A binary encoding would roughly halve
it.

## 3. Roster events as MLS

Each channel is one MLS group, and each device is a leaf. Control is the delivery service. Every
roster event is one commit, and every commit says in its AAD which roster state it enacts.

| roster event | MLS | who commits | demo |
|---|---|---|---|
| invite link | the joiner's device makes an **external commit** from the GroupInfo control holds, with roster v+1 (signed by the one-time key) in the AAD | the joiner's own device | yes |
| *Ask me first* | the owner's device writes roster v+1, then commits **Add** with the joiner's KeyPackage and sends the **Welcome** | an owner's device | yes |
| remove a member | roster v+1 without them; one commit **Removes** every leaf of that account | the owner's device that signed it, or any member's device that sees the new roster first | yes |
| remove a device | a revocation in the account's revocation log; one commit **Removes** that leaf | any member's device | yes |
| Lock | roster v+1 with `locked`; one commit **Removes** every non-owner leaf | an owner's device | yes |
| Unlock | roster v+1 without `locked`; the owner **Adds** every member device that has a KeyPackage at control; devices without one rejoin by **external commit** | an owner's device, then each device | rejoin only |
| new device of a member | the device makes an **external commit**; no roster change, since its certificate chains to a member account | the new device | yes |
| a device too far behind | **external commit** again; openmls removes its old leaf in the same commit (same MLS key) | the device | yes |

**openmls supports external commits and exporting GroupInfo.** `MlsGroup::export_group_info(..,
with_ratchet_tree: true)` makes it, and `MlsGroup::external_commit_builder().with_aad(..)` joins from
it. Control has to store the latest GroupInfo, with the tree, which each committer uploads with its
commit (34 kB at 50 devices). The GroupInfo lists every device credential. Control already knows the
roster, so that's no new exposure.

**How the one-time invite key proves the join is allowed.** It never touches MLS. It signs roster
v+1 exactly as it does today (control-e2e.md). The joiner's external commit names v+1 in its AAD.
Every member's device checks v+1 by the existing rules: an owner signed the invite, it's not for an
owner, it hasn't expired or been spent, and nothing else changed. Then it checks that the joining
leaf's certificate chains to the account v+1 adds. The AAD is signed by the joiner's leaf key, and
for encrypted commits it's also inside the AEAD, so control can't change it.

**Lock: remove the leaves, don't use a separate owners-only group.** Removal is the only thing that
stops a device from reading new messages. A second group per channel would mean two places to post
during a lock, and members would still hold the main group's keys. With removal, Lock is one commit
per channel (11 ms in Chrome at 50 devices) plus delivery, which fits the one-second goal. Unlock
can't restore the old leaves, so it adds devices again:

- An owner's device commits Adds for every member device with a KeyPackage at control. Offline
  devices are back in when they return, and nobody has to wait for them.
- Devices without a KeyPackage rejoin by external commit when they come online, with the Unlock
  version in the AAD.
- Control needs a last-resort KeyPackage per device (openmls has the `LastResort` extension). The
  demo only shows the external-commit path.
- Nothing posted during the lock is readable to the people who were locked out. The demo checks this.

### Binding MLS to the roster (the recommended scheme)

**Credential:** a `BasicCredential` whose identity bytes are the device certificate plus an MLS key
binding:

```
DeviceCredential { cert: <the device certificate as control stores it>,
                   binding: { mls_key, sig: Ed25519(device sign key, "illogical mls leaf v1" || mls_key) } }
```

- A custom credential type would work too (`CredentialType::Other(..)` exists), but every leaf
  would then have to list it in its capabilities, and nothing else gets better. Basic is enough,
  because MLS treats the bytes as opaque and the app checks them.
- **The MLS key is separate from the device key.** openmls' `Signer` trait is synchronous, and a
  browser's device key is a non-extractable WebCrypto key that only signs asynchronously. So the
  MLS key is made in wasm and stored with the group state.
- The CLI and daemon could use their file keys directly. The browser can't, so keep one rule for
  all of them.
- That makes the MLS key, and all group state, extractable by script in the page origin. It's the
  same trade as the WebKit wrapped-key fallback (control-e2e.md): keep it in IndexedDB encrypted
  under a non-extractable AES-GCM key. The difference is that an XSS can always read channel
  plaintext in the page anyway.

**AAD on every commit:** `CommitAad { roster: { v, hash }, revocations: <count of the revocation log
applied> }`.

**What each device checks before merging a commit** (`check_commit` in `src/client.rs`):

1. The AAD names a roster version this device has verified, and a revocation count it has. The
   state is no older than what the group has already applied, so a commit can't roll back a removal.
2. Every leaf in the new epoch's tree is allowed:
   - its certificate chains to an account in that roster;
   - its MLS key is the bound one;
   - it isn't revoked within the named count;
   - it's an owner if the roster is locked.
3. Every leaf the commit drops is one the roster no longer allows, so no member can kick another
   without a roster change.
4. A device that is itself being removed (it doesn't get the new tree) goes along only if the roster
   no longer allows it.

A commit that fails isn't merged. The check uses only signed data that the commit names, so every
honest device reaches the same answer, and no fork comes out of it. In the demo, control's forged
"alice" device and bob's attempt to kick carol were both refused by every device. The forged device
read 0 messages.

**Open detail for M62: Welcome joiners.** A device that joins from a Welcome doesn't see earlier
commits, so it starts "applied" at the newest roster it has verified. If that's newer than what the
group has applied, it could refuse a commit that others accept. The fix is to put the
`(roster v, hash, revocations)` state in a GroupContext extension that each enacting commit updates.
The state is then part of the epoch, and a joiner gets it in the Welcome and GroupInfo. External
commits can't change GroupContext extensions, so an invite join names v+1 in its AAD while the
context still says v, and the next member commit catches the context up. The spike didn't build
this.

## 4. An offline device

`cargo run --release --bin scenarios offline`:

- **Missed 3 commits and 4 messages, then processed them in order.** Every message and commit was
  processed, nothing failed. It ended at the same epoch, with the same epoch authenticator as the
  others. Processing in order is enough, because each message is decrypted at its own epoch as the
  device steps through.
- **What it can't decrypt: a message from an epoch it has already left.** openmls keeps past
  epochs' message secrets only per `max_past_epochs` (default 0). A message from epoch 1 that
  reached bob at epoch 4:

  | max_past_epochs | result |
  |---|---|
  | 0 | can't ("Generation is too old to be processed") |
  | 2 | can't |
  | 3 | read |

  This isn't only an offline problem. In the same run, alice committed while bob's message was in
  flight, and with 0 she lost it. **Use 3 to 5.** The cost is keeping that many epochs of sender
  ratchet secrets, which weakens forward secrecy for those epochs.
- **Too far behind: it rejoins.** Control kept only the last 2 commits, so carol (at epoch 1, with
  the group at 6) couldn't bridge the gap. Each log entry failed with "epoch differs". She made an
  external commit from the current GroupInfo, and openmls removed her old leaf in the same commit
  (same MLS key, so the group still had 1 carol). She read everything after that. The 5 messages
  from the gap stay unreadable unless a device that has them re-shares them. M62 already leaves that
  out.
- **When to rejoin instead of replay:** if control no longer has the commit for the device's epoch.
  Control should keep commits at least as long as messages. A device that comes back after the
  roster removed it can't rejoin at all; the roster check refuses it.
- **Stale leaves weaken post-compromise security.** A device offline for weeks still holds the leaf
  keys it had. Suggest that a member's device removes a device it hasn't heard from in some period,
  such as 30 days. It can rejoin when it returns.

## 5. A hostile delivery service

`cargo run --release --bin scenarios hostile`:

| control does | what happens | demo |
|---|---|---|
| replays a message | refused: "The requested secret was deleted to preserve forward secrecy." | yes |
| replays an old commit | refused: "Message epoch differs from the group's epoch." | yes |
| delivers commits out of order | the later one is refused (epoch differs) and the state doesn't change; in order, both apply | yes |
| reorders messages within an epoch | all read (out-of-order tolerance of 20 here) | yes |
| drops a commit for one device | that device can't read later messages (epoch differs). A message for an epoch ahead of its own tells it a commit is missing | yes |
| two commits for the same epoch, honest control | control keeps the first and refuses the second ("stale"); the loser drops its pending commit, applies the winner, and commits again | yes |
| forks: sends alice's commit to carol and bob's to dave | two branches at the same epoch number with different epoch authenticators; dave can't decrypt carol ("AEAD decryption" error) | yes |
| adds a device of its own by external commit | every device refuses it (roster check) | yes (demo) |
| withholds a new roster version, or the commit that enacts it | the removal is delayed, which is the risk control-e2e.md already accepts | — |
| drops everything for someone | denial of service, nothing more | — |

**Clients must merge their own commit only after control accepts it.** That's what makes the
"first wins" rule safe. The spike's `add`, `remove` and `self_update` merge right away, which is fine
with a single committer. The concurrent-commit test uses the stage, submit, then merge-or-clear
path.

**Detecting a fork.** A fork never gives control plaintext, because both branches are real MLS
epochs among real members. It splits the conversation: each side sees the other go quiet.
Recommended:

1. **Watch the epoch numbers.** Messages carry their epoch in the clear. A message for an epoch
   ahead of yours means a commit is missing; ask control for it. If control has none, rejoin.
2. **Equal epoch numbers with an AEAD failure mean a fork.** Treat it as one, not as noise.
3. **Compare epoch authenticators over channels control can't forge.**
   - Each device reports `(channel, epoch, first 8 bytes of SHA-256(epoch authenticator))` to the
     team daemons it already has Noise sessions with.
   - Daemons already verify rosters. They compare the reports from different members, and if two
     disagree at the same epoch, they raise it.
   - Control can drop those reports, but it can't forge them.
   - In the fork test, the two branches showed different authenticators at epoch 8 (for example
     `eb84782aa84f` and `c043cf3d6f3b`; the values differ each run).
4. **Show a channel fingerprint** (the epoch authenticator as words) in channel info, for people who
   want to compare in person.
5. **Resolve by control's order.** The branch whose commit control logged first wins. Devices on the
   other branch rejoin it by external commit and lose what they posted on their branch; the client
   should tell them so. In the test, all four devices ended at epoch 10 with one authenticator, and
   dave's next message reached alice.
6. openmls 0.9 also has a `fork-resolution` feature with helpers to remove and re-add the members
   on the wrong branch, or to start a new group. Rejoining by external commit is simpler and works
   with the roster check unchanged. The spike didn't try the feature.

## 6. Should team machines be channel members?

**Recommendation: not by default. Agents stay on pane and session threads.** Later, an owner can
add one machine to one channel on purpose.

- **A machine as a member puts the team's channels on that machine's disk.** The daemon's MLS key
  and group state are files. Anyone with a shell as the daemon's user, or root, reads every message
  and can post as the machine. Every agent running there can too.
- **Roles are about access to panes on the machine.** A role decides what someone can do on a
  machine. Channel membership would send what people say to every team machine, which is a much
  wider spread.
- **Agents already have a way to post.** A person's own CLI device is already a member, so an agent
  running in that person's terminal can post as them, with their access.
- **Team machines aren't needed as commit helpers.** Machines are always online, which would make
  them good at committing roster changes quickly and keeping the tree fresh. But any member device
  can commit a roster change (the check is deterministic), so nothing depends on machines.
- **If it's added later:**
  - an owner turns it on for a named channel, and it's a roster entry of its own, so the roster
    check covers it;
  - the UI marks the machine's messages as from a machine;
  - removing it is an ordinary removal commit.

## 7. The demo (done-when)

`cargo run --release --bin demo` (or `node run-wasm.mjs wasm-rustcrypto demo`, or in a browser via
`bench.html`). Three accounts (alice, bob, carol) and four devices talk through the stub delivery
service. Every commit goes through the roster check. In order, the transcript shows:

1. alice founds the team and `#general`;
2. *Ask me first* admits bob (Add plus Welcome), and alice and bob talk;
3. carol joins by invite link (external commit, roster v3 signed by the one-time key);
4. alice adds her phone as a new device (external commit);
5. control's forged device is refused, and bob's attempt to kick carol is refused;
6. **alice removes bob mid-conversation.** bob takes everything control stored after his removal
   and reads 0 of 2 messages:
   - a patched client that ignores the removal gets "Message epoch differs";
   - the honest client applies its removal (the group goes inactive), then gets "Tried to use a
     group after being evicted";
7. alice's phone is revoked and removed by carol's device, and reads nothing after that;
8. Lock removes carol, who reads nothing while locked;
9. Unlock lets carol rejoin from the current epoch. She reads new messages but still can't read the
   locked-period one.

At the end, control holds 18 log entries (9 commits, 9 messages, 12 kB), 6 signed rosters, 1
revocation and 1 GroupInfo, and no private key.

## What this didn't check

- **Persistence.** Everything uses openmls' in-memory storage. In the browser, group state has to go
  to IndexedDB, and openmls' `StorageProvider` trait is synchronous, so it needs a write-behind
  cache.
  - If the state rolls back (a tab closes before the write lands), the device has lost or reused
    secrets and should rejoin.
  - Rule: persist before acknowledging a commit to control.
- **A real phone.** No iOS Safari or Android numbers. WebKit on Linux stands in for Safari, and
  it's within 1 to 2 ms of Chrome.
- **WebKitGTK and WebView2.** The desktop app's webviews weren't run. WebKit and Chrome engines both
  work, so no trouble is expected.
- **Warm trees and large groups.** Only cold trees, and only up to 50 devices (51 during an add).
- **The GroupContext binding for Welcome joiners** (section 3) and the fork-report channel to
  daemons (section 5) are designs, not code.
- **Multiple channels.** Each channel is its own group, so a roster change is one commit per
  channel. At 11 ms per commit in Chrome, 20 channels take about 0.2 s of CPU. Not measured.
- **The identity model is simplified.** Account roots sign device certificates directly, and
  revocations are signed by the root. The real chain (device approves device) only changes how
  `DeviceCert::verify` walks the chain.
