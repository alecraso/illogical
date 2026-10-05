// illogical web client: tabs and splits of terminals owned by the daemon.

import { setLending } from "./hand";
import { render } from "preact";
import "./style.css";
import { Client } from "./client";
import { Fleet, type HostRef } from "./fleet";
import { directory } from "./hosts";
import { App } from "./ui/app";
import { remotes } from "./blocks";
import { measureCell } from "./ui/cells";
import { enableControlPush, registerWorker, setPushBackend } from "./push";
import { ControlSession, detectControl, restoreInvite } from "./control";
import { ControlGate, ControlOverlay, controlMenuItems, NoMachines, useControl } from "./ui/control";
import { setFleet, setHostMenuExtras } from "./ui/hosts";
import { setControlSession } from "./ui/people";
import { activeHuddle } from "./call";
import { unhex } from "./e2e/cert.ts";
import { useSubscribe } from "./ui/hooks";
import { useEffect, useState } from "preact/hooks";
import { SwarmView } from "./swarm/view";
import { fakeSwarm } from "./swarm/fake";
import { closeSwarm, onSwarmRoute, swarmRoute } from "./swarm/route";
import { setupDesktop } from "./desktop";
import { openThread } from "./ui/threads";

// Served by illogical control (M17), not a daemon: sign in, enroll this
// browser, and reach daemons through end-to-end channels. A read-only link
// (M19, `#link=…`) needs no account: its key is in the fragment.
const info = await detectControl();
// Back from signing in with a presigned invite's link (kept off control).
if (info) restoreInvite();
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
// S33: lend this device's tools to agents, if it was turned on.
setLending(session);
if (session) {
  setHostMenuExtras(() => controlMenuItems(session));
  // A tapped notice from control (#104): what waits shows now.
  addEventListener("illogical:control-refresh", () => void session.refresh());
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
// Inside the desktop app: its titlebar and keys (M46).
setupDesktop(() => client);
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

// Every host at once (M25): a summaries-only connection to each, for the
// swarm. The tab view above still connects for real to the one it shows.
const fleet = new Fleet((h) => {
  if (session) {
    const t = h.id ? session.target(h.id) : undefined;
    return t ? new Client(`e2e:${h.id}`, t, true) : null;
  }
  return new Client(directory.base(h.name), undefined, true);
});
const fleetHosts = (): HostRef[] =>
  directory.names.map((name) => {
    const h = directory.find(name);
    const d = session?.daemons.find((x) => x.id === h?.id);
    return {
      name,
      id: h?.id,
      transport: name === directory.home ? "home" : (h?.transport ?? "tailnet"),
      status: h?.status,
      owner: d?.account ? (d.owner_name ?? d.account) : undefined,
      team: d?.team ?? null,
      ownerId: d?.account,
      teamName: d?.team ? session?.teams.find((t) => t.team === d.team)?.roster.name : undefined,
    };
  });
fleet.onOpen = (host, pane) => {
  directory.select(host);
  // The host's client is made when the directory switches, then connects:
  // show the pane once it knows it.
  const until = Date.now() + 15_000;
  const go = () => {
    if (client.base === directory.base(host) && client.info(pane)) return client.setActive(pane);
    if (Date.now() < until) setTimeout(go, 100);
  };
  go();
};
setFleet(fleet);
if (!linkTarget) {
  // Teams load after the directory: their names come with them (M30).
  session?.subscribe(() => {
    if (session.login) fleet.me = session.login;
    fleet.setHosts(fleetHosts());
  });
  directory.subscribe(() => fleet.setHosts(fleetHosts()));
  fleet.setHosts(fleetHosts());
  fleet.start();
}

/** The swarm (M26) over the tab view, when `/#swarm` says so. */
function Swarm() {
  const [, set] = useState(0);
  useEffect(() => onSwarmRoute(() => set((n) => n + 1)), []);
  const route = swarmRoute();
  if (!route || linkTarget) return null;
  if (session && session.phase !== "ready") return null;
  const f = route.focus;
  // A notification through control names its daemon; else it's this page's.
  const host = f ? (f.daemon ? directory.list?.hosts.find((h) => h.id === f.daemon)?.name : (directory.home ?? directory.names[0])) : undefined;
  return <SwarmView fleet={fleet} back={closeSwarm} focus={f && host ? { host, pane: f.pane } : null} />;
}

const draw = () =>
  render(
    <>
      {session ? <ControlRoot key={client.base} s={session} /> : <App key={client.base} client={client} cell={cell} />}
      <Swarm />
    </>,
    root,
  );
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


// Opened from a notification (`#pane=N`, with `&thread=pane-N` for an
// @mention, M61), or told to by the service worker. Notifications come
// from the home daemon, so show it first.
const openPane = (pane: number, daemon?: string, thread?: string) => {
  // From a notification through control: that daemon's host first.
  const host = daemon ? directory.list?.hosts.find((h) => h.id === daemon)?.name : undefined;
  if (host) directory.select(host);
  else if (directory.home !== null) directory.select(directory.home);
  const go = () => {
    if (!client.info(pane)) return false;
    client.setActive(pane);
    const t = /^(pane|session)-(\d+)$/.exec(thread ?? "");
    if (t) openThread(client, t[1] === "pane" ? { pane: Number(t[2]) } : { session: Number(t[2]) });
    return true;
  };
  if (!go()) {
    const off = client.subscribe(() => go() && off());
  }
};
// A pane opened on the home daemon from elsewhere (a sandbox shell).
window.addEventListener("illogical:open-pane", (e) => openPane((e as CustomEvent<number>).detail));
const fromHash = /^#pane=(?:([0-9a-f]+)\.)?(\d+)(?:&thread=((?:pane|session)-\d+))?$/.exec(location.hash);
if (fromHash) {
  const [, daemon, pane, thread] = fromHash;
  // A notification through control names its daemon: once its host is in
  // the list, open the pane there.
  const go = () => openPane(Number(pane), daemon, thread);
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
    /** #17: the connections remote panes use, by host. */
    remotes,
    control: session,
    fleet,
    /** M63: the huddle this page is in. */
    get huddle() {
      return activeHuddle();
    },
    /** M26: made-up panes in the swarm (frame-rate check, screenshots). */
    swarmFake: (n: number) => fakeSwarm(fleet, n),
    /** M26: the swarm's field, when it's shown. */
    get swarm() {
      return (window as unknown as { __swarm?: unknown }).__swarm ?? null;
    },
    /** M23: a second connection to the same daemon that only takes
     * summaries (what the swarm and the fleet use). */
    summaries: () => {
      const c = new Client(client.base, client.e2e, true);
      c.connect();
      return c;
    },
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
