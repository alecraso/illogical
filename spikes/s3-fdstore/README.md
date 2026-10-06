# S3: PTYs survive daemon restarts (fd store + per-pane scopes)
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s3-fdstore/<file>`.

Run 2026-10-01 on geek (systemd 259, user manager). **Result: pass.**

`src/main.rs` (about 150 lines):
1. On first start it opens a PTY.
2. It runs `bash` via `systemd-run --user --scope -- setsid -c bash` with the
   slave as stdio, so the shell sits in its own `s3-pane-<pid>.scope`, outside
   the service's cgroup.
3. It sends the master to systemd with `FDSTORE=1`, `FDNAME=pane-1`,
   `FDPOLL=0` (SCM_RIGHTS over `$NOTIFY_SOCKET`).
4. On a later start it takes the master back from `LISTEN_FDS` /
   `LISTEN_FDNAMES`.
5. Each start asks the shell for `$$` through the PTY.

Run it as a transient service (nothing installed):

```
systemd-run --user --unit=s3-fdstore -p Type=notify -p NotifyAccess=main \
  -p FileDescriptorStoreMax=16 -p Restart=on-failure -p RestartSec=200ms \
  $PWD/target/release/s3-fdstore
journalctl --user -u s3-fdstore -o cat -f
```

| Case | Result |
|---|---|
| `systemctl --user restart` x2 | master returned as `pane-1` on fd 3, same shell pid answers |
| `kill -9` main pid with `Restart=on-failure` | same: store kept across the crash restart, shell untouched |
| `systemctl --user stop` | store dropped, master closed, shell gets SIGHUP and its scope goes away |

## Findings for illogicald

- **Set `O_CLOEXEC` on PTY masters.** `nix::pty::openpty` doesn't. Without it
  the shell inherits its own master, never sees a hangup, and survives a clean
  stop as an orphan (the first run of this spike leaked one).
- **Unique scope names per pane.** A leftover scope with the same name makes
  `systemd-run --scope` fail and the pane never starts.
- **`stop` kills panes; `restart` and crashes don't.** That's the systemd 259
  default `FileDescriptorStorePreserve=restart`. Upgrades must use `restart`,
  not stop+start. Setting `FileDescriptorStorePreserve=yes` would keep panes
  across a stop too; then `illogical kill-server` would need to clear the store
  explicitly (`FDSTOREREMOVE=1`).
- **`Restart=on-failure` is required** for crash survival; the M2 unit has it.
- **A restarted daemon is not the shell's parent** (the shell is reparented
  to `systemd --user`), so it can't `waitpid`. The exit-status shim from PLAN
  M2b is needed. Alternatively, a pidfd opened by pid after restart gives
  exit *notification* (not the status), guarded by start time against pid
  reuse.
- Not covered: VT state across a restart (it comes back from checkpoint + log
  tail, per the plan), and many fds (`FileDescriptorStoreMax` defaults to 0;
  set it well above the expected pane count).
