// illogical web client: tabs and splits of terminals owned by the daemon.

import { render } from "preact";
import "./style.css";
import { Client } from "./client";
import { directory } from "./hosts";
import { App } from "./ui/app";
import { measureCell } from "./ui/cells";
import { registerWorker } from "./push";

// One client for the host shown (M4a); switching hosts closes it and opens
// one to the other daemon, so hidden hosts hold no connection.
let client = new Client(directory.base());

// The visible height excludes a phone's on-screen keyboard, so the key bar
// sits just above it.
const vv = window.visualViewport;
const fitViewport = () => document.documentElement.style.setProperty("--app-height", `${vv?.height ?? innerHeight}px`);
vv?.addEventListener("resize", fitViewport);
fitViewport();

const cell = await measureCell();
const root = document.getElementById("root")!;
const draw = () => render(<App key={client.base} client={client} cell={cell} />, root);
draw();
client.connect();

directory.subscribe(() => {
  const base = directory.base();
  if (base === client.base) return;
  client.close();
  client = new Client(base);
  draw();
  client.connect();
});
void directory.refresh();

// Opened from a notification (`#pane=N`), or told to by the service worker.
// Notifications come from the home daemon, so show it first.
const openPane = (pane: number) => {
  if (directory.home !== null) directory.select(directory.home);
  const go = () => {
    if (!client.info(pane)) return false;
    client.setActive(pane);
    return true;
  };
  if (!go()) {
    const off = client.subscribe(() => go() && off());
  }
};
// A pane opened on the home daemon from elsewhere (a sandbox shell).
window.addEventListener("illogical:open-pane", (e) => openPane((e as CustomEvent<number>).detail));
const fromHash = /^#pane=(\d+)$/.exec(location.hash);
if (fromHash) {
  openPane(Number(fromHash[1]));
  history.replaceState(null, "", "/");
}
void registerWorker(openPane);

// For end-to-end tests.
Object.assign(window, {
  __illogical: {
    get client() {
      return client;
    },
    hosts: directory,
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
