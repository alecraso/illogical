// Injected into every S25 page, the daemon's included. window.__S25_FLAGS is
// prepended by main.rs.
(() => {
  if (window.top !== window) return;
  const flags = window.__S25_FLAGS || {};
  const invoke = (cmd, args) => window.__TAURI__?.core.invoke(cmd, args);
  window.__S25 = { flags, invoke };

  const chord = (e) =>
    [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Meta", e.code].filter(Boolean).join("-");

  const ready = (f) => (document.readyState === "loading" ? addEventListener("DOMContentLoaded", f) : f());

  if (flags.hud) {
    ready(() => {
      const hud = document.createElement("div");
      hud.style.cssText =
        "position:fixed;right:6px;bottom:6px;z-index:2147483647;font:11px ui-monospace,monospace;" +
        "background:#000c;color:#0f0;padding:4px 6px;border-radius:4px;pointer-events:none;white-space:pre";
      document.body.appendChild(hud);
      let frames = 0, worst = 0, last = performance.now(), mark = last, keys = 0, lastKey = "", lagged = "";
      const tick = (t) => {
        frames++;
        worst = Math.max(worst, t - last);
        last = t;
        if (t - mark >= 1000) {
          hud.textContent = `${Math.round((frames * 1000) / (t - mark))} fps  worst ${worst.toFixed(0)} ms\n${keys} keys  ${lastKey}${lagged}`;
          frames = 0;
          worst = 0;
          mark = t;
        }
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
      // Capture phase, first: did the key reach the page at all? Then the
      // next frame: how long until the page could paint after it.
      addEventListener(
        "keydown",
        (e) => {
          keys++;
          lastKey = chord(e);
          const t0 = performance.now();
          requestAnimationFrame(() => requestAnimationFrame((t) => (lagged = `  ${(t - t0).toFixed(0)} ms to 2nd frame`)));
        },
        true,
      );
    });
  }

  if (flags.titlebar === "overlay") {
    const mac = flags.os === "macos";
    const style = document.createElement("style");
    style.textContent = mac
      ? ".bar{padding-left:78px!important}"
      : ".s25-wc{display:flex;margin-left:auto}.s25-wc button{width:40px;border:0;background:none;color:inherit;font:14px sans-serif}" +
        ".s25-wc button:hover{background:#8884}";
    const win = () => window.__TAURI__?.window.getCurrentWindow();
    const dress = (bar) => {
      bar.setAttribute("data-tauri-drag-region", "");
      if (mac || bar.querySelector(".s25-wc")) return;
      const wc = document.createElement("div");
      wc.className = "s25-wc";
      for (const [label, act] of [["–", "minimize"], ["□", "toggleMaximize"], ["×", "close"]]) {
        const b = document.createElement("button");
        b.textContent = label;
        b.onclick = () => win()?.[act]();
        wc.appendChild(b);
      }
      bar.appendChild(wc);
    };
    ready(() => {
      document.head.appendChild(style);
      const look = () => document.querySelectorAll(".bar").forEach(dress);
      look();
      new MutationObserver(look).observe(document.body, { childList: true, subtree: true });
    });
  }
})();
