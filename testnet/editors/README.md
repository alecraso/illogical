# testnet/editors: VS Code over Remote-SSH (M28)

M28 was built and tested with code-server standing in for VS Code over
Remote-SSH (`web/e2e/editor-swarm.spec.ts`). This runs the real thing:

- `@vscode/test-electron` downloads VS Code (stable, into
  `~/.cache/arugula/vscode-test`; `ARUGULA_VSCODE_VERSION` pins one),
  and its CLI installs Microsoft's Remote-SSH from the Marketplace into a
  user-data and extensions directory of the test's own. The person's VS
  Code is never read or written.
- Playwright launches it as an Electron app on
  `vscode-remote://ssh-remote+m28-box/home/illo/shop`, with Remote-SSH
  pointed at `testnet/editors/.state/ssh_config` and
  `remote.SSH.localServerDownload: always` (the client fetches the VS Code
  server and copies it over, as for a box with no way out).
- `m28-box` is a Debian container (`box/Dockerfile`: sshd, Node for the
  debugger, Python for the stand-in Claude Code, socat) running the static
  `arugulad` as `illo` in its usual state directory. Arugula's
  extension is installed into the box's VS Code server from the daemon's
  own VSIX (`arugula editors vsix`), and the window reloads.
- The phone is a Pixel-sized page on the box's daemon.

```sh
just testnet-editors     # static build, then web/e2e/editor-remote-ssh.spec.ts
```

The spec is skipped unless `ARUGULA_TESTNET_EDITORS=1` (the recipe sets
it); it brings the box up and down itself (`ARUGULA_TESTNET_KEEP=1`
leaves it). `box.sh up|down` does it by hand. Ports: ssh on
`127.0.0.1:17751`, the daemon on `127.0.0.1:17752`.

What it checks, from M28's "not covered":

- the extension, in VS Code's remote extension host over Remote-SSH, joins
  the box's daemon only when asked and reports `app: vscode`,
  `remote: ssh-remote`;
- the phone follows its cursor (44 ms from Go to Line on this Mac) and its
  typing, and VS Code's status bar says someone follows;
- a breakpoint in the box's Node, through VS Code's js-debug, is a card on
  the phone's rail, and Continue runs it to the end;
- an edit proposed by the stand-in Claude Code in a pane on the box is
  accepted from the phone's rail and lands in the file there.

It needs Docker: without it the recipe fails, unless `ARUGULA_SKIP_DOCKER=1`,
which skips and says nothing ran.

## Where it runs, and what it doesn't cover

- macOS: VS Code opens a window on the logged-in session's screen for the
  half minute the spec takes. Nobody touches it.
- Linux: Electron needs a display; the recipe runs it under `xvfb-run`
  when there's no `DISPLAY` (the runner needs `xvfb` and VS Code's
  libraries: libnss3, libgtk-3-0, libgbm1, libasound2).
- It downloads VS Code, the VS Code server and Remote-SSH, so it needs the
  network and isn't part of `just check`.
- Cursor's own Remote-SSH, the Dev Containers extension, and publishing the
  extension to the Marketplace and Open VSX are still not covered. The
  Claude Code in the pane is still the stand-in (`fake_claude.py`).
