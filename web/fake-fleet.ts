// `just fake-fleet`: three throwaway daemons with scripted work
// (e2e/fake-fleet.ts) to try the swarm by hand, at http://127.0.0.1:7730/#swarm.
// Keys: t = make trouble (a batch of failures on build-02), a = an agent
// asks, q = an agent asks a question, d = a long build finishes, x = quit.

import { FakeFleet } from "./e2e/fake-fleet.ts";
import { signIn } from "./e2e/local-token.ts";

const f = new FakeFleet();
await f.machine("workstation", 7730);
await f.machine("build-01", 7731);
await f.machine("build-02", 7732);
await f.populate();
// Its daemons share a token: the link signs the browser in to all three.
console.log(`swarm: ${signIn("http://127.0.0.1:7730", "/#swarm")}\n   (t trouble, a ask, q question, d done, x quit)`);
const bye = () => {
  f.stop();
  process.exit(0);
};
process.on("SIGINT", bye);
process.stdin.setRawMode?.(true);
process.stdin.on("data", (b) => {
  const k = b.toString();
  if (k === "t") void f.trouble("build-02", 5);
  if (k === "a") void f.agentAsks("build-01", "illogical", "cargo test -p illogical-vt");
  if (k === "q") void f.agentQuestion("workstation", "hal0");
  if (k === "d") void f.finish("workstation");
  if (k === "x" || k === "\u0003") bye();
});
