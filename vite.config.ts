import { defineConfig } from "vite";
import { resolve } from "node:path";

export default defineConfig({
  root: resolve(__dirname, "src"),
  publicDir: false,
  clearScreen: false,
  // Unique per-project dev port (avoid the 1420 default other Tauri apps use,
  // or their webviews collide on http://localhost:1420). Sits beside the API port 7717.
  server: { port: 7716, strictPort: true },
  build: {
    outDir: resolve(__dirname, "dist"),
    emptyOutDir: true,
  },
  resolve: {
    alias: { "@": resolve(__dirname, "src") },
  },
});
