// What the menus offer, in one place (#139): the pane menu, a tab's menu,
// the `+` button's menu and the session menu are built here, and the
// command palette reads the same lists, so the two can't drift.

import { paneIds, type Client } from "../client";
import type { Machine, PaneId, Policy, SessionId, TabView } from "../proto";
import { askText, type MenuItem } from "./menu";
import { openSandboxes } from "./sandboxes";
import { driveItems, shareSession } from "./people";
import { newRemote, openChanges, openEditor, openFountain, openIssue, openPort, openPr, openWorkspace, remoteHosts } from "../blocks";
import { startAgent } from "./agent-dialog";
import { pickConversation } from "./conversations";
import { pickApp } from "./apps";
import { openPicker } from "./picker";
import { agentNotifyItems, notificationItems } from "./notify";
import { openGettingStarted } from "./welcome";
import { openRules } from "./rules";
import { desktopApp, openInNewWindow } from "../desktop";
import { sessionThreadItems, threadItems } from "./threads";

/** Chords, as menus and the palette show them. */
export const PICKER_KEY = "Ctrl+Shift+G";
export const PALETTE_KEY = "Ctrl+Shift+P";

/** A pane's right-click menu. `workspace` is the directory it's in when
 * that is a chant workspace (M34). */
export function paneItems(client: Client, id: PaneId, phone: boolean, workspace: string | null): MenuItem[] {
  const info = client.info(id);
  const entry = client.panes.get(id);
  // #17: a shell on another host, beside this pane.
  const elsewhere = (): MenuItem[] =>
    remoteHosts().map((h) => ({ label: `Split right on ${h}`, run: () => void newRemote(client, h, { split: id }) }));
  const moveToTab: MenuItem = {
    label: "Move to new tab",
    disabled: (client.tabOfPane(id) && paneIds(client.tabOfPane(id)!).length < 2) ?? true,
    run: () => client.intent({ op: "break_pane", pane: id, session: client.session!, index: null }),
  };
  const closePane: MenuItem = { label: "Close pane", danger: true, run: () => client.intent({ op: "close_pane", pane: id }) };

  if (info?.type === "remote") {
    // Its terminal is its host's: here, only where it is.
    const at = client.blocks.get(id)?.state as { host: string; pane: PaneId } | null;
    return [
      ...(at ? [{ header: `%${at.pane} on ${at.host}` } as MenuItem] : []),
      { label: "Split right", run: () => client.intent({ op: "split", pane: id, edge: "right" }) },
      { label: "Split down", run: () => client.intent({ op: "split", pane: id, edge: "bottom" }) },
      ...elsewhere(),
      "separator",
      moveToTab,
      closePane,
    ];
  }

  const cwd = client.cwd(id);
  const tabId = client.tabOfPane(id)?.id;
  const tabMachine = tabId === undefined ? undefined : client.tabMachine(tabId);
  const mine = client.machine(id);
  const own = mine !== undefined && "pane" in mine.owner && mine.owner.pane === id;
  return [
    { label: "Split right", run: () => client.intent({ op: "split", pane: id, edge: "right" }) },
    { label: "Split down", run: () => client.intent({ op: "split", pane: id, edge: "bottom" }) },
    ...(tabMachine ? [{ label: "Split (local)", run: () => client.intent({ op: "split", pane: id, edge: "right", local: true }) } as MenuItem] : []),
    ...(client.has("vms") ? [{ label: "New VM pane on the right", run: () => void client.newVm({ split: id }) } as MenuItem] : []),
    ...elsewhere(),
    {
      label: "Open a web page…",
      run: async () => {
        const url = await askText("Open a web page", "", "https://… or example.com");
        if (url) void client.api("/api/blocks", { type: "browser", config: { url }, split: id }, "couldn't open that page");
      },
    },
    // A port where this pane runs: its machine, or this host.
    { label: mine ? "Open a port on this machine…" : "Open a port…", run: () => void openPort(client, { split: id, host: mine?.id, local: !mine }) },
    { label: "Start an agent…", run: () => startAgent(client, { split: id, from: id }) },
    // M33: Claude Code conversations from terminals and the desktop app.
    ...(!client.state?.roles
      ? [{ label: "Claude Code conversations…", run: () => pickConversation(client, { split: id, cwd: cwd ?? undefined }) } as MenuItem]
      : []),
    // M35: a studio app's box beside this pane.
    ...(!client.state?.roles && client.has("studio") ? [{ label: "Open a studio app…", run: () => pickApp(client, { split: id }) } as MenuItem] : []),
    // M36: a pull request beside it (N: in its repository).
    ...(!client.state?.roles ? [{ label: "Open pull request…", run: () => void openPr(client, { split: id, dir: cwd }) } as MenuItem] : []),
    // M37: an issue beside it (N: in its repository).
    ...(!client.state?.roles ? [{ label: "Open issue…", run: () => void openIssue(client, { split: id, dir: cwd }) } as MenuItem] : []),
    // M43: the Fountain agent catalog beside it.
    ...(!client.state?.roles ? fountainItems(client, { split: id }) : []),
    // M27: VS Code where this pane runs, in its directory. The owner's,
    // like ports.
    ...(entry && !client.state?.roles ? [{ label: "Open in editor", run: () => openEditor(client, id) } as MenuItem] : []),
    // M11: what changed in its repository, where it runs.
    ...(!client.state?.roles ? [{ label: "Changes", run: () => openChanges(client, id) } as MenuItem] : []),
    ...(workspace ? [{ label: "Open as workspace", run: () => openWorkspace(client, workspace, id) } as MenuItem] : []),
    ...(own && !tabMachine
      ? [{ label: "Share machine with tab", run: () => void client.api(`/api/panes/${id}/share-machine`) } as MenuItem]
      : []),
    "separator",
    moveToTab,
    { label: "Go to directory…", shortcut: PICKER_KEY, run: () => openPicker(client, id, phone) },
    { label: "Copy working directory", disabled: !cwd, run: () => cwd && void navigator.clipboard?.writeText(cwd) },
    // A dial-out host's links would be on a daemon nobody can reach.
    ...(client.base.startsWith("/")
      ? []
      : [
          {
            label: "Share read-only link…",
            run: async () => {
              const url = await client.share(id);
              if (url) await askText("Read-only link to this pane, for an hour (copied)", url);
            },
          } as MenuItem,
        ]),
    // M65: an ssh command for a guest with only OpenSSH. The owner's.
    ...(client.base.startsWith("/") || client.state?.roles
      ? []
      : [
          {
            label: "Invite over ssh…",
            run: async () => {
              const cmd = await client.guestInvite(id);
              if (cmd) await askText("Read-only ssh invite to this pane, one login, for an hour (copied)", cmd);
            },
          } as MenuItem,
        ]),
    ...driveItems(client, id),
    "separator",
    // M61: the people's conversation about this pane.
    ...threadItems(client, id),
    "separator",
    ...restartItems(client, id),
    "separator",
    ...(info && (info.attention === "needs_input" || info.attention === "done")
      ? [{ label: "Dismiss", run: () => void client.act({ action: "dismiss", pane: id }) } as MenuItem]
      : []),
    {
      label: "Shell integration (new shells)",
      checked: info?.integration ?? true,
      run: () => client.paneOp(id, { op: "set_integration", on: !(info?.integration ?? true) }),
    },
    {
      label: "Forget history",
      run: () => client.paneOp(id, { op: "purge" }),
    },
    closePane,
  ];
}

/** A tab's right-click menu. */
export function tabItems(client: Client, tab: TabView, rename: () => void): MenuItem[] {
  const machine = client.tabMachine(tab.id);
  return [
    { label: "Rename tab", run: rename },
    { label: "New tab", run: () => client.intent({ op: "new_tab", session: client.session!, from_pane: client.active(tab.id) ?? null }) },
    ...(client.has("vms") ? [{ label: "New VM tab", run: () => void client.newVm({ session: client.session!, tab: true }) } as MenuItem] : []),
    { label: "Go to directory…", shortcut: PICKER_KEY, run: () => openPicker(client, client.active(tab.id)) },
    // The desktop app (M46): this tab in a window of its own.
    ...(desktopApp() ? [{ label: "Open in new window", run: () => openInNewWindow(client.active(tab.id)) } as MenuItem] : []),
    // M11: what changed in the active pane's repository, on its machine.
    ...(!client.state?.roles && client.active(tab.id) !== undefined
      ? [{ label: "Changes", run: () => openChanges(client, client.active(tab.id)!) } as MenuItem]
      : []),
    ...machineItems(client, tab),
    "separator",
    { label: machine ? "Close tab and machine" : "Close tab", danger: true, run: () => client.intent({ op: "close_tab", tab: tab.id }) },
  ];
}

/** The `+` button's menu: a new tab, and blocks in tabs of their own. */
export function newTabItems(client: Client, session: SessionId): MenuItem[] {
  return [
    { label: "New tab", run: () => client.intent({ op: "new_tab", session, from_pane: client.active() ?? null }) },
    ...(client.has("vms") ? [{ label: "New VM tab", run: () => void client.newVm({ session, tab: true }) } as MenuItem] : []),
    { label: "In a directory…", shortcut: PICKER_KEY, disabled: client.active() === undefined, run: () => openPicker(client, client.active()) },
    // M35: a studio app's box, in a tab of its own. The owner's.
    ...(!client.state?.roles && client.has("studio") ? [{ label: "Open a studio app…", run: () => pickApp(client, { session }) } as MenuItem] : []),
    // M36: a pull request, in a tab of its own.
    ...(!client.state?.roles ? [{ label: "Open pull request…", run: () => void openPr(client, { session }) } as MenuItem] : []),
    // M37: an issue, in a tab of its own.
    ...(!client.state?.roles ? [{ label: "Open issue…", run: () => void openIssue(client, { session }) } as MenuItem] : []),
    // M43: the Fountain agent catalog, in a tab of its own.
    ...(!client.state?.roles ? fountainItems(client, { session }) : []),
    // #17: a tab here whose shell runs on another host.
    ...remoteHosts().map((h): MenuItem => ({ label: `New tab on ${h}`, run: () => void newRemote(client, h, { session }) })),
  ];
}

/** The session button's menu, after the list of sessions. */
export function sessionItems(client: Client, session: SessionId, rename: () => void): MenuItem[] {
  return [
    { label: "New session", run: () => client.intent({ op: "new_session", name: null, from_pane: client.active() ?? null }) },
    // VMs and sandboxes need wisp (#180).
    ...(client.has("vms")
      ? [
          { label: "New VM tab", run: () => void client.newVm({ session, tab: true }) } as MenuItem,
          { label: "Sandboxes…", run: () => openSandboxes() } as MenuItem,
        ]
      : []),
    { label: "Rename session", run: rename },
    // M61: the people's conversation about this session.
    ...sessionThreadItems(client, session),
    // Sharing is the daemon's owner's (M13).
    ...(client.state?.roles ? [] : [{ label: "Share session…", run: () => shareSession(session) } as MenuItem]),
    "separator",
    ...notificationItems(client),
    ...agentNotifyItems(client, session),
    // #166: this machine's standing permission rules, the owner's.
    ...(client.state?.roles ? [] : [{ label: "Permission rules…", run: () => openRules(client) } as MenuItem]),
    { label: "Getting started", run: () => openGettingStarted(undefined, client) },
    "separator",
    { label: "Close session", danger: true, run: () => client.intent({ op: "close_session", session }) },
  ];
}

/** The Fountain catalog with a login here; the runner view where the
 * runner's unit is (#180). */
function fountainItems(client: Client, where: { session?: number; split?: PaneId }): MenuItem[] {
  return [
    ...(client.has("fountain") ? [{ label: "Fountain agents…", run: () => void openFountain(client, where) } as MenuItem] : []),
    ...(client.features === null || client.fountainRunner
      ? [{ label: "Fountain runner…", run: () => void openFountain(client, where, "runner") } as MenuItem]
      : []),
  ];
}

/** "idle" when nothing in the tab runs on its machine any more. */
export function machineState(client: Client, m: Machine): string {
  return m.state === "running" && client.panesOn(m.id).length === 0 ? "idle" : m.state;
}

/** The tab's machine: how it is, a new pane on it, reset. */
function machineItems(client: Client, tab: TabView): MenuItem[] {
  const m = client.tabMachine(tab.id);
  if (!m) return [];
  const on = client.panesOn(m.id);
  const anchor = client.active(tab.id) ?? paneIds(tab)[0];
  return [
    "separator",
    { header: `Machine ${m.name ?? m.sprite} · ${machineState(client, m)}` },
    {
      label: "New pane on machine",
      disabled: anchor === undefined,
      run: () => anchor !== undefined && client.intent({ op: "split", pane: anchor, edge: "right", local: false }),
    },
    {
      label: "Open a port on machine…",
      disabled: anchor === undefined,
      run: () => anchor !== undefined && void openPort(client, { split: anchor, host: m.id }),
    },
    {
      label: "Reset machine",
      disabled: on.length === 0,
      run: () => void client.api(`/api/machines/${m.id}/reset`, {}, "couldn't reset the machine"),
    },
  ];
}

/** What the pane does when the daemon starts again, e.g. after a reboot. */
function restartItems(client: Client, id: PaneId): MenuItem[] {
  const info = client.info(id);
  const p = info?.policy ?? { kind: "shell" };
  const cmd = info?.command;
  const set = (policy: Policy) => client.paneOp(id, { op: "set_policy", policy });
  const short = (s: string) => (s.length > 32 ? `${s.slice(0, 31)}…` : s);
  return [
    { header: "After a restart" },
    { label: "Start a shell here", checked: p.kind === "shell", run: () => set({ kind: "shell" }) },
    {
      label: cmd ? `Re-run ${short(cmd)}, asking first` : "Re-run the command, asking first",
      checked: p.kind === "rerun" && p.confirm,
      run: () => set({ kind: "rerun", confirm: true }),
    },
    {
      label: cmd ? `Re-run ${short(cmd)}` : "Re-run the command",
      checked: p.kind === "rerun" && !p.confirm,
      run: () => set({ kind: "rerun", confirm: false }),
    },
    {
      label: p.kind === "hook" ? `Run ${short(p.command)}` : "Run a command…",
      checked: p.kind === "hook",
      run: async () => {
        const command = await askText("Run when restored", p.kind === "hook" ? p.command : "", "claude --continue");
        if (command?.trim()) set({ kind: "hook", command: command.trim() });
      },
    },
    {
      label: info?.resumes ? `Resume ${short(info.resumes)}` : "Resume the agent's conversation",
      checked: p.kind === "resume",
      run: () => set({ kind: "resume" }),
    },
    { label: "Nothing (wait for Enter)", checked: p.kind === "none", run: () => set({ kind: "none" }) },
  ];
}
