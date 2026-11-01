import { resolve } from "node:path";
import { defineConfig } from "vite";

// In development the Vite server proxies API calls to the Python backend.
export default defineConfig({
  server: {
    port: 5173,
    proxy: { "/api": "http://127.0.0.1:8000" },
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
