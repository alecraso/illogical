# Arugula app 0.26.2

- **Mac updates work** (#533): the update archive no longer carries
  macOS's `._` metadata files, which the updater couldn't unpack
  ("failed to unpack `._Arugula.app`"). Macs on the app 0.24 to 0.26.1
  can now update with *Check for Updates…* (to this release).
- **Windows: updating from the illogical app removes it** (#533). The
  installer closes the old app before running its uninstaller, which does
  nothing while the app is open, and keeps that uninstaller unless the
  removal worked.

Not notarized yet (#177): from a browser download, open it once with
*Open Anyway* in System Settings › Privacy & Security.
