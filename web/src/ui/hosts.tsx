// Switching hosts (M4a): a button in the desktop bar and a section in the
// phone's sheet. Each host has its own sessions and tabs; switching shows
// that host's layout, connected straight to it.

import { Fragment } from "preact";
import { directory } from "../hosts";
import type { Fleet } from "../fleet";
import { useSubscribe } from "./hooks";
import { openMenu, type MenuItem } from "./menu";
import { openSwarm } from "../swarm/route";
import { runnerLabel, runnerOf, subscribeRunners, watchRunners } from "../runners";

function seen(name: string): string {
  const h = directory.find(name);
  if (!h) return "";
  // A sandbox's state comes from its provider; it may be asleep.
  if (h.transport === "provider") return `${h.status ?? "?"} · ${h.provider?.provider ?? "sandbox"}`;
  if (h.transport === "control" && h.status === "online") return "online";
  if (h.last_seen_ms === null) return "not seen yet";
  const s = Math.max(0, Math.round((Date.now() - h.last_seen_ms) / 1000));
  const ago = s < 60 ? `${s}s` : s < 3600 ? `${Math.round(s / 60)}m` : s < 86400 ? `${Math.round(s / 3600)}h` : `${Math.round(s / 86400)}d`;
  return `seen ${ago} ago`;
}

let fleet: Fleet | null = null;
/** M25: every host's summary connection, for what the menu says of each. */
export function setFleet(f: Fleet) {
  fleet = f;
  // M45b: which machines are Fountain runners, for their lines here.
  watchRunners(f);
}

/** M45b: " · Fountain runner geek online · v0.21.0", for a runner's host. */
function runnerSuffix(name: string): string {
  const r = runnerOf(name);
  return r ? ` · ${runnerLabel(r)}` : "";
}

/** The fleet, if this page has one (#78: every host's conversations). */
export function getFleet(): Fleet | null {
  return fleet;
}

function ago(ms: number): string {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  return s < 60 ? `${s}s` : s < 3600 ? `${Math.round(s / 60)}m` : s < 86400 ? `${Math.round(s / 3600)}h` : `${Math.round(s / 86400)}d`;
}

/** What the fleet knows of a host: live with its panes, or how it's away. */
export function fleetLabel(name: string): string | null {
  const h = fleet?.host(name);
  if (!h) return null;
  const n = h.summary?.panes.length ?? 0;
  const panes = `${n} pane${n === 1 ? "" : "s"}`;
  switch (h.state) {
    case "connected":
      return `${panes} · live`;
    case "connecting":
      return h.summary ? `${panes} · connecting` : "connecting";
    case "stale":
      return `${panes} · stale, seen ${h.lastSeen ? ago(h.lastSeen) : "?"} ago`;
    case "offline":
      return h.summary ? `${panes} · offline` : "offline";
    case "asleep":
      return `asleep${h.summary ? ` · ${panes}` : ""}`;
    case "capped":
      return `${h.summary ? `${panes} · ` : ""}not live (too many machines)`;
  }
}

/** More for the host menu (control mode: the account's items). */
let extras: () => MenuItem[] = () => [];
export function setHostMenuExtras(f: () => MenuItem[]) {
  extras = f;
}

/** Only worth showing once there is somewhere else to go. */
function useHosts(): boolean {
  useSubscribe((fn) => directory.subscribe(fn));
  useSubscribe((fn) => fleet?.subscribe(fn) ?? (() => {}));
  useSubscribe((fn) => subscribeRunners(fn));
  return directory.control || directory.names.length > 1 || directory.shown !== null || !!directory.joined;
}

/** M49: on a joined daemon's own page, the way to the account's other
 * machines is control's page, not a list here. */
function allMachines(): MenuItem[] {
  const url = directory.joined;
  if (directory.control || !url) return [];
  return [{ label: "All your machines…", run: () => void window.open(url, "_blank", "noopener") }];
}

/** M30: hosts grouped by whose they are (yours, each teammate's, each
 * team's); one group while there's only one person. */
function hostGroups(): { label: string; names: string[] }[] {
  const groups = new Map<string, { label: string; names: string[] }>();
  for (const name of directory.names) {
    const h = fleet?.host(name);
    const p = h ? fleet!.personOf(h) : { id: "me", name: "", kind: "me" as const };
    const label = p.kind === "me" ? "Yours" : p.kind === "team" ? `Team ${p.name}` : `${p.name}'s`;
    (groups.get(p.id) ?? groups.set(p.id, { label, names: [] }).get(p.id)!).names.push(name);
  }
  return [...groups.values()];
}

/** Desktop: the shown host, opening a menu of the others. */
export function HostButton() {
  if (!useHosts()) return null;
  const open = (e: MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const item = (name: string): MenuItem => ({
      label: `${name === directory.current ? "✓ " : "    "}${name}${name === directory.home ? " (home)" : ""}  · ${fleetLabel(name) ?? seen(name)}${runnerSuffix(name)}`,
      run: () => directory.select(name),
    });
    const groups = hostGroups();
    const items: MenuItem[] =
      groups.length > 1
        ? groups.flatMap((g) => [{ header: g.label } as MenuItem, ...g.names.map(item)])
        : directory.names.map(item);
    // M51: boxes reached over ssh, which only a terminal can open.
    if (directory.sshOnly.length) {
      items.push("separator", { header: "From a terminal (ssh)" } as MenuItem);
      for (const h of directory.sshOnly) {
        items.push({ label: `    ${h.name}  · illogical --host ${h.name} tui`, disabled: true, run: () => {} });
      }
    }
    items.push("separator", { label: "Swarm: every pane at once", run: openSwarm });
    if (fleet?.notice) items.push("separator", { label: fleet.notice, disabled: true, run: () => {} });
    if (directory.stale) {
      const what = directory.control ? "Control unreachable: saved list" : "Home daemon unreachable: saved list";
      items.push("separator", { label: what, disabled: true, run: () => {} });
    }
    const all = allMachines();
    if (all.length) items.push("separator", ...all);
    items.push(...extras());
    openMenu({ clientX: r.left, clientY: r.bottom + 4, preventDefault: () => e.preventDefault() }, items);
  };
  return (
    <button class="host-button" title="Hosts" data-host={directory.current} onClick={open} onContextMenu={open}>
      {directory.current}
      {directory.path ? (
        <span class={`host-path ${directory.path}`} data-path={directory.path} title={directory.path === "relayed" ? "Through illogical control's relay (end to end encrypted)" : "Straight to the machine"}>
          {directory.path}
        </span>
      ) : null}{" "}
      <span class="caret">▾</span>
    </button>
  );
}

/** While a host hasn't answered yet (it may be down): the way to another. */
export function HostPicker() {
  if (!useHosts()) return null;
  return (
    <div class="empty host-picker">
      <p>Connecting to {directory.current}…</p>
      {directory.names
        .filter((n) => n !== directory.current)
        .map((name) => (
          <button key={name} data-host={name} onClick={() => directory.select(name)}>
            Switch to {name}
          </button>
        ))}
    </div>
  );
}

/** Phone: the shown host's name in the header, when it isn't home. */
export function HostCrumb() {
  useSubscribe((fn) => directory.subscribe(fn));
  if (directory.shown === null && !directory.control) return null;
  return (
    <span class="host-crumb">
      {directory.current}
      {directory.path === "relayed" ? <span class="host-path relayed">relayed</span> : null}
    </span>
  );
}

/** Phone: a section of the sheet listing every host. */
export function HostSection({ close }: { close: () => void }) {
  if (!useHosts()) return null;
  const groups = hostGroups();
  return (
    <section class="sheet-hosts">
      <h2>Machines{directory.stale ? " (saved list)" : ""}</h2>
      {groups.map((g) => (
        <Fragment key={g.label}>
          {groups.length > 1 && <h3>{g.label}</h3>}
          <div class="sheet-chips">
            {g.names.map((name) => (
              <button
                key={name}
                class={name === directory.current ? "chip sheet-host current" : "chip sheet-host"}
                aria-pressed={name === directory.current}
                data-host={name}
                onClick={() => {
                  directory.select(name);
                  close();
                }}
              >
                <span class="host-name">{name}</span>
                <span class="host-seen" data-fleet={fleet?.host(name)?.state}>
                  {name === directory.home ? "home · " : ""}
                  {fleetLabel(name) ?? seen(name)}
                </span>
                {runnerOf(name) && (
                  <span class={`host-seen host-runner${runnerOf(name)!.problem ? " problem" : ""}`} data-fountain-runner={runnerOf(name)!.name} title={runnerOf(name)!.problem ?? ""}>
                    {runnerLabel(runnerOf(name)!)}
                  </span>
                )}
              </button>
            ))}
          </div>
        </Fragment>
      ))}
      <div class="sheet-chips">
        {[...allMachines(), ...extras()].flatMap((item) =>
          typeof item === "object" && "run" in item
            ? [
                <button
                  key={item.label}
                  class="chip"
                  onClick={() => {
                    item.run();
                    close();
                  }}
                >
                  {item.label}
                </button>,
              ]
            : [],
        )}
      </div>
    </section>
  );
}
