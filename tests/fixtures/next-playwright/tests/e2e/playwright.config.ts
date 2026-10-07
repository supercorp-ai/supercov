import { existsSync } from 'node:fs';
import { defineConfig } from '@playwright/test';

const port = 3187;
const mockPort = 3188;

export default defineConfig({
  testDir: '.',
  testMatch: '*.spec.ts',
  workers: 1,
  reporter: 'line',
  use: { baseURL: `http://127.0.0.1:${port}` },
  webServer: [
    {
      // The suite builds and serves the application itself.
      command: `npx next build . && npx next start . -p ${port}`,
      url: `http://127.0.0.1:${port}/api/ping`,
      cwd: existsSync('package.json') ? process.cwd() : undefined,
      reuseExistingServer: false,
      timeout: 180_000,
    },
    {
      // Relative to this file: a web server without `cwd` starts beside its config.
      command: `node mock-server.mjs ${mockPort}`,
      url: `http://127.0.0.1:${mockPort}/`,
      reuseExistingServer: false,
    },
  ],
});
