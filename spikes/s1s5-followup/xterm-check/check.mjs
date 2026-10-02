// Replay each wire snapshot into @xterm/headless 6 (what the web client
// runs) and compare, under each probe:
//   - xterm(raw fixture bytes) vs xterm(snapshot): text with scrollback,
//     cursor, active buffer, input modes, and every visible cell's char and
//     attributes. This is the fair test: what the client would show had it
//     seen the whole stream.
//   - Ghostty A (the source, as S1 did) vs xterm(snapshot): text, cursor,
//     active buffer.
// Usage: node check.mjs [suffix] [names...]   (suffix: "" or ".patched")
import { readFileSync, readdirSync } from "node:fs";
import xtermHeadless from "@xterm/headless";
import { Unicode11Addon } from "@xterm/addon-unicode11";

const { Terminal } = xtermHeadless;
const out = new URL("../work/out/", import.meta.url);
const fixtures = new URL("../fixtures/", import.meta.url);
const [suffix = "", ...only] = process.argv.slice(2);
const names = only.length
  ? only
  : readdirSync(out).filter((f) => f.endsWith(".expect.json")).map((f) => f.slice(0, -12)).sort();

const norm = (s) => s.split("\n").map((l) => l.trimEnd()).join("\n").trimEnd();

function term(cols, rows) {
  const t = new Terminal({ cols, rows, scrollback: 100000, allowProposedApi: true });
  t.loadAddon(new Unicode11Addon());
  t.unicode.activeVersion = "11";
  return t;
}
const write = (t, d) => new Promise((r) => t.write(d, r));

// SGR 31 and 38;5;1 are the same color; xterm records them differently.
const mode = (c, w) => (c[`is${w}Palette`]() ? "P" : c[`is${w}RGB`]() ? "RGB" : "D");

function view(t) {
  const buf = t.buffer.active;
  const lines = [];
  for (let i = 0; i < buf.length; i++) lines.push(buf.getLine(i).translateToString(true));
  const cells = [];
  const c = buf.getNullCell();
  for (let y = 0; y < t.rows; y++) {
    const line = buf.getLine(buf.baseY + y);
    const row = [];
    for (let x = 0; x < t.cols; x++) {
      line.getCell(x, c);
      row.push(
        [c.getChars() || " ", c.getWidth(), mode(c, "Fg"), c.getFgColor(), mode(c, "Bg"), c.getBgColor(),
         c.isBold(), c.isItalic(), c.isDim(), c.isUnderline(), c.isInverse(), c.isInvisible(), c.isStrikethrough()].join(","),
      );
    }
    cells.push(row);
  }
  return {
    alt: buf.type === "alternate",
    cursor: `${buf.cursorX},${buf.cursorY}`,
    scrollback: buf.baseY,
    plain: norm(lines.join("\n")),
    modes: JSON.stringify(t.modes),
    cells,
  };
}

function diff(a, b, labelA, labelB) {
  const d = [];
  for (const k of ["alt", "cursor", "scrollback", "modes"])
    if (a[k] !== b[k]) d.push(`${k}: ${labelA}=${a[k]} ${labelB}=${b[k]}`);
  if (a.plain !== b.plain) {
    const x = a.plain.split("\n"), y = b.plain.split("\n");
    const i = x.findIndex((l, i) => l !== y[i]);
    d.push(`text: ${x.length} vs ${y.length} lines, first diff ${i}: ${JSON.stringify((x[i] ?? "").slice(0, 80))} vs ${JSON.stringify((y[i] ?? "").slice(0, 80))}`);
  }
  if (a.cells && b.cells) {
    let n = 0, first = null;
    a.cells.forEach((row, y) => row.forEach((c, x) => {
      if (c !== b.cells[y][x]) { n++; first ??= `(${x},${y}) ${labelA}=[${c}] ${labelB}=[${b.cells[y][x]}]`; }
    }));
    if (n) d.push(`cells: ${n} differ, first ${first}`);
  }
  return d;
}

let failed = 0;
for (const name of names) {
  const exp = JSON.parse(readFileSync(new URL(`${name}.expect.json`, out), "utf8"));
  const raw = readFileSync(new URL(`${name}.bin`, fixtures));
  const snap = readFileSync(new URL(`${name}${suffix}.snap`, out));
  const results = [];
  for (const p of exp.probes) {
    const a = term(exp.cols, exp.rows);
    let pos = 0;
    for (const r of exp.resizes) {
      await write(a, raw.subarray(pos, r.offset));
      a.resize(r.cols, r.rows);
      pos = r.offset;
    }
    await write(a, raw.subarray(pos));
    const g = p.ghostty_a;
    const b = term(g.cols, g.rows);
    await write(b, snap);
    await write(a, p.bytes);
    await write(b, p.bytes);
    const va = view(a), vb = view(b);
    const vsRaw = diff(va, vb, "xterm(raw)", "xterm(snap)");
    const ga = { alt: g.alt, cursor: g.cursor.join(","), plain: norm(g.plain) };
    const vsGhostty = diff(ga, { alt: vb.alt, cursor: vb.cursor, plain: vb.plain }, "ghostty(A)", "xterm(snap)");
    results.push({ probe: p.probe, vsRaw, vsGhostty });
    a.dispose();
    b.dispose();
  }
  const bad = results.filter((r) => r.vsRaw.length || r.vsGhostty.length);
  const rawOk = results.every((r) => !r.vsRaw.length), ghOk = results.every((r) => !r.vsGhostty.length);
  console.log(`${name.padEnd(14)} vs xterm(raw): ${rawOk ? "OK" : "DIFF"}   vs ghostty(A): ${ghOk ? "OK" : "DIFF"}`);
  for (const r of bad) {
    console.log(`    [${r.probe}]`);
    for (const d of r.vsRaw) console.log(`      raw:     ${d}`);
    for (const d of r.vsGhostty) console.log(`      ghostty: ${d}`);
  }
  failed += bad.length;
}
process.exit(failed ? 1 : 0);
