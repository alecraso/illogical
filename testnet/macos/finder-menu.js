// Right-click ITEM in Finder's front window (a list view) with the mouse,
// as a person does, and pick LABEL from the menu that opens
// (testnet/macos/desktop.sh's finder claim):
//   osascript -l JavaScript finder-menu.js ITEM LABEL
// Prints where it found LABEL; throws, listing the menu, when it isn't
// there. System Events' AXShowMenu opens no menu on a Finder row, so the
// click is a CGEvent at the row.
ObjC.import('CoreGraphics');

function rightClick(x, y) {
  const pt = $.CGPointMake(x, y);
  for (const kind of [$.kCGEventRightMouseDown, $.kCGEventRightMouseUp]) {
    $.CGEventPost($.kCGHIDEventTap, $.CGEventCreateMouseEvent(null, kind, pt, $.kCGMouseButtonRight));
    delay(0.1);
  }
}

function run(argv) {
  const [item, label] = argv;
  const finder = Application('System Events').processes.byName('Finder');
  finder.frontmost = true;
  delay(0.5);
  const list = finder.windows[0].splitterGroups[0].splitterGroups[0].scrollAreas[0].outlines[0];
  const row = list.rows().find((r) => {
    try { return r.uiElements[0].textFields[0].value() === item; } catch (e) { return false; }
  });
  if (!row) throw new Error(`no row ${item} in Finder's window`);
  const [x, y] = row.position();
  const [, h] = row.size();
  rightClick(x + 120, y + h / 2);
  delay(1);
  if (list.menus.length === 0) throw new Error('the right-click opened no menu');
  const menu = list.menus[0];
  const names = menu.menuItems.name();
  if (!names.includes(label)) {
    $.CGEventPost($.kCGHIDEventTap, $.CGEventCreateKeyboardEvent(null, 53, true));
    throw new Error(`no ${label} in the menu: ${names.filter((n) => n).join(' | ')}`);
  }
  menu.menuItems.byName(label).click();
  return `right-click on ${item} > ${label}`;
}
