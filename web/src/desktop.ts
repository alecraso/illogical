// Inside the desktop app (crates/desktop, M46). The app's init script sets
// `window.__illogicalApp` ({ name, platform }) on every page it opens.
//
// - The client's bar is the window's titlebar: `html[data-desktop]` lets the
//   stylesheet leave room for macOS's window buttons, and on Linux (no
//   decorations) the bar carries its own (ui/window-buttons.tsx).
// - On macOS the app's menus take only the Mac's own keys (Cmd-Q, H,
//   Option-H) and Edit's, so Cmd-W, T, N and U reach the page: Cmd-W closes
//   the pane (not the window), Cmd-T opens a tab, Cmd-N a window, and Cmd-U
//   attaches files (as Claude's app does): the agent composer's 📎 when one
//   has focus or is the active pane, else the active terminal's
//   *Attach file…* (M70).

import type { Client } from "./client";

type AppInfo = { name?: string; platform?: "macos" | "linux" };

export function desktopApp(): AppInfo | null {
  return (globalThis as { __illogicalApp?: AppInfo }).__illogicalApp ?? null;
}

export function desktopPlatform(): "macos" | "linux" | null {
  return desktopApp()?.platform ?? null;
}

/** The Tauri window this page is in, when the app lets the page move it. */
export function tauriWindow(): {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
} | null {
  const t = (globalThis as { __TAURI__?: { window?: { getCurrentWindow?: () => unknown } } }).__TAURI__;
  return (t?.window?.getCurrentWindow?.() as ReturnType<typeof tauriWindow>) ?? null;
}

/** A new window of the app, showing `pane` (or the same layout). */
export function openInNewWindow(pane?: number) {
  window.open(pane === undefined ? "/" : `/#pane=${pane}`, "_blank");
}

/** Mark the page and take the app's keys. `client` is read on each press:
 * the page swaps it when the host changes. */
export function setupDesktop(client: () => Client) {
  const platform = desktopPlatform();
  if (!platform) return;
  document.documentElement.dataset.desktop = platform;
  if (platform !== "macos") return;
  addEventListener(
    "keydown",
    (e) => {
      if (!e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;
      const c = client();
      const act = (
        {
          KeyW: () => {
            const pane = c.active();
            if (pane !== undefined) c.intent({ op: "close_pane", pane });
          },
          KeyT: () => {
            if (c.session !== null) c.intent({ op: "new_tab", session: c.session, from_pane: c.active() ?? null });
          },
          KeyN: () => openInNewWindow(),
          KeyU: () => {
            const pane = c.active();
            const el = pane === undefined ? null : document.querySelector(`[data-pane="${pane}"]`);
            const composer = document.activeElement?.closest(".agent-composer") ?? el?.querySelector(".agent-composer");
            const attach = composer?.querySelector<HTMLButtonElement>("button.attach");
            if (attach) attach.click();
            else if (pane !== undefined && c.panes.has(pane) && c.mayType(pane)) void c.attachFiles(pane);
          },
        } as Record<string, () => void>
      )[e.code];
      if (!act) return;
      e.preventDefault();
      e.stopPropagation();
      act();
    },
    true,
  );
}
