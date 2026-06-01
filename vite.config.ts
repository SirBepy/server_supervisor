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
  },
  resolve: {
    alias: {
      "@": resolve(__dirname, "src"),
      // tauri_kit's About page uses plugin-opener, which this app doesn't
      // install — stub it so dev + build resolve. (plugin-updater is now a real
      // dependency, used for in-app auto-update, so it is no longer stubbed.)
      "@tauri-apps/plugin-opener": resolve(__dirname, "src/vendor-stubs/plugin-opener.ts"),
    },
  },
});
