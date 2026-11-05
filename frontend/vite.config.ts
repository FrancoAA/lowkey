import { resolve } from "node:path";
import { defineConfig } from "vite";

// The page of the desktop app (Tauri). `tauri dev` uses the dev server on port 5173.
export default defineConfig({
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    rollupOptions: {
      // Two pages: the app, and the small always-on-top overlay for the global hotkey.
      input: { main: resolve(__dirname, "index.html"), overlay: resolve(__dirname, "overlay.html") },
    },
  },
});
