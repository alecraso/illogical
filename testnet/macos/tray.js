// Click the app's tray icon with the mouse, as a person does, and pick
// LABEL from its menu (testnet/macos/desktop.sh's this claim, #323):
//   osascript -l JavaScript tray.js LABEL
// System Events' click on the status item opens no menu (the tray icon
// takes mouseDown in its own view, and AXPress never reaches it), so the
// click is a CGEvent at the icon; the menu item is then clicked through
// accessibility. Throws, listing the menu, when LABEL isn't in it.
ObjC.import('CoreGraphics');

function click(x, y) {
  const pt = $.CGPointMake(x, y);
  $.CGEventPost($.kCGHIDEventTap, $.CGEventCreateMouseEvent(null, $.kCGEventMouseMoved, pt, $.kCGMouseButtonLeft));
  delay(0.2);
  for (const kind of [$.kCGEventLeftMouseDown, $.kCGEventLeftMouseUp]) {
    $.CGEventPost($.kCGHIDEventTap, $.CGEventCreateMouseEvent(null, kind, pt, $.kCGMouseButtonLeft));
    delay(0.1);
  }
}

function run(argv) {
  const [label] = argv;
  const app = Application('System Events').processes.byName('illogical-desktop');
  const icon = app.menuBars[1].menuBarItems[0];
  const [x, y] = icon.position();
  const [w, h] = icon.size();
  click(x + w / 2, y + h / 2);
  delay(1);
  let names;
  try { names = icon.menus[0].menuItems.name(); } catch (e) { throw new Error(`the tray's menu didn't open: ${e}`); }
  if (!names.includes(label)) throw new Error(`no ${label} in the tray's menu: ${names.join(', ')}`);
  icon.menus[0].menuItems.byName(label).click();
  return label;
}
