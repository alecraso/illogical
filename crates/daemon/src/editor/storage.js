// Put first in an editor block's pages by the block's site (#69).
//
// An editor block is a frame from another site in the app's page. A
// browser that blocks third-party cookies blocks that frame's storage too:
// reading `localStorage` throws, and VS Code's workbench stops before it
// connects. It already falls back to memory when IndexedDB is refused; this
// does the same for `localStorage` and `sessionStorage`. Nothing a block
// keeps there outlives the page, as the block's own settings live on the
// server.
(() => {
  for (const name of ["localStorage", "sessionStorage"]) {
    try {
      if (window[name]) continue;
    } catch {
      // refused: replaced below
    }
    const items = new Map();
    const storage = {
      get length() {
        return items.size;
      },
      key: (i) => [...items.keys()][i] ?? null,
      getItem: (k) => (items.has(String(k)) ? items.get(String(k)) : null),
      setItem: (k, v) => void items.set(String(k), String(v)),
      removeItem: (k) => void items.delete(String(k)),
      clear: () => items.clear(),
    };
    try {
      Object.defineProperty(window, name, { value: storage, configurable: true });
    } catch {
      // nothing more to do
    }
  }
})();
