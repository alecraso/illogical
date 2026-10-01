import { defineConfig } from "vite";

// `just dev` runs a dev daemon on 7682 that accepts this server's origin.
export default defineConfig({
  server: {
    port: 5173,
    proxy: { "/ws": { target: "ws://127.0.0.1:7682", ws: true, changeOrigin: true } },
  },
  build: { outDir: "dist", emptyOutDir: true, target: "es2022" },
});
