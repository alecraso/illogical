# Arugula app 0.26.0

**illogical is now Arugula.** The app is `Arugula.app` (on Windows,
Arugula in the Start menu). Update with *Check for Updates…*.

- **Approve the app's window again, once.** Its identifier changed
  (`io.arugula.desktop`), so its window has a new key: approve it as you
  approve a browser. Its settings come along.
- **The old app goes.** On macOS the app stops the old app's daemon agent
  and runs the daemon from its own; on Windows the installer removes the
  old app; the Linux packages replace it.
- `arugula://` links open it, and old `illogical://` links still do.
- It finds a daemon installed as `arugulad` or as `illogicald`.

Not notarized yet (#177): from a browser download, open it once with
*Open Anyway* in System Settings › Privacy & Security.
