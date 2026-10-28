// Build production artifacts, export the checked-in synthetic publisher fixture,
// then run the real service. Playwright owns termination of this process tree.
import { spawn, spawnSync } from 'node:child_process';
import { appendFileSync, createWriteStream, existsSync, readdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
const web = fileURLToPath(new URL('..', import.meta.url));
const root = resolve(web, '../..');
const local = process.env.PRAJNA_E2E_ROOT;
if (!local) throw new Error('Use npm run test:e2e (fresh owned root required)');
const fixture = join(local, 'fixture');
const lake = join(fixture, 'lake');
const target = process.env.CARGO_TARGET_DIR ?? join(root, 'target');
const bin = (name) => join(target, 'debug', name + (process.platform === 'win32' ? '.exe' : ''));
function run(command, args, cwd = root, extra = {}) {
  appendFileSync(join(local, 'server.log'), `\n$ ${command} ${args.join(' ')}\n`);
  const result = spawnSync(command, args, { cwd, env: { ...process.env, ...extra },
    encoding: 'utf8', timeout: 1_200_000, maxBuffer: 16 * 1024 * 1024 });
  appendFileSync(join(local, 'server.log'), `${result.stdout ?? ''}${result.stderr ?? ''}`);
  if (result.error || result.status !== 0) {
    console.error(result.stderr ?? result.error);
    throw new Error(`E2E preparation failed: ${command} (see ${local}/server.log)`);
  }
}
run('npm', ['run', 'build'], web);
run('cargo', ['build', '-p', 'quant-research', '--bin', 'quant-research', '--locked', '--offline']);
run('cargo', ['build', '-p', 'prajna-experiment', '--bins', '--example', 'selection_fixture', '--locked', '--offline']);
run('cargo', ['build', '-p', 'prajna-data', '--bin', 'prajna-publish-daily', '--locked', '--offline']);
run('cargo', ['test', '-p', 'prajna-data', '--test', 'dataset_preview_read', '--locked', '--offline',
  'export_synthetic_preview_fixture', '--', '--ignored', '--exact'], root,
  { PRAJNA_PREVIEW_FIXTURE_ROOT: fixture });
if (!existsSync(join(fixture, 'synthetic-preview-fixture'))) throw new Error('Fixture export did not run');
// Publish #350 @2 status Datasets into the same synthetic lake. The backtests
// page submits through T04b, which requires --dataset-root to equal --output.
run(bin(join('examples', 'selection_fixture')), [lake]);
const manifests = readdirSync(join(lake, 'manifests')).filter((name) => name.endsWith('.json'));
if (manifests.length < 4) throw new Error(`Expected preview and selection Datasets, found ${manifests.length}`);
run(bin('prajna-experiment'), ['--help']);
const log = createWriteStream(join(local, 'server.log'), { flags: 'a' });
const child = spawn(bin('quant-research'), ['serve', '--address', `127.0.0.1:${process.env.PRAJNA_E2E_PORT}`,
  '--web-dist', join(web, 'dist'), '--dataset-root', lake, '--output', lake,
  '--market-data-dir', join(local, 'market')],
  { cwd: root, env: { ...process.env, PRAJNA_DATASET_PREVIEW_CLI: bin('prajna-preview-dataset'),
    PRAJNA_EXPERIMENT_CLI: bin('prajna-experiment'), PRAJNA_SELECTION_CLI: bin('prajna-freeze-selection') },
    stdio: ['ignore', 'pipe', 'pipe'] });
child.stdout.pipe(log); child.stderr.pipe(log);
child.on('error', (error) => { console.error(error); process.exitCode = 1; });
child.on('exit', (code, signal) => { if (!signal) process.exitCode = code ?? 1; log.end(); });
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => child.kill(signal));
