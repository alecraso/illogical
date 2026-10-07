// Where AppKit's native tab bar ends and the page's bar starts, in the app's
// front window (testnet/macos/desktop.sh's tabs claim, #323):
//   osascript -l JavaScript tab-bar.js
// Prints `WINDOW_TOP STRIP_BOTTOM BAR_TOP TABS`: the window's top edge, the
// bottom of the tab bar (0 with no tab bar), the top of the highest button
// in the page (the page's bar), and how many native tabs show. Screen
// points, from the accessibility tree.

function topButton(el, depth) {
  // The page's bar holds the page's highest buttons and tabs.
  let best = Infinity;
  let kids;
  try { kids = el.uiElements(); } catch (e) { return best; }
  for (const k of kids) {
    let role = '';
    try { role = k.role(); } catch (e) { continue; }
    if (role === 'AXButton' || role === 'AXRadioButton' || role === 'AXTab') {
      try { best = Math.min(best, k.position()[1]); } catch (e) { /* gone */ }
    }
    if (depth < 12) best = Math.min(best, topButton(k, depth + 1));
  }
  return best;
}

function findWebArea(el, depth) {
  let kids;
  try { kids = el.uiElements(); } catch (e) { return null; }
  for (const k of kids) {
    try { if (k.role() === 'AXWebArea') return k; } catch (e) { continue; }
    if (depth < 6) {
      const w = findWebArea(k, depth + 1);
      if (w) return w;
    }
  }
  return null;
}

function run() {
  const app = Application('System Events').processes.byName('arugula-desktop');
  const win = app.windows[0];
  const top = win.position()[1];
  let stripBottom = 0;
  let tabs = 0;
  if (win.tabGroups.length > 0) {
    const g = win.tabGroups[0];
    stripBottom = g.position()[1] + g.size()[1];
    tabs = g.radioButtons.length;
  }
  const web = findWebArea(win, 0);
  if (!web) throw new Error('no web area in the front window');
  const bar = topButton(web, 0);
  if (bar === Infinity) throw new Error('no buttons in the page');
  return [top, stripBottom, bar, tabs].map(Math.round).join(" ");
}
