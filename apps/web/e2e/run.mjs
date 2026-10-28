// Each invocation owns a fresh synthetic lake and artifacts directory.
import { spawnSync } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
const web = fileURLToPath(new URL('..', import.meta.url));
const root = resolve(web, '../..');
const local = mkdtempSync(join(tmpdir(), 'prajna-web-e2e-'));
const env = { ...process.env, PRAJNA_E2E_ROOT: local,
  CARGO_TARGET_DIR: resolve(root, process.env.CARGO_TARGET_DIR ?? 'target') };
console.log(`Synthetic E2E artifacts (local only): ${local}`);
const result = spawnSync(join(web, 'node_modules/.bin/playwright'), ['test', ...process.argv.slice(2)],
  { cwd: web, env, stdio: 'inherit' });
if (result.error) throw result.error;
process.exit(result.status ?? 1);
