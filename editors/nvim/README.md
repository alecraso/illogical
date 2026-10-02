# illogical.nvim

Your nvim in your [illogical](https://illogical.widgets.wtf) swarm. Core nvim (0.10 or later), no dependencies; nvim-dap's debugger stops show too if it's installed.

Install it like any plugin from this directory, e.g. with lazy.nvim:

```lua
{ dir = "~/src/illogical/editors/nvim" }
```

or copy `editors/nvim` onto your `runtimepath`.

- `:IllogicalJoin` shows the current folder in the swarm and remembers it: nvim joins by itself next time it starts there.
- `:IllogicalLeave` takes it out at once, and forgets it.
- `:IllogicalStatus` says whether it's in, and how many follow it. `require("illogical").status()` is the same for a statusline.

It talks to the illogical daemon on the same machine (`$ILLOGICAL_SOCK`, else `~/.local/state/illogical/sock`). Over ssh, nvim runs on the remote machine, so that's the daemon there. Set `vim.g.illogical = { socket = "…" }` before the plugin loads to use another socket.
