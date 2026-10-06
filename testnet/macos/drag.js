// Drag the app's front window by its top strip with the mouse, as a person
// does (testnet/macos/desktop.sh's drag claim, #316):
//   osascript -l JavaScript drag.js X Y         drag from (X, Y) 150 right, 80 down
//   osascript -l JavaScript drag.js X Y double  double-click there instead
// X and Y are points from the window's top left; a negative X counts from
// its right edge. X `gap` is the middle of the widest stretch of the page's
// bar (its top 40 points) with no button or tab in it, the bar's empty
// fill, wherever the tabs have pushed it. Prints `X0 Y0 W0 H0 X1 Y1 W1 H1`: the window's frame
// before and after. CGEvents, since System Events can't drag.
ObjC.import('CoreGraphics');

function post(kind, x, y, clicks) {
  const e = $.CGEventCreateMouseEvent(null, kind, $.CGPointMake(x, y), $.kCGMouseButtonLeft);
  if (clicks) $.CGEventSetIntegerValueField(e, $.kCGMouseEventClickState, clicks);
  $.CGEventPost($.kCGHIDEventTap, e);
}

// The x of the middle of the widest buttonless stretch across the page's
// bar, from the accessibility tree (as tab-bar.js reads it).
function barGap(win, frame) {
  const spans = [];
  const walk = (el, depth) => {
    let kids;
    try { kids = el.uiElements(); } catch (e) { return; }
    for (const k of kids) {
      let role = '';
      try { role = k.role(); } catch (e) { continue; }
      if (['AXButton', 'AXRadioButton', 'AXTab', 'AXPopUpButton', 'AXMenuButton'].includes(role)) {
        try {
          const [x, y] = k.position();
          const [w, h] = k.size();
          if (y + h / 2 < frame[1] + 40 && w > 0) spans.push([x, x + w]);
        } catch (e) { /* gone */ }
      }
      if (depth < 14) walk(k, depth + 1);
    }
  };
  walk(win, 0);
  // The traffic lights' end, then each button, then the window's right edge.
  spans.push([frame[0], frame[0] + 78], [frame[0] + frame[2], frame[0] + frame[2]]);
  spans.sort((a, b) => a[0] - b[0]);
  let best = null;
  let end = frame[0];
  for (const [a, b] of spans) {
    if (a - end > 0 && (!best || a - end > best[1] - best[0])) best = [end, a];
    end = Math.max(end, b);
  }
  if (!best || best[1] - best[0] < 20) throw new Error(`no empty stretch in the bar: ${JSON.stringify(spans)}`);
  return (best[0] + best[1]) / 2;
}

function frame(win) {
  return [...win.position(), ...win.size()];
}

function run(argv) {
  const app = Application('System Events').processes.byName('illogical-desktop');
  app.frontmost = true;
  delay(0.5);
  const win = app.windows[0];
  const before = frame(win);
  const dx = Number(argv[0]);
  const x = argv[0] === 'gap' ? barGap(win, before) : dx < 0 ? before[0] + before[2] + dx : before[0] + dx;
  const y = before[1] + Number(argv[1]);
  post($.kCGEventMouseMoved, x, y);
  delay(0.2);
  if (argv[2] === 'double') {
    for (const n of [1, 2]) {
      post($.kCGEventLeftMouseDown, x, y, n);
      post($.kCGEventLeftMouseUp, x, y, n);
      delay(0.05);
    }
  } else {
    post($.kCGEventLeftMouseDown, x, y, 1);
    delay(0.3);
    for (let i = 1; i <= 10; i++) {
      post($.kCGEventLeftMouseDragged, x + 15 * i, y + 8 * i, 1);
      delay(0.03);
    }
    delay(0.2);
    post($.kCGEventLeftMouseUp, x + 150, y + 80, 1);
  }
  delay(1.5);
  return [...before, ...frame(win)].map(Math.round).join(' ');
}
