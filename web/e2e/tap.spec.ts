// Where a tap on a notification goes (src/tap.ts, used by the service
// worker): the pane, the card, and a mention's thread (M61).

import { expect, test } from "@playwright/test";
import { tapUrl } from "../src/tap";

test("a tap opens the pane, and a mention's thread over it", () => {
  expect(tapUrl({ pane: 3 })).toBe("/#pane=3");
  expect(tapUrl({ pane: 3, thread: "pane-3" })).toBe("/#pane=3&thread=pane-3");
  expect(tapUrl({ pane: 3, thread: "session-2" })).toBe("/#pane=3&thread=session-2");
});

test("through control the pane is on its daemon", () => {
  expect(tapUrl({ pane: 3, daemon: "d1", thread: "pane-3" })).toBe("/#pane=d1.3&thread=pane-3");
  expect(tapUrl({ pane: 3, daemon: "d1" })).toBe("/#pane=d1.3");
});

test("a thread that isn't a pane's or a session's is dropped", () => {
  for (const thread of ["pane-x", "pane-3/../x", "other-3", "pane-3&x=1", "", "pane-"]) {
    expect(tapUrl({ pane: 3, thread })).toBe("/#pane=3");
  }
});

test("something that wants you opens at its card", () => {
  expect(tapUrl({ pane: 3, reason: { kind: "failed" } })).toBe("/#swarm=3");
  expect(tapUrl({ pane: 3, daemon: "d1", reason: { kind: "failed" } })).toBe("/#swarm=d1.3");
});

test("with no pane it opens the app", () => {
  expect(tapUrl({})).toBe("/");
});
