// illogical web client: tabs and splits of terminals owned by the daemon.

import { render } from "preact";
import "./style.css";
import { Client } from "./client";
import { directory } from "./hosts";
import { App } from "./ui/app";
import { measureCell } from "./ui/cells";
import { enableControlPush, registerWorker, setPushBackend } from "./push";
import { ControlSession, detectControl } from "./control";
import { ControlGate, ControlOverlay, controlMenuItems, NoMachines, useControl } from "./ui/control";
import { setHostMenuExtras } from "./ui/hosts";
import { setControlSession } from "./ui/people";
import { unhex } from "./e2e/cert.ts";
import { useSubscribe } from "./ui/hooks";

// Served by illogical control (M17), not a daemon: sign in, enroll this
// browser, and reach daemons through end-to-end channels. A read-only link
// (M19, `#link=…`) needs no account: its key is in the fragment.
const info = await detectControl();
const linkMatch = /^#link=([0-9a-f]+)\.([0-9a-f]{64})\.([0-9a-f]{64})\.([0-9a-f]{64})$/.exec(location.hash);
const session = info && !linkMatch ? new ControlSession(info) : null;
setControlSession(session);
let linkTarget: import("./client").E2ETarget | undefined;
if (info && linkMatch) {
  const [, daemon, noise, seed, pub] = linkMatch;
  const pkcs8 = new Uint8Array(48);
  pkcs8.set(unhex("302e020100300506032b656e04220420"));
  pkcs8.set(unhex(seed), 16);
  const privateKey = await crypto.subtle.importKey("pkcs8", pkcs8, { name: "X25519" }, false, ["deriveBits"]);
  const publicKey = await crypto.subtle.importKey("raw", unhex(pub), { name: "X25519" }, true, []);
  const keys = { noise: { privateKey, publicKey }, sign: undefined as unknown as CryptoKeyPair, id: "link", noisePub: pub, signPub: "" };
  linkTarget = { daemon: { id: daemon, noise }, direct: [], relay: `${info.url.replace(/^http/, "ws")}/api/relay/link/${daemon}`, keys };
  // Keep the key out of the address bar (and of anything that reads it).
  history.replaceState(null, "", "/");
}
if (session) {
  setHostMenuExtras(() => controlMenuItems(session));
  setPushBackend({
    enable: () => enableControlPush(session.info.vapid, (sub) => session.subscribePush(sub)),
    disable: async () => {
      const reg = await navigator.serviceWorker.getRegistration();
      const sub = await reg?.pushManager.getSubscription();
      if (sub) {
        await session.unsubscribePush(sub.endpoint).catch(() => {});
        await sub.unsubscribe();
      }
      return "off";
    },
  });
  session.subscribe(() => {
    // A hosted VM this browser started: show it once it's up.
    const started = session.starting && session.daemons.find((d) => d.sandbox === session.starting);
    if (started) {
      session.starting = null;
      queueMicrotask(() => directory.select(started.name));
    }
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
    );
  });
  void session.boot();
}

function makeClient(base: string): Client {
  if (linkTarget) return new Client(`e2e:${linkTarget.daemon.id}`, linkTarget);
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
    let hadSessions = false;
    c.subscribe(() => {
      if (c !== client) return;
      directory.setPath(c.connected ? c.path : null);
      // A hosted VM whose last tab closed is done (M20): delete it.
      const n = c.state?.sessions.length ?? 0;
      if (n > 0) hadSessions = true;
      const vm = session.daemons.find((d) => d.id === c.e2e?.daemon.id)?.sandbox;
      if (vm && hadSessions && n === 0 && c.connected) {
        hadSessions = false;
        void session.deleteSandbox(vm);
      }
    });
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

// A link's page shows its one daemon: no host list.
if (!linkTarget) {
  directory.subscribe(() => {
    const base = directory.base();
    if (base === client.base) return;
    client.close();
    client = makeClient(base);
    draw();
    connect();
  });
  void directory.refresh();
}

// Opened from a notification (`#pane=N`), or told to by the service worker.
// Notifications come from the home daemon, so show it first.
const openPane = (pane: number, daemon?: string) => {
  // From a notification through control: that daemon's host first.
  const host = daemon ? directory.list?.hosts.find((h) => h.id === daemon)?.name : undefined;
  if (host) directory.select(host);
  else if (directory.home !== null) directory.select(directory.home);
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
const fromHash = /^#pane=(?:([0-9a-f]+)\.)?(\d+)$/.exec(location.hash);
if (fromHash) {
  const [, daemon, pane] = fromHash;
  // A notification through control names its daemon: once its host is in
  // the list, open the pane there.
  const go = () => openPane(Number(pane), daemon);
  if (daemon && !directory.find?.(directory.list?.hosts.find((h) => h.id === daemon)?.name ?? "")) {
    const off = directory.subscribe(() => {
      if (directory.list?.hosts.some((h) => h.id === daemon)) {
        off();
        go();
      }
    });
  } else go();
  history.replaceState(null, "", "/");
}
if (!linkTarget) void registerWorker(openPane);

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
