import { defineConfig } from "vite";

// In development the Vite server proxies API calls to the Python backend.
export default defineConfig({
  server: {
    port: 5173,
    proxy: { "/api": "http://127.0.0.1:8000" },
  },
  build: { outDir: "dist", emptyOutDir: true },
});
