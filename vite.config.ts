import { defineConfig } from "vite";
import { resolve } from "node:path";

export default defineConfig({
  root: resolve(__dirname, "src"),
  publicDir: false,
  clearScreen: false,
  // Unique per-project dev port (avoid the 1420 default other Tauri apps use,
  // or their webviews collide on http://localhost:1420). 6969 is the vite dev
  // port; the runtime localhost API sits beside it on 6970.
  server: { port: 6969, strictPort: true },
  build: {
    outDir: resolve(__dirname, "dist"),
    emptyOutDir: true,
  },
  resolve: {
    alias: { "@": resolve(__dirname, "src") },
  },
});
