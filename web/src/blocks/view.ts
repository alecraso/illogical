// A block that isn't a terminal, as the page shows it. Like a terminal's
// view, its element is moved between layout slots rather than recreated, so
// moving a block around never reloads it.

import type { Client } from "../client";
import type { BlockType, PaneId } from "../proto";

export interface BlockView {
  /** The element placed in the block's slot. */
  readonly host: HTMLElement;
  setVisible(visible: boolean): void;
  /** The daemon's latest state for the block (whole, not a diff). */
  update(state: unknown): void;
  /** What to call it in tabs and lists. */
  title(): string;
  /** Plain text, as `capture --text` would give (for tests and copying). */
  text(): string;
  focus(): void;
  dispose(): void;
  /** Its place in the layout is `cols`×`rows` cells (#17: a remote pane
   * passes it on to its host). */
  layout?(cols: number, rows: number): void;
  /** It's being closed here: close what it stands for (#17: the pane on
   * its host). */
  closing?(): void;
}

export type BlockRenderer = (client: Client, id: PaneId) => BlockView;

const renderers = new Map<BlockType, BlockRenderer>();

/** Each block type's module registers how to draw it. */
export function registerBlock(type: BlockType, make: BlockRenderer) {
  renderers.set(type, make);
}

export function makeBlockView(type: BlockType, client: Client, id: PaneId): BlockView {
  const make = renderers.get(type);
  return make ? make(client, id) : unknownView(type);
}

function unknownView(type: string): BlockView {
  const host = document.createElement("div");
  host.className = "block block-unknown";
  host.textContent = `This client can't show ${type} blocks yet.`;
  return {
    host,
    setVisible: (v) => (host.style.display = v ? "" : "none"),
    update: () => {},
    title: () => type,
    text: () => host.textContent ?? "",
    focus: () => {},
    dispose: () => host.remove(),
  };
}
