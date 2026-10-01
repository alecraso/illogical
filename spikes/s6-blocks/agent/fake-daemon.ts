// Stand-in for illogicald's side of the permission relay: a Unix socket that
// receives {id, args} lines from perm-mcp.ts and answers {id, decision}.
// usage: bun fake-daemon.ts <sock> never|allow
import { unlinkSync } from "node:fs";

const [path, mode = "allow"] = process.argv.slice(2);
try { unlinkSync(path); } catch {}
const t0 = Date.now();
const log = (o: object) => console.log(JSON.stringify({ t: Date.now() - t0, daemon: process.pid, ...o }));

Bun.listen({
  unix: path,
  socket: {
    open() { log({ ev: "mcp-connected" }); },
    data(s, d) {
      for (const line of new TextDecoder().decode(d).split("\n").filter(Boolean)) {
        const m = JSON.parse(line);
        log({ ev: "request", id: m.id, tool: m.args?.tool_name, input: m.args?.input });
        if (mode === "allow") {
          const decision = { behavior: "allow", updatedInput: m.args.input };
          s.write(JSON.stringify({ id: m.id, decision }) + "\n");
          log({ ev: "answered", id: m.id, decision });
        }
      }
    },
    close() { log({ ev: "mcp-gone" }); },
  },
});
log({ ev: "listening", path, mode });
