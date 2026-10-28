import { defineConfig } from '@playwright/test';
import { join } from 'node:path';
const local = process.env.PRAJNA_E2E_ROOT;
const port = Number(process.env.PRAJNA_E2E_PORT ?? '18789');
if (!Number.isInteger(port) || port < 1024 || port > 65535) throw new Error('Invalid E2E port');
const baseURL = `http://127.0.0.1:${port}`;
export default defineConfig({
  testDir: './e2e', testMatch: '*.spec.ts', workers: 1, fullyParallel: false,
  forbidOnly: true, retries: 0, timeout: 30_000, expect: { timeout: 10_000 },
  reporter: 'list', outputDir: local ? join(local, 'test-results') : '/tmp/prajna-e2e-list-only',
  use: { baseURL, browserName: 'chromium', viewport: { width: 1280, height: 1000 },
    launchOptions: { executablePath: process.env.PRAJNA_CHROME_EXECUTABLE },
    screenshot: 'only-on-failure', trace: 'retain-on-failure' },
  webServer: { command: 'node e2e/server.mjs', url: `${baseURL}/api/v1/health`,
    reuseExistingServer: false, timeout: 1_200_000, stdout: 'pipe', stderr: 'pipe',
    env: { PRAJNA_E2E_PORT: String(port) } },
});
