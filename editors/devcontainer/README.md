# Arugula dev container feature

Puts VS Code (or Cursor) in a dev container in your Arugula swarm.

The extension runs inside the container, so it can't see the Arugula daemon on the host by itself. This feature mounts the daemon's editors' socket directory (`~/.local/state/arugula/editors` on the machine running the container) at `/run/arugula`, sets `ARUGULA_SOCK`, and asks for the extension. That socket only lets an editor join the swarm; the container can't drive the daemon through it.

```jsonc
// .devcontainer/devcontainer.json
"features": {
  "ghcr.io/arugula-salad/arugula/arugula:0": {}
}
```

Until it's published, copy `src/arugula` into `.devcontainer/arugula` and use `"./arugula": {}`. It was never published under its old name (`illogical`), so there's no old feature id to keep; a copy of the old `src/illogical` keeps working on a machine whose daemon started out as illogical, since its socket is still where it was.

On a machine whose daemon started out as illogical, the state directory is still `~/.local/state/illogical`, and `~/.local/state/arugula` is a link to it, so the mount's source is there either way (on Linux and macOS). The extension it asks for, `arugula.arugula-editor`, comes from the marketplace once it's published there; until then, write the VSIX on the host (`arugula editors vsix`) and install it in the container.

The socket is 0600 and belongs to you on the host: the container's user needs your uid (the usual `vscode` user is 1000) or root.
