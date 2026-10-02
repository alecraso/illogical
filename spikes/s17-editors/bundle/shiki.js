// Shiki (TextMate grammars, JS regex engine), fine-grained: three languages and the terminal's
// own theme (Catppuccin Mocha), rendered to static HTML.
import { createHighlighterCore } from "shiki/core";
import { createJavaScriptRegexEngine } from "shiki/engine/javascript";

const hl = await createHighlighterCore({
  themes: [import("shiki/themes/catppuccin-mocha.mjs")],
  langs: [import("shiki/langs/rust.mjs"), import("shiki/langs/typescript.mjs"), import("shiki/langs/python.mjs")],
  engine: createJavaScriptRegexEngine(),
});
document.getElementById("code").innerHTML = hl.codeToHtml(window.SAMPLE, { lang: "rust", theme: "catppuccin-mocha" });
window.view = hl;
