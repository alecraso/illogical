import { TerminalView } from "../terminal-view";

export interface Cell {
  width: number;
  height: number;
}

/** Measure one terminal cell by rendering a small terminal off screen.
 * The daemon lays tabs out in cells, so this turns its layout into pixels
 * and a window's pixels into a size in cells. */
export async function measureCell(): Promise<Cell> {
  await document.fonts?.ready;
  const probe = new TerminalView();
  const box = document.createElement("div");
  box.style.cssText = "position:fixed;left:-10000px;top:0;visibility:hidden";
  box.appendChild(probe.host);
  document.body.appendChild(box);
  probe.resize(10, 4);
  // With the renderer panes draw with: WebGL snaps a cell to whole device
  // pixels, so the DOM renderer's width is too wide and a pane's last
  // columns come up short, leaving a gap at its right (#164).
  probe.setVisible(true);
  await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  const cell = probe.cellSize() ?? { width: 8.4, height: 17 };
  probe.setVisible(false);
  probe.dispose();
  box.remove();
  return cell;
}
