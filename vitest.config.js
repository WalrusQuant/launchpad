import { defineConfig } from "vitest/config";

// Separate from vite.config.js so the Tauri dev-server config never bleeds into
// the test run. jsdom is the default environment because some units under test
// (conflictmarkers.js) import CodeMirror, which touches DOM globals at load.
export default defineConfig({
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.js"],
    globals: true,
  },
});
