// Switching hosts (M4a): a button in the desktop bar and a section in the
// phone's sheet. Each host has its own sessions and tabs; switching shows
// that host's layout, connected straight to it.

import { directory } from "../hosts";
import { useSubscribe } from "./hooks";
import { openMenu, type MenuItem } from "./menu";

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

/** More for the host menu (control mode: the account's items). */
let extras: () => MenuItem[] = () => [];
export function setHostMenuExtras(f: () => MenuItem[]) {
  extras = f;
}

/** Only worth showing once there is somewhere else to go. */
function useHosts(): boolean {
  useSubscribe((fn) => directory.subscribe(fn));
  return directory.control || directory.names.length > 1 || directory.shown !== null;
}

/** Desktop: the shown host, opening a menu of the others. */
export function HostButton() {
  if (!useHosts()) return null;
  const open = (e: MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const items: MenuItem[] = directory.names.map((name) => ({
      label: `${name === directory.current ? "✓ " : "    "}${name}${name === directory.home ? " (home)" : `  · ${seen(name)}`}`,
      run: () => directory.select(name),
    }));
    if (directory.stale) {
      const what = directory.control ? "Control unreachable: saved list" : "Home daemon unreachable: saved list";
      items.push("separator", { label: what, disabled: true, run: () => {} });
    }
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
  return (
    <section class="sheet-hosts">
      <h2>Hosts{directory.stale ? " (saved list)" : ""}</h2>
      {directory.names.map((name) => (
        <button
          key={name}
          class={name === directory.current ? "sheet-item sheet-host current" : "sheet-item sheet-host"}
          data-host={name}
          onClick={() => {
            directory.select(name);
            close();
          }}
        >
          {name}
          <span class="host-seen">{name === directory.home ? "home" : seen(name)}</span>
        </button>
      ))}
      {extras().flatMap((item) =>
        typeof item === "object" && "run" in item
          ? [
              <button
                key={item.label}
                class="sheet-item"
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
    </section>
  );
}
