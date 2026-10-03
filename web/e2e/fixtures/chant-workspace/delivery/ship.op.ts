// A toy gated op: `chant run ship` stops at the gate (exit 3) until someone
// approves it, then walks through.
import { Op, phase, gate, shell } from "@intentius/chant/op";

export default Op({
  name: "ship",
  overview: "Wait for a person, then ship",
  phases: [
    phase("Approve", [gate("approve-ship", { timeout: "24h", description: "Ship the toy" })]),
    phase("Ship", [shell("echo shipped")]),
  ],
});
