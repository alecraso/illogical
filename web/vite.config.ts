import { defineConfig } from "vite";

// `just dev` runs a dev daemon on 7682 that accepts this server's origin.
// Two pages: the app, and the read-only viewer behind share links.
export default defineConfig({
  server: {
    port: 5173,
    proxy: { "/ws": { target: "ws://127.0.0.1:7682", ws: true, changeOrigin: true } },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
    rollupOptions: { input: { main: "index.html", share: "share.html" } },
    // Follow mode's CodeMirror (M28) is a chunk of its own, about 510 kB
    // (180 kB gzipped), loaded only when someone follows an editor.
    chunkSizeWarningLimit: 560,
  },
});
