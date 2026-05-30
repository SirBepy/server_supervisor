import { defineConfig } from "vite";
import { resolve } from "node:path";

export default defineConfig({
  root: resolve(__dirname, "src"),
  publicDir: false,
  clearScreen: false,
  // Non-default port to avoid colliding with the 1420 Tauri default when
  // multiple Tauri apps run concurrently. 6970 is the Vite dev port; the
  // runtime localhost API defaults to 6969.
  server: { port: 6970, strictPort: true },
  build: {
    outDir: resolve(__dirname, "dist"),
    emptyOutDir: true,
    rollupOptions: {
      // These packages are vendor peer deps (tauri_kit updater/opener features)
      // not used by this app. Tauri provides them at runtime via its plugin system,
      // so they must not be bundled.
      external: ["@tauri-apps/plugin-updater", "@tauri-apps/plugin-opener"],
    },
  },
  resolve: {
    alias: { "@": resolve(__dirname, "src") },
  },
});
