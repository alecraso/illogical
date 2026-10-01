// Files on a host (M7), read-only: the shapes of `crates/proto/src/fs.rs`
// and calls to the shown daemon's `/api/fs/…`. `pane` puts a call on the
// host that block runs on (its VM, say); without it, the daemon's own.

import type { Client } from "./client";
import type { PaneId } from "./proto";

export type FsKind = "file" | "directory" | "symlink" | "other";

export interface FsEntry {
  name: string;
  path: string;
  type: FsKind;
  size: number;
  mode: number;
  mtime_ms: number;
  /** For a symlink: what it points at, when known. */
  target?: FsKind;
}

export interface FsList {
  path: string;
  parent: string | null;
  entries: FsEntry[];
  truncated: boolean;
}

export const isDir = (e: FsEntry) => e.type === "directory" || e.target === "directory";

async function get<T>(client: Client, path: string, q: Record<string, string | number | undefined>): Promise<T> {
  const params = new URLSearchParams();
  for (const [k, v] of Object.entries(q)) if (v !== undefined) params.set(k, String(v));
  const res = await fetch(`${client.base}${path}?${params}`);
  const body = (await res.json().catch(() => null)) as (T & { error?: string }) | null;
  if (!res.ok || body === null) throw new Error(body?.error ?? `HTTP ${res.status}`);
  return body;
}

/** A directory's subdirectories (and links to them). */
export const listDirs = (client: Client, pane: PaneId, path: string) =>
  get<FsList>(client, "/api/fs/list", { pane, path, dirs: 1 });

/** Directories used lately on the block's host, newest first. */
export const recentDirs = (client: Client, pane: PaneId) => get<string[]>(client, "/api/fs/recent", { pane });

/**
 * How well `query` matches `text` as a subsequence (case-insensitive):
 * higher is better, null if it doesn't. Runs of letters and matches at the
 * start of a word count more, and a shorter text wins a tie.
 */
export function fuzzy(query: string, text: string): number | null {
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  if (!q) return 0;
  let score = 0;
  let at = 0;
  let run = 0;
  for (const ch of q) {
    const i = t.indexOf(ch, at);
    if (i < 0) return null;
    run = i === at ? run + 1 : 1;
    const wordStart = i === 0 || "/-_. ".includes(t[i - 1]);
    score += 1 + run * 2 + (wordStart ? 6 : 0) - Math.min(i - at, 5);
    at = i + 1;
  }
  if (t.startsWith(q)) score += 10;
  return score - t.length / 100;
}
