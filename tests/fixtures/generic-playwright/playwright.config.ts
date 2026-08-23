import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: ".",
  testMatch: ["tests/e2e/**/*.spec.ts", "specs/**/*.spec.cjs"],
  // The coverage adapter must preserve exact attribution across independent
  // Playwright worker processes sharing one application server.
  workers: 2,
  retries: 0,
  reporter: "list",
  use: {
    baseURL: "http://127.0.0.1:4397",
    launchOptions: {
      executablePath: process.env["SUPERCOV_CHROME"] || undefined,
    },
  },
  webServer: {
    command: "node server.mjs",
    url: "http://127.0.0.1:4397",
    reuseExistingServer: false,
  },
});
