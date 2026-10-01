// illogical web client: tabs and splits of terminals owned by the daemon.

import { render } from "preact";
import "./style.css";
import { Client } from "./client";
import { App } from "./ui/app";
import { measureCell } from "./ui/cells";

const client = new Client();

// The visible height excludes a phone's on-screen keyboard, so the key bar
// sits just above it.
const vv = window.visualViewport;
const fitViewport = () => document.documentElement.style.setProperty("--app-height", `${vv?.height ?? innerHeight}px`);
vv?.addEventListener("resize", fitViewport);
fitViewport();

const cell = await measureCell();
render(<App client={client} cell={cell} />, document.getElementById("root")!);
client.connect();

// For end-to-end tests.
Object.assign(window, {
  __illogical: {
    client,
    cell,
    text: (pane: number) => client.panes.get(pane)?.view.text() ?? "",
    screen: (pane: number) => client.panes.get(pane)?.view.screen() ?? "",
    size: (pane: number) => {
      const v = client.panes.get(pane)?.view;
      return v ? [v.cols, v.rows] : null;
    },
    offset: (pane: number) => client.panes.get(pane)?.offset ?? null,
  },
});
