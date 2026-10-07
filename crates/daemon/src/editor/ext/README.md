# Arugula for VS Code and Cursor

Puts your editor in your [Arugula](https://illogical.widgets.wtf) swarm, beside your terminals and agents.

- **It joins when you ask.** Run *arugula: Show this workspace in the swarm*. It's remembered for the folder; *Take this workspace out of the swarm* removes it at once.
- **Others see where you are.** The swarm shows the editor as a tile in its project, with the file you're in, the lines around your cursor, errors and unsaved files.
- **Following.** Anyone you let see the machine can follow your cursor across files, read-only, from a browser or a phone. The status bar says when someone is following.
- **Things that need you go on the rail.** The debugger stopping at a breakpoint (with Continue), errors after a save, and a merge conflict.
- **Remote-SSH and dev containers.** The extension runs where the files are and talks to the Arugula daemon on that machine. In a dev container, mount the daemon's state directory (the Arugula dev container feature does).

It talks only to the Arugula daemon on its own machine, over a local socket. Your files and cursor go only to the people following you, over Arugula's end-to-end channels.

It also brings Arugula's colour theme.

It was called illogical before, and what you set up then keeps working: `illogical.*` settings are read when the `arugula.*` ones aren't set, folders that joined stay joined, and the illogical extension (`illogical.illogical-editor`) is uninstalled when this one starts, since both would join.
