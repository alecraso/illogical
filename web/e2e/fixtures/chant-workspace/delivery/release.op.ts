// A second toy gated op, for a second gate after the first is approved.
import { Op, phase, gate, shell } from "@intentius/chant/op";

export default Op({
  name: "release",
  overview: "Wait for a person, then release",
  phases: [
    phase("Approve", [gate("approve-release", { timeout: "24h", description: "Release the toy" })]),
    phase("Release", [shell("echo released")]),
  ],
});
