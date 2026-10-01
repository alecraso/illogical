// Replay each libghostty snapshot into @xterm/headless (what the web client
// runs) and compare text, cursor, screen and input modes with the source.
import { readFileSync, readdirSync } from "node:fs";
import xtermHeadless from "@xterm/headless";
import { Unicode11Addon } from "@xterm/addon-unicode11";

const { Terminal } = xtermHeadless;
const dir = new URL("../fixtures/", import.meta.url);
const names = process.argv.slice(2).length
  ? process.argv.slice(2)
  : readdirSync(dir).filter((f) => f.endsWith(".snap")).map((f) => f.slice(0, -5)).sort();

const norm = (s) => s.split("\n").map((l) => l.trimEnd()).join("\n").trimEnd();
let failed = 0;

for (const name of names) {
  const snap = readFileSync(new URL(`${name}.snap`, dir));
  const exp = JSON.parse(readFileSync(new URL(`${name}.expect.json`, dir), "utf8"));
  const term = new Terminal({ cols: exp.cols, rows: exp.rows, scrollback: 100000, allowProposedApi: true });
  term.loadAddon(new Unicode11Addon());
  term.unicode.activeVersion = "11";
  await new Promise((r) => term.write(snap, r));

  const buf = term.buffer.active;
  const lines = [];
  for (let i = 0; i < buf.length; i++) lines.push(buf.getLine(i).translateToString(true));
  const got = {
    alt: buf.type === "alternate",
    cursor: [buf.cursorX, buf.cursorY],
    plain: norm(lines.join("\n")),
    modes: {
      mouse1006: term.modes.mouseTrackingMode !== "none" ? "tracking" : "none",
      bracketed_paste: term.modes.bracketedPasteMode,
      app_cursor: term.modes.applicationCursorKeysMode,
      app_keypad: term.modes.applicationKeypadMode,
      focus: term.modes.sendFocusMode,
    },
  };
  const want = { ...exp, plain: norm(exp.plain) };
  const diffs = [];
  if (got.alt !== want.alt) diffs.push(`alt: ghostty=${want.alt} xterm=${got.alt}`);
  if (String(got.cursor) !== String(want.cursor)) diffs.push(`cursor: ghostty=${want.cursor} xterm=${got.cursor}`);
  for (const k of ["bracketed_paste", "app_cursor", "app_keypad", "focus"])
    if (got.modes[k] !== want.modes[k]) diffs.push(`mode ${k}: ghostty=${want.modes[k]} xterm=${got.modes[k]}`);
  if (got.plain !== want.plain) {
    const a = want.plain.split("\n"), b = got.plain.split("\n");
    const i = a.findIndex((l, i) => l !== b[i]);
    diffs.push(`text: ${a.length} vs ${b.length} lines; first diff line ${i}\n      ghostty: ${JSON.stringify(a[i])}\n      xterm:   ${JSON.stringify(b[i])}`);
  }
  console.log(`${name.padEnd(12)} ${diffs.length ? "DIFF" : "OK"}  (mouse tracking in xterm: ${got.modes.mouse1006})`);
  for (const d of diffs) console.log("    " + d);
  failed += diffs.length;
  term.dispose();
}
process.exit(failed ? 1 : 0);
