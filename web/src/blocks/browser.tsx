// Browser blocks (M6a): a port on a machine (a dev server, on the block's
// own origin), or a web page in a frame; for sites that refuse to be framed,
// a card that opens them in a new tab.

import { render } from "preact";
import type { Client } from "../client";
import type { OpenRequest, PaneId } from "../proto";
import { askText, tellSetup } from "../ui/menu";
import { registerBlock, type BlockView } from "./view";

export interface BrowserState {
  /** Where the page is now. */
  url: string;
  /** What the frame was last told to load. */
  src: string;
  title: string | null;
  framable: boolean | null;
  error: string | null;
  loading: boolean;
  reloads: number;
  back: string[];
  /** For a port: which, its path, and the machine (null: the daemon's host). */
  port: number | null;
  path: string | null;
  machine: string | null;
}

const ADVANCED = "https://github.com/arugula-salad/illogical/blob/main/docs/advanced.md";

/** Ports and editors in blocks are off on this machine: say what turns them
 * on, rather than fail (#171). */
export function blocksOff(what: "port" | "editor") {
  const [title, thing, anchor] =
    what === "port"
      ? ["Dev servers in blocks are off here", "A dev server beside its terminal", "#browser-blocks-on-ports"]
      : ["VS Code in blocks is off here", "VS Code beside your terminals", "#editor-blocks"];
  void tellSetup(
    title,
    `${thing} is optional: each block gets a web origin of its own, so the daemon needs a listener for them. ` +
      "For a browser on this computer, start illogicald with --block-listen 127.0.0.1:7701 " +
      "(illogicald install -- --block-listen 127.0.0.1:7701). From your phone or other machines it also needs a domain of yours.",
    { label: "How to turn it on (advanced setup)", href: ADVANCED + anchor },
  );
}

/** `:5173/path` for a port, else the URL. */
function shown(s: BrowserState) {
  return s.port !== null ? `:${s.port}${s.path ?? "/"}` : s.url;
}

/**
 * Ask for a port and open it beside `split`: on machine `host`, or (`local`)
 * on the daemon's host even in a VM tab.
 */
export async function openPort(client: Client, where: { split?: PaneId; host?: number; local?: boolean }) {
  if (!client.has("blocks")) return blocksOff("port");
  const v = await askText("Open a port", "", "5173, or 5173/path");
  const m = v?.trim().match(/^:?(\d{1,5})(\/.*)?$/);
  if (!v) return;
  if (!m) {
    client.toast(`not a port: ${v}`);
    return;
  }
  void client.api(
    "/api/blocks",
    {
      type: "browser",
      config: { port: Number(m[1]), path: m[2] ?? "/" },
      split: where.split ?? null,
      host: where.host ?? null,
      local: !!where.local,
    } satisfies OpenRequest,
    "couldn't open that port",
  );
}

function BrowserBlock({ client, id, s }: { client: Client; id: PaneId; s: BrowserState | null }) {
  if (!s) return <div class="browser-card">…</div>;
  const call = (method: string, args: unknown = {}) => void client.api(`/api/blocks/${id}/call/${method}`, args);
  const port = s.port !== null;
  const where = (() => {
    if (port) return `:${s.port}`;
    try {
      return new URL(s.url).host;
    } catch {
      return s.url;
    }
  })();
  return (
    <div class="browser">
      <div class="browser-bar">
        <button title="Back" disabled={s.back.length === 0} onClick={() => call("back")}>
          ←
        </button>
        <button title="Reload" onClick={() => call("reload")}>
          ↻
        </button>
        {port && s.machine && (
          <span class="host-tag" title={`port ${s.port} on ${s.machine}`}>
            VM
          </span>
        )}
        <form
          class="browser-url"
          onSubmit={(e) => {
            e.preventDefault();
            const v = (e.currentTarget.elements.namedItem("url") as HTMLInputElement).value;
            call("navigate", { url: v });
          }}
        >
          <input name="url" value={shown(s)} spellcheck={false} autocomplete="off" />
        </form>
        <a class="browser-open" href={s.url} target="_blank" rel="noopener noreferrer" title="Open in a new tab">
          ↗
        </a>
      </div>
      {s.error ? (
        <div class="browser-card error">
          <p>{port ? `Nothing is answering on ${where}` : `Couldn't load ${where}`}</p>
          <p class="dim">{s.error}</p>
          {port && <p class="dim">It shows again when the server is back.</p>}
          <button onClick={() => call("reload")}>Try again</button>
        </div>
      ) : s.framable === false ? (
        <div class="browser-card">
          <p>{s.title ?? where} doesn't allow being shown inside another page.</p>
          <a class="button" href={s.url} target="_blank" rel="noopener noreferrer">
            Open in new tab
          </a>
        </div>
      ) : s.framable === null ? (
        <div class="browser-card dim">Loading {where}…</div>
      ) : (
        // Its own origin, never the app's: a port on its block's own name,
        // or the site's. `allow-same-origin` means that origin.
        <iframe
          key={`${s.src}#${s.reloads}`}
          class="browser-frame"
          src={s.src}
          title={s.title ?? s.url}
          sandbox={port ? "allow-scripts allow-forms allow-same-origin" : "allow-scripts allow-forms allow-same-origin allow-popups"}
          referrerpolicy="no-referrer"
        />
      )}
    </div>
  );
}

registerBlock("browser", (client, id): BlockView => {
  const host = document.createElement("div");
  host.className = "block block-browser";
  let state: BrowserState | null = null;
  const draw = () => render(<BrowserBlock client={client} id={id} s={state} />, host);
  draw();
  return {
    host,
    setVisible: (v) => (host.style.display = v ? "" : "none"),
    update: (s) => {
      state = s as BrowserState;
      draw();
    },
    title: () => state?.title ?? (state ? shown(state) : "browser"),
    text: () => (state ? `${state.title ?? ""}\n${shown(state)}`.trim() : ""),
    focus: () => host.querySelector<HTMLElement>("iframe, input")?.focus(),
    dispose: () => {
      render(null, host);
      host.remove();
    },
  };
});
