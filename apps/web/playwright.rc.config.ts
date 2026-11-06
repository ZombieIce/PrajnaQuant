// Reuse production browser assertions against an already-running, installed RC.
import { spawnSync } from 'node:child_process';
import { readFileSync, realpathSync } from 'node:fs';
import { join } from 'node:path';
import { defineConfig } from '@playwright/test';

const local = process.env.PRAJNA_E2E_ROOT;
const installed = process.env.PRAJNA_RC_BUNDLE;
if (!local || !installed) throw new Error('PRAJNA_E2E_ROOT and PRAJNA_RC_BUNDLE are required');
const lake = realpathSync(join(local, 'fixture', 'lake'));
const bundle = realpathSync(installed);
const status = spawnSync(join(bundle, 'prajna'), ['status', '--data-root', lake], { encoding: 'utf8' });
if (status.status !== 0 || status.stdout.trim() !== 'ready') {
  throw new Error(`Installed RC is not ready: ${status.stderr}`);
}
const state: { port: number } = JSON.parse(readFileSync(join(lake, '.prajna-rc/instance.json'), 'utf8'));
if (!Number.isInteger(state.port) || state.port < 1024 || state.port > 65535) throw new Error('Invalid RC port');

export default defineConfig({
  testDir: './e2e', testMatch: '*.spec.ts', workers: 1, fullyParallel: false,
  forbidOnly: true, retries: 0, timeout: 30_000, expect: { timeout: 10_000 },
  reporter: 'list', outputDir: join(local, 'rc-test-results'),
  use: {
    baseURL: `http://127.0.0.1:${state.port}`, browserName: 'chromium',
    viewport: { width: 1280, height: 1000 },
    launchOptions: { executablePath: process.env.PRAJNA_CHROME_EXECUTABLE },
    screenshot: 'only-on-failure', trace: 'retain-on-failure',
  },
});
