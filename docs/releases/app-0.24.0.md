# illogical app 0.24.0

The first desktop app released on its own lane (#393). It carries the
newest daemon for a machine that has none, and never replaces one it
finds (#392): the daemon updates itself.

- **This app updates itself** (#419): *Check for Updates…* in the app
  menu and the tray. Apps from 0.23 and older have no updater, so install
  this one once by hand.
- **The Daemon menu** (#322) in the tray, the app menu and the Dock:
  version, state, service, control's standing; Restart, Stop, Start and
  Open log.
- **Too old a daemon gets the setup page** (#317, #390): one below
  0.19.0, or one whose protocol doesn't match, with the update for how
  it was installed.
- **macOS:** Cmd-Q quits, Cmd-H hides (#320); *This machine* brings its
  window forward instead of adding a tab, and the page's bar sits below
  native tabs (#323); Cmd-U attaches files (#417).
- **Any page can be dragged** (#316): the app supplies window dragging
  itself, so an older daemon's page moves too.
- A notification when control drops this machine (#325).
- The arm64 Linux app is back (#287).

Not notarized yet (#177): from a browser download, open it once with
*Open Anyway* in System Settings › Privacy & Security, or install it with
`curl -fsSL https://illogical.widgets.wtf/install.sh | sh`.
