# S5: Ghostty's snapshot format (GHOSTSNP) for checkpoints
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s5-snapshot/<file>`.

Run 2026-10-01 on geek. **Result: pass. Use GHOSTSNP for checkpoints**, with
the caveats under "Format stability".

## Setup

- `libghostty-vt` from libghostty-rs `master` (`8953a74`, 2026-09-28): the
  first version that binds `ghostty_snapshot_*`. It is unreleased; crates.io
  0.2.2 does not have it.
- It pins Ghostty `22d13172` (2026-08-06), which needs **Zig 0.16.0**
  (`.mise.toml` here). The daemon's crates still pin 0.2.2 / Zig 0.15.2.
- The API changed from 0.2.2:
  - `Terminal::new(cols, rows)`;
  - scrollback limits are set separately as `set_scrollback_max_bytes` and
    `set_scrollback_max_lines`, which confirms S1's finding that the old limit
    was in bytes;
  - `encode_snapshot_alloc` returns `Option<Bytes>`.

```
LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast mise exec -- cargo run --release
```

## Results

**Fidelity.** All seven S1 fixtures round-trip with **no fix-ups**. The
harness feeds a fixture into A, encodes, decodes into B, and compares every
cell, cursor, pending wrap, 17 modes, title, cwd, palette, Kitty flags,
scrollback, plain and VT text. For full-screen apps it also leaves the alt
screen on both and compares the primary screen underneath. It checks the
saved cursor (DECSC) by restoring it on both, which the formatter could not
carry at all. Encoding doesn't disturb the source terminal.

**Size and time** (median of 20, ms; release build):

| | GHOSTSNP bytes | encode | decode | formatter VT bytes | format | replay VT |
|---|---|---|---|---|---|---|
| less | 1,528 | 0.008 | 0.052 | 5,756 | 0.008 | 0.075 |
| modes | 1,767 | 0.008 | 0.030 | 5,827 | 0.007 | 0.069 |
| nvim | 15,182 | 0.049 | 0.123 | 15,189 | 0.016 | 0.099 |
| seq (5k lines) | 35,395 | 0.27 | 0.64 | 34,517 | 0.44 | 0.87 |
| 200k colored lines (64,511 kept) | 11,068,899 | 38.8 | 42.7 | 3,943,184 | 11.1 | 21.7 |

The big case compressed with zstd, CLI timings:

| | zstd -1 | zstd -3 | zstd -9 |
|---|---|---|---|
| GHOSTSNP | 151,589 B | **147,091 B (75x, ~9 ms)** | 118,672 B |
| formatter VT | 76,596 B | 58,319 B | 47,149 B |

Upstream does not compress; the brief's mention of zstd is not in the format.
Checkpoints should be zstd-compressed by us.

**Incremental decode.** For the 11 MB snapshot:

- `ready()` returns a terminal whose visible screen is identical to the
  source after **0.31 ms**, holding 269 scrollback rows.
- The remaining 166 history pages are prepended over 70 ms, ending with
  exactly the source's 64,511 rows and identical full text.
- `history_rows_primary()` is only available after READY (the harness asked
  too early and got none).

This is the visible-first attach and "unpark" path: show READY, stream the
history.

**Continuation.** The terminal was snapshotted halfway through `ESC [ 1 ; 3`.
The decoded terminal, fed the rest (`1mred…`), ends identical to the source.
This needs `set_continuation_max_bytes` on the live terminal **before** the
input arrives; without it, encoding mid-sequence fails with `InvalidValue`
rather than producing a wrong snapshot.

**Corruption.** A single flipped byte and a truncated snapshot both fail to
decode with `InvalidValue`.

## Format stability (the catch)

- Upstream says version 1 "does not yet carry a binary-compatibility
  guarantee", and it means it. Between our pinned `22d13172` and Ghostty
  `main` (`59c2dc0`, 2026-10-01), commit `219173ab3` "terminal/snapshot:
  remove BLAKE3 digests" changed the READY record. `envelope.version` is
  still `1`.
- So a checkpoint written by one Ghostty build may not decode in another,
  and the version field won't say so. This is inferred from the commit; I
  did not build `main` and try it.
- Twelve more snapshot commits followed (vectorized codecs, style tables,
  retained continuations).

**Consequences for M2:**

1. Each checkpoint records the Ghostty commit (`build_info`) that wrote it.
   On a mismatch, or any decode error, it is ignored, and the pane's VT state
   is rebuilt by replaying the log tail. The raw-bytes log is the source of
   truth; checkpoints are a cache.
2. Checkpoints are written zstd-compressed with a small header (magic,
   Ghostty commit, log offset, CRC of the compressed body).
3. Wire snapshots to xterm.js stay formatter VT bytes, with the S1 fix-ups,
   until a ghostty-web client exists.
   - The decoder's READY/HISTORY split can still drive `part: screen|history`
     frames: format the READY terminal for the screen part and the history
     afterwards.
   - The formatter's alt-screen limitation still applies there.
4. Moving the daemon to libghostty-rs `master` means Zig 0.16 for every
   build. Do it at the start of M2, in one step, and re-run the S1 fixtures
   and the vt tests. libghostty-rs has not released it yet; pin a rev.
