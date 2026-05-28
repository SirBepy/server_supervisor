import { defineConfig } from "vite";
import { resolve } from "node:path";

export default defineConfig({
  root: resolve(__dirname, "src"),
  publicDir: false,
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    outDir: resolve(__dirname, "dist"),
    emptyOutDir: true,
  },
  resolve: {
    alias: { "@": resolve(__dirname, "src") },
  },
});
