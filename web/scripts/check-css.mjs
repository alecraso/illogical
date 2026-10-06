// The stylesheet doesn't nest rules, so a rule opened inside another (not
// in an @media or similar) is a missing "}". Browsers take it as nesting
// and drop every rule after it without a word: a merge lost one twice in
// 2026-10 (the S33 hand cards, then the whole chat view).
import { readFileSync } from "node:fs";

let bad = 0;
for (const file of process.argv.slice(2)) {
  const css = readFileSync(file, "utf8").replace(/\/\*[\s\S]*?\*\//g, (c) => c.replace(/[^\n]/g, " "));
  const open = [];
  css.split("\n").forEach((line, i) => {
    for (const ch of line) {
      if (ch === "{") {
        if (open.length && !open.some((o) => o.text.startsWith("@"))) {
          console.error(`${file}:${i + 1}: "${line.trim()}" opens inside "${open.at(-1).text}" (line ${open.at(-1).line}): a missing "}"?`);
          bad++;
        }
        open.push({ text: line.trim(), line: i + 1 });
      } else if (ch === "}") open.pop();
    }
  });
  if (open.length) {
    console.error(`${file}: "${open.at(-1).text}" (line ${open.at(-1).line}) is never closed`);
    bad++;
  }
}
process.exit(bad ? 1 : 0);
