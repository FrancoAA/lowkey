import { defineConfig } from "vitest/config";

// The page's tests (npm test): its pure parts, and the whole page on the demo stand-in
// (src/demo.ts) in a simulated browser.
export default defineConfig({
  test: {
    environment: "happy-dom",
    include: ["test/**/*.test.ts"],
  },
});
