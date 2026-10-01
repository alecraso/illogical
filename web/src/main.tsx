// illogical web client: tabs and splits of terminals owned by the daemon.

import { render } from "preact";
import "./style.css";
import { Client } from "./client";
import { App } from "./ui/app";
import { measureCell } from "./ui/cells";
import { registerWorker } from "./push";

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

// Opened from a notification (`#pane=N`), or told to by the service worker.
const openPane = (pane: number) => {
  const go = () => {
    if (!client.info(pane)) return false;
    client.setActive(pane);
    return true;
  };
  if (!go()) {
    const off = client.subscribe(() => go() && off());
  }
};
const fromHash = /^#pane=(\d+)$/.exec(location.hash);
if (fromHash) {
  openPane(Number(fromHash[1]));
  history.replaceState(null, "", "/");
}
void registerWorker(openPane);

// For end-to-end tests.
Object.assign(window, {
  __illogical: {
    client,
    cell,
    text: (pane: number) => client.panes.get(pane)?.view.text() ?? client.blocks.get(pane)?.view.text() ?? "",
    screen: (pane: number) => client.panes.get(pane)?.view.screen() ?? "",
    size: (pane: number) => {
      const v = client.panes.get(pane)?.view;
      return v ? [v.cols, v.rows] : null;
    },
    offset: (pane: number) => client.panes.get(pane)?.offset ?? null,
    selection: (pane: number) => client.panes.get(pane)?.view.selection() ?? "",
  },
});
