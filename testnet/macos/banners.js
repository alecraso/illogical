// Close every notification banner on screen (testnet/macos/desktop.sh's
// drag claim): the app's first start posts "App Background Activity",
// which stays for minutes at the screen's top right, over the window's
// bar, and takes the mouse there. Prints how many it closed.
//   osascript -l JavaScript banners.js
// Restarting NotificationCenter doesn't do: it shows the banner again.
function run() {
  const nc = Application('System Events').processes.byName('NotificationCenter');
  let closed = 0;
  const walk = (el, depth) => {
    let kids;
    try { kids = el.uiElements(); } catch (e) { return; }
    for (const k of kids) {
      let acts = [];
      try { acts = k.actions(); } catch (e) { /* none */ }
      const close = acts.find((a) => /Name:Close\b|^AXClose$/.test(a.name()));
      if (close) {
        try { close.perform(); closed++; delay(0.3); continue; } catch (e) { /* gone */ }
      }
      if (depth < 8) walk(k, depth + 1);
    }
  };
  for (const w of nc.windows()) {
    try { if (w.name() === 'Notification Center') walk(w, 0); } catch (e) { /* gone */ }
  }
  return closed;
}
