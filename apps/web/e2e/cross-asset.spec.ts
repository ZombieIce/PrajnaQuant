import { test, expect, type APIRequestContext, type Page } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

const lake = () => {
  if (!process.env.PRAJNA_E2E_ROOT) throw new Error('Run through npm run test:e2e');
  return join(process.env.PRAJNA_E2E_ROOT, 'fixture', 'lake');
};
type Adjusted = { version: string; instrument_id: string };
type Detail = { dsv: string; adjusted_series: { versions: Adjusted[] } };
type Version = { version: number; universe: string; members: string[] };

const bindingsFor = (detail: Detail) => {
  const bindings: Record<string, string> = {};
  for (const member of ['510300.XSHG', '159952.XSHE']) {
    const version = detail.adjusted_series.versions.find((item) => item.instrument_id === member);
    if (!version) throw new Error(`missing synthetic adjusted version for ${member}`);
    bindings[member] = version.version;
  }
  return bindings;
};

/** The two test-only executable multi-Venue Datasets: matching axis and mismatch. */
const fixtures = async (request: APIRequestContext) => {
  const catalog = (await (await request.get('/api/v1/datasets')).json()).data.datasets as {
    dsv: string; normalizer_version: string; sessions: number; venues: string[];
  }[];
  const executable = catalog.find((item) => item.normalizer_version === 'synthetic-pool-executable@1' && item.sessions === 6);
  const mismatched = catalog.find((item) => item.normalizer_version === 'synthetic-pool-executable@1' && item.sessions === 5);
  if (!executable || !mismatched) throw new Error('Expected the executable and mismatched shared-axis fixture Datasets');
  expect([...executable.venues].sort()).toEqual(['XSHE', 'XSHG']);
  const detail = async (dsv: string) => (await (await request.get(`/api/v1/datasets/${encodeURIComponent(dsv)}`)).json()).data as Detail;
  return {
    executable: executable.dsv,
    mismatched: mismatched.dsv,
    executableBindings: bindingsFor(await detail(executable.dsv)),
    mismatchedBindings: bindingsFor(await detail(mismatched.dsv)),
  };
};

/** Publish one immutable XSHG+XSHE version through the production pool entry. */
const publishMixedPool = async (request: APIRequestContext) => {
  const registry = (await (await request.get('/api/v1/research-pools')).json()).data;
  const pool = registry.pools.find((item: { id: string }) => item.id === 'cross-asset');
  const saved = await request.post('/api/v1/research-pools', {
    data: { op: 'save', id: 'cross-asset', expected: pool.draft, members: ['510300.XSHG', '159952.XSHE'] },
  });
  expect(saved.ok()).toBe(true);
  const draft = (await saved.json()).data.pools.find((item: { id: string }) => item.id === 'cross-asset').draft;
  const published = await request.post('/api/v1/research-pools', { data: { op: 'publish', id: 'cross-asset', expected: draft } });
  expect(published.ok()).toBe(true);
  const version = (await published.json()).data.pools
    .find((item: { id: string }) => item.id === 'cross-asset').versions.at(-1) as Version;
  expect([...version.members].sort()).toEqual(['159952.XSHE', '510300.XSHG']);
  return version;
};

const select = async (page: Page, version: number, dsv: string, bindings: Record<string, string>) => {
  await page.goto('/pools');
  await page.getByLabel('研究池', { exact: true }).selectOption('cross-asset');
  await page.getByLabel('固定版本', { exact: true }).selectOption(String(version));
  await page.getByLabel('研究池 Dataset', { exact: true }).selectOption(dsv);
  for (const [member, binding] of Object.entries(bindings)) {
    await page.getByLabel(`复权版本 ${member}`, { exact: true }).selectOption(binding);
  }
};

const preflight = async (page: Page) => {
  const response = page.waitForResponse((value) => value.url().endsWith('/selections/preflight'));
  await page.getByRole('button', { name: '预检共同窗口', exact: true }).click();
  return response;
};

const submit = async (page: Page) => {
  await page.getByRole('button', { name: '提交固定池实验', exact: true }).click();
  const frozen = page.getByTestId('frozen-selection');
  await expect(frozen).toBeVisible();
  const link = page.getByRole('link', { name: '查看固定池任务', exact: true });
  const taskId = new URL((await link.getAttribute('href'))!, 'http://localhost').searchParams.get('task')!;
  return { frozen, taskId };
};

test.afterEach(async ({ request }) => {
  const registry = (await (await request.get('/api/v1/research-pools')).json()).data;
  const pool = registry.pools.find((item: { id: string }) => item.id === 'cross-asset');
  const preset = ['513300.XSHG', '518880.XSHG', '159925.XSHE', '159612.XSHE', '159952.XSHE'].sort();
  if (JSON.stringify(pool.draft) !== JSON.stringify(preset)) {
    expect((await request.post('/api/v1/research-pools', { data: { op: 'save', id: 'cross-asset', expected: pool.draft, members: preset } })).ok()).toBe(true);
  }
});

test('cross-asset preflight shows both Venues and the frozen identity, and a shared-axis disagreement stays a visible structural error', async ({ page }) => {
  test.setTimeout(120_000);
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  const fixture = await fixtures(page.request);
  const version = await publishMixedPool(page.request);

  await select(page, version.version, fixture.executable, fixture.executableBindings);
  const planned = await (await preflight(page)).json();
  expect(planned.data.runnable).toBe(true);
  expect([...planned.data.venues].sort()).toEqual(['XSHE', 'XSHG']);
  await expect(page.getByTestId('plan-venues')).toContainText('XSHE');
  await expect(page.getByTestId('plan-venues')).toContainText('XSHG');
  await expect(page.getByTestId('plan-venues')).toContainText('不合并为单一 Venue');

  const { frozen, taskId } = await submit(page);
  await expect(frozen).toContainText('cross-asset v');
  await expect(frozen).toContainText(fixture.executable);
  await expect(frozen).toContainText('XSHE');
  await expect(frozen).toContainText('XSHG');
  await expect(frozen).toContainText('fast_event@14');
  await expect(frozen).toContainText('评价窗口：2026-01-05 ～ 2026-01-07');
  const definition = JSON.parse(await readFile(join(lake(), 'tasks', taskId, 'experiment.json'), 'utf8'));
  expect(definition.venues).toEqual(['XSHE', 'XSHG']);
  expect(definition.venue).toBeUndefined();
  expect(definition.fast_event.rebalance).toBe('full_target');
  expect(definition.fast_event.entry_budget).toBeUndefined();
  expect(definition.unfilled_entry).toBe('retry');
  expect(definition.universe).toBe(version.universe);
  expect(definition.dsv).toBe(fixture.executable);
  expect(definition.evaluation.semantic_version).toBe('common-window@1');
  expect(definition.evaluation.start).toBe(planned.data.window.start);
  expect(definition.evaluation.end).toBe(planned.data.window.end);

  await select(page, version.version, fixture.mismatched, fixture.mismatchedBindings);
  const failed = await preflight(page);
  expect(failed.status()).toBe(422);
  const alert = page.getByRole('alert');
  await expect(alert).toContainText('inconsistent_venue_calendar');
  await expect(alert).toContainText('inconsistent shared Session axis');
  await expect(alert).toContainText('结构性共享日历错误');
  await expect(page.getByRole('button', { name: '提交固定池实验', exact: true })).toBeDisabled();
  await expect(page.getByTestId('frozen-selection')).toHaveCount(0);
  expect(errors).toEqual([]);
});

test('cross-asset pool baseline runs through the production entry and reads the persisted result', async ({ page }) => {
  test.setTimeout(180_000);
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  const fixture = await fixtures(page.request);
  const version = await publishMixedPool(page.request);

  await select(page, version.version, fixture.executable, fixture.executableBindings);
  await (await preflight(page)).json();
  const { taskId } = await submit(page);

  await page.goto(`/backtests?task=${encodeURIComponent(taskId)}`);
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('回测与结果');
  await expect(page.getByTestId('task-status')).toContainText('已完成', { timeout: 90_000 });
  const result = await (await page.request.get(`/api/v1/tasks/experiments/${taskId}/result`)).json();
  const rows = result.data.runs[0].series.account_valuations as {
    session_date: string; valuation_point: string; cash: string; equity: string;
    positions: Record<string, { quantity: string }>;
  }[];
  const summary = result.data.runs[0].summary;
  await expect(page.getByTestId('result-metrics')).toContainText(`${(summary.total_return * 100).toFixed(2)}%`);
  const entered = rows.find((row) => row.session_date === '2026-01-06' && row.valuation_point === 'close');
  const opened = rows.find((row) => row.session_date === '2026-01-05' && row.valuation_point === 'close');
  if (!entered || !opened) throw new Error('Expected the persisted evaluation-window rows');
  expect(opened.positions).toEqual({});
  expect(opened.cash).toBe('1000000');
  expect(entered.cash).toBe('500.44006');
  expect(entered.positions['159952.XSHE'].quantity).toBe('50000');
  expect(entered.positions['510300.XSHG'].quantity).toBe('49900');
  expect(entered.equity).toBe('999500.44006');
  const valuationTable = page.getByTestId('valuation-rows');
  await expect(valuationTable).toContainText('500.44006');
  await expect(valuationTable).toContainText('999500.44006');
  await expect(valuationTable).toContainText('159952.XSHE 50000');
  await expect(valuationTable).toContainText('510300.XSHG 49900');
  // Re-reading the same task after a refresh keeps the persisted status and rows.
  await page.reload();
  await expect(page.getByTestId('task-status')).toContainText('已完成');
  await expect(page.getByTestId('valuation-rows')).toContainText('999500.44006');
  expect(await readFile(join(lake(), 'tasks', taskId, 'experiment.json'), 'utf8')).toContain('"venues"');
  expect(errors).toEqual([]);
});
