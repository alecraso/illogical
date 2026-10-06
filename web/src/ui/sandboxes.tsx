// The home daemon's sandboxes (M4b): a shell on one with no daemon there
// (disposable), or a daemon made resident in it (a host of its own, whose
// layout and scrollback survive the sandbox sleeping or rebooting). Opened
// from the session menu and the phone's sheet. Always asks the home daemon
// (this page's own), whichever host is shown: it holds the provider.

import { useEffect, useState } from "preact/hooks";
import { directory } from "../hosts";
import type { RunRequest, RunResponse } from "../proto";

interface SandboxInfo {
  name: string;
  status: string;
  host: string | null;
}

interface SandboxList {
  provider: { name: string; exec_replay: number; resident: boolean };
  sandboxes: SandboxInfo[];
}

let shown = false;
const listeners = new Set<() => void>();
const changed = () => listeners.forEach((fn) => fn());

export function openSandboxes() {
  shown = true;
  changed();
}

export function SandboxesLayer() {
  const [, setTick] = useState(0);
  useEffect(() => {
    const fn = () => setTick((t) => t + 1);
    listeners.add(fn);
    return () => {
      listeners.delete(fn);
    };
  }, []);
  if (!shown) return null;
  return (
    <Sandboxes
      close={() => {
        shown = false;
        changed();
      }}
    />
  );
}

async function post(path: string, body: unknown): Promise<unknown> {
  const res = await fetch(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
  const v = (await res.json().catch(() => ({}))) as { error?: string };
  if (!res.ok) throw new Error(v.error ?? `HTTP ${res.status}`);
  return v;
}

function size(bytes: number): string {
  return bytes >= 1 << 20 ? `${Math.round(bytes / (1 << 20))} MB` : `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

const ORDER: Record<string, number> = { running: 0, warm: 1, cold: 2 };

function Sandboxes({ close }: { close: () => void }) {
  const [list, setList] = useState<SandboxList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [filter, setFilter] = useState("");

  useEffect(() => {
    fetch("/api/sandboxes")
      .then(async (r) => {
        const v = await r.json();
        if (!r.ok) throw new Error(v.error ?? `HTTP ${r.status}`);
        setList(v as SandboxList);
      })
      .catch((e: Error) => setError(e.message));
  }, []);

  const shell = async (name: string) => {
    setBusy(name);
    try {
      const v = (await post("/api/run", { sandbox: name } satisfies RunRequest)) as RunResponse;
      close();
      window.dispatchEvent(new CustomEvent("illogical:open-pane", { detail: v.pane }));
    } catch (e) {
      setError((e as Error).message);
      setBusy(null);
    }
  };

  const promote = async (name: string) => {
    setBusy(name);
    try {
      const h = (await post(`/api/sandboxes/${encodeURIComponent(name)}/promote`, {})) as { name: string };
      await directory.refresh();
      close();
      directory.select(h.name);
    } catch (e) {
      setError((e as Error).message);
      setBusy(null);
    }
  };

  const open = (host: string) => {
    close();
    directory.select(host);
  };

  const rows = (list?.sandboxes ?? [])
    .filter((s) => s.name.includes(filter.trim()))
    .sort((a, b) => (ORDER[a.status] ?? 3) - (ORDER[b.status] ?? 3) || a.name.localeCompare(b.name));

  return (
    <div class="prompt-backdrop" role="dialog" aria-label="Sandboxes" onPointerDown={(e) => e.target === e.currentTarget && close()}>
      <div class="prompt sandboxes" onKeyDown={(e) => e.key === "Escape" && close()}>
        <h2>Sandboxes{list ? ` on ${list.provider.name}` : ""}</h2>
        {list && (
          <p class="hint">
            <b>Shell</b>: no daemon there, so it's disposable. While nothing follows it, {list.provider.name} keeps only{" "}
            {size(list.provider.exec_replay)} of its output, and it ends when its pane closes.{" "}
            {list.provider.resident && (
              <>
                <b>Make resident</b>: the daemon runs there as a service, a host of its own whose layout and scrollback come
                back after the sandbox sleeps or reboots.
              </>
            )}
          </p>
        )}
        {error && <p class="error">{error}</p>}
        {!list && !error && <p>Asking the provider…</p>}
        {list && list.sandboxes.length > 6 && (
          <input
            class="sandbox-filter"
            placeholder="Filter"
            value={filter}
            onInput={(e) => setFilter(e.currentTarget.value)}
            autoFocus
          />
        )}
        <ul class="sandbox-list">
          {rows.map((s) => (
            <li key={s.name} data-sandbox={s.name}>
              <span class="sandbox-name">{s.name}</span>
              <span class={`sandbox-status ${s.status}`}>{s.status}</span>
              <span class="sandbox-actions">
                {s.host ? (
                  <button class="primary" onClick={() => open(s.host!)}>
                    Open {s.host}
                  </button>
                ) : (
                  <>
                    <button disabled={busy !== null} onClick={() => void shell(s.name)}>
                      Shell
                    </button>
                    {list?.provider.resident && (
                      <button disabled={busy !== null} onClick={() => void promote(s.name)}>
                        {busy === s.name ? "Working…" : "Make resident"}
                      </button>
                    )}
                  </>
                )}
              </span>
            </li>
          ))}
        </ul>
        <div class="prompt-buttons">
          <button type="button" onClick={close}>
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
