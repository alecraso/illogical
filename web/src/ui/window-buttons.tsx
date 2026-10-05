// The window's own buttons, in the bar, for the desktop app on Linux
// (M46): the app's windows have no decorations there, so the client's bar
// is the titlebar. macOS keeps its native buttons over the bar's left end.

import { desktopPlatform, tauriWindow } from "../desktop";

export function WindowButtons() {
  const w = desktopPlatform() === "linux" ? tauriWindow() : null;
  if (!w) return null;
  return (
    <div class="window-buttons">
      <button title="Minimize" aria-label="Minimize" data-window="minimize" onClick={() => void w.minimize()}>
        –
      </button>
      <button title="Maximize" aria-label="Maximize" data-window="maximize" onClick={() => void w.toggleMaximize()}>
        □
      </button>
      <button title="Close window" aria-label="Close window" class="close" data-window="close" onClick={() => void w.close()}>
        ×
      </button>
    </div>
  );
}
