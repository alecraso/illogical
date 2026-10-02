// illogical web client: tabs and splits of terminals owned by the daemon.

import { render } from "preact";
import "./style.css";
import { Client } from "./client";
import { directory } from "./hosts";
import { App } from "./ui/app";
import { measureCell } from "./ui/cells";
import { registerWorker } from "./push";
import { ControlSession, detectControl } from "./control";
import { ControlGate, ControlOverlay, controlMenuItems, NoMachines, useControl } from "./ui/control";
import { setHostMenuExtras } from "./ui/hosts";
import { useSubscribe } from "./ui/hooks";

// Served by illogical control (M17), not a daemon: sign in, enroll this
// browser, and reach daemons through end-to-end channels.
const info = await detectControl();
const session = info ? new ControlSession(info) : null;
if (session) {
  setHostMenuExtras(() => controlMenuItems(session));
  session.subscribe(() =>
    directory.setControl(
      session.daemons.map((d) => ({
        name: d.name,
        id: d.id,
        urls: d.urls,
        transport: "control" as const,
        added_ms: 0,
        last_seen_ms: d.last_seen,
        status: d.online ? "online" : "offline",
      })),
      session.stale,
    ),
  );
  void session.boot();
}

function makeClient(base: string): Client {
  if (session && base.startsWith("e2e:")) return new Client(base, session.target(base.slice(4)));
  return new Client(base);
}

// One client for the host shown (M4a); switching hosts closes it and opens
// one to the other daemon, so hidden hosts hold no connection.
let client = makeClient(directory.base());
const connect = () => {
  // In control mode there's nothing to connect to until a daemon is known.
  if (!session || client.e2e) client.connect();
  if (session) {
    const c = client;
    directory.setPath(null);
    c.subscribe(() => c === client && directory.setPath(c.connected ? c.path : null));
  }
};

// The visible height excludes a phone's on-screen keyboard, so the key bar
// sits just above it.
const vv = window.visualViewport;
const fitViewport = () => document.documentElement.style.setProperty("--app-height", `${vv?.height ?? innerHeight}px`);
vv?.addEventListener("resize", fitViewport);
fitViewport();

const cell = await measureCell();
const root = document.getElementById("root")!;
function ControlRoot({ s }: { s: ControlSession }) {
  useControl(s);
  useSubscribe((fn) => directory.subscribe(fn));
  const body =
    s.phase !== "ready" ? <ControlGate s={s} /> : !client.e2e ? <NoMachines s={s} /> : <App key={client.base} client={client} cell={cell} />;
  return (
    <>
      {body}
      <ControlOverlay s={s} />
    </>
  );
}

const draw = () =>
  render(session ? <ControlRoot key={client.base} s={session} /> : <App key={client.base} client={client} cell={cell} />, root);
draw();
connect();

directory.subscribe(() => {
  const base = directory.base();
  if (base === client.base) return;
  client.close();
  client = makeClient(base);
  draw();
  connect();
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
if (!session) void registerWorker(openPane);

// For end-to-end tests.
Object.assign(window, {
  __illogical: {
    get client() {
      return client;
    },
    hosts: directory,
    control: session,
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
