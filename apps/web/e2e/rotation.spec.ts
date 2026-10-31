import { test, expect, type APIRequestContext, type Page } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

type Adjusted = { version: string; instrument_id: string; method_version?: string };
type Detail = { dsv: string; bars?: number; adjusted_series: { versions: Adjusted[] } };
type Version = { version: number; universe: string; members: string[] };
type Task = { task_id: string };

const lake = () => {
  if (!process.env.PRAJNA_E2E_ROOT) throw new Error('Run through npm run test:e2e');
  return join(process.env.PRAJNA_E2E_ROOT, 'fixture', 'lake');
};

const catalogDetails = async (request: APIRequestContext) => {
  const catalog = (await (await request.get('/api/v1/datasets')).json()).data.datasets as { dsv: string; bars: number }[];
  return Promise.all(catalog.map(async (dataset) => ({
    dsv: dataset.dsv,
    bars: dataset.bars,
    detail: (await (await request.get(`/api/v1/datasets/${encodeURIComponent(dataset.dsv)}`)).json()).data as Detail,
  })));
};

const fixture = async (request: APIRequestContext) => {
  const details = await catalogDetails(request);
  // The momentum fixture publishes the 130-Session executable panel with 650 Bars.
  const executable = details.find((item) => item.bars === 650);
  if (!executable) throw new Error('Expected the 130-Session executable momentum fixture');
  const adjusted = executable.detail.adjusted_series.versions;
  const rising = JSON.parse(await readFile(join(lake(), 'synthetic-momentum-request.json'), 'utf8'));
  const binding = (instrument: string, version?: string) => {
    if (version) return version;
    const found = adjusted.find((item) => item.instrument_id === instrument);
    if (!found) throw new Error(`missing synthetic adjusted version for ${instrument}`);
    return found.version;
  };
  return {
    dsv: executable.dsv,
    versions: {
      '510300.XSHG': binding('510300.XSHG', rising.adjusted['510300.XSHG']),
      '510880.XSHG': binding('510880.XSHG'),
      '159952.XSHE': binding('159952.XSHE'),
    } as Record<string, string>,
  };
};

/** Publish one immutable cross-asset version through the production pool entry. */
const publishPool = async (request: APIRequestContext, members: string[]) => {
  const registry = (await (await request.get('/api/v1/research-pools')).json()).data;
  const pool = registry.pools.find((item: { id: string }) => item.id === 'cross-asset');
  const expected = [...members].sort();
  if (JSON.stringify(pool.draft) !== JSON.stringify(expected)) {
    expect((await request.post('/api/v1/research-pools', {
      data: { op: 'save', id: 'cross-asset', expected: pool.draft, members },
    })).ok()).toBe(true);
  }
  const published = await request.post('/api/v1/research-pools', {
    data: { op: 'publish', id: 'cross-asset', expected },
  });
  expect(published.ok()).toBe(true);
  const version = (await published.json()).data.pools
    .find((item: { id: string }) => item.id === 'cross-asset').versions.at(-1) as Version;
  expect([...version.members].sort()).toEqual(expected);
  return version.version;
};

const select = async (page: Page, version: number, dsv: string, bindings: Record<string, string>) => {
  await page.goto('/pools');
  await page.getByLabel('研究池', { exact: true }).selectOption('cross-asset');
  await page.getByLabel('固定版本', { exact: true }).selectOption(String(version));
  await page.getByLabel('研究池 Dataset', { exact: true }).selectOption(dsv);
  await page.getByLabel('预检策略', { exact: true }).selectOption('pool_momentum_topk');
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

const fillRotation = async (page: Page, lookback: string, topK: string, frequency: string, cash: string, costs?: { rate: string; minimum: string }) => {
  await page.getByLabel('动量窗口', { exact: true }).fill(lookback);
  await page.getByLabel('Top-K', { exact: true }).fill(topK);
  await page.getByLabel('调仓频率', { exact: true }).fill(frequency);
  await page.getByLabel('本金（CNY）', { exact: true }).fill(cash);
  if (costs) {
    await page.getByLabel('佣金率', { exact: true }).fill(costs.rate);
    await page.getByLabel('最低佣金（CNY）', { exact: true }).fill(costs.minimum);
  }
};

test.afterEach(async ({ request }) => {
  const registry = (await (await request.get('/api/v1/research-pools')).json()).data;
  const pool = registry.pools.find((item: { id: string }) => item.id === 'cross-asset');
  const preset = ['513300.XSHG', '518880.XSHG', '159925.XSHE', '159612.XSHE', '159952.XSHE'].sort();
  if (JSON.stringify(pool.draft) !== JSON.stringify(preset)) {
    expect((await request.post('/api/v1/research-pools', {
      data: { op: 'save', id: 'cross-asset', expected: pool.draft, members: preset },
    })).ok()).toBe(true);
  }
});

test('pool momentum Top-K freezes parameters/costs, executes at the next Session and reads persisted values', async ({ page }) => {
  test.setTimeout(180_000);
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  const data = await fixture(page.request);
  const version = await publishPool(page.request, ['510300.XSHG', '510880.XSHG']);

  await select(page, version, data.dsv, { '510300.XSHG': data.versions['510300.XSHG'], '510880.XSHG': data.versions['510880.XSHG'] });
  await fillRotation(page, '1', '3', '5', '10000', { rate: '0.001', minimum: '1' });
  await expect(page.getByTestId('rotation-parameters')).toContainText('动量窗口 1 · Top-K 3 · 调仓频率 5');
  const planned = await (await preflight(page)).json();
  expect(planned.data.runnable).toBe(true);
  expect(planned.data.window.start).toBe('2026-01-06');
  expect(planned.data.window.warmup_sessions).toBe(1);

  const { frozen, taskId } = await submit(page);
  await expect(frozen).toContainText('cross-asset v');
  await expect(page.getByTestId('frozen-rotation-parameters')).toContainText('动量窗口 1 · Top-K 3 · 调仓频率 5');
  await expect(page.getByTestId('frozen-rotation-parameters')).toContainText('本金 10000 CNY');
  await expect(page.getByTestId('frozen-rotation-parameters')).toContainText('佣金率 0.001');
  const definition = JSON.parse(await readFile(join(lake(), 'tasks', taskId, 'experiment.json'), 'utf8'));
  expect(definition.strategy).toBe('pool_momentum_topk');
  expect(definition.parameter_space.list).toEqual([{ params_version: 1, lookback: 1, top_k: 3, rebalance_every: 5 }]);
  expect(definition.adjusted_versions).toEqual({
    '510300.XSHG': data.versions['510300.XSHG'],
    '510880.XSHG': data.versions['510880.XSHG'],
  });
  expect(definition.costs).toEqual({ commission_rate: 0.001, buy_slippage_bps: 2, sell_slippage_bps: 2, buy_tax_rate: 0, sell_tax_rate: 0 });
  expect(definition.fast_event.initial_cash).toBe('10000');
  expect(definition.fast_event.minimum_commission).toBe('1');
  expect(definition.fast_event.lot_size_source.kind).toBe('instrument');
  expect(definition.evaluation.start).toBe('2026-01-06');

  await page.goto(`/backtests?task=${encodeURIComponent(taskId)}`);
  await expect(page.getByTestId('task-status')).toContainText('已完成', { timeout: 90_000 });
  const result = await (await page.request.get(`/api/v1/tasks/experiments/${taskId}/result`)).json();
  const run = result.data.runs[0];
  expect(run.result_level).toBe('full');
  expect(run.tables.saved).toEqual(expect.arrayContaining(['fills', 'orders', 'fee_settlements']));
  const rows = run.series.account_valuations as {
    session_date: string; valuation_point: string; cash: string; equity: string;
    positions: Record<string, { quantity: string }>;
  }[];
  const before = rows.find((row) => row.session_date === '2026-01-06' && row.valuation_point === 'close');
  const first = rows.find((row) => row.session_date === '2026-01-07' && row.valuation_point === 'close');
  // Independent hand calculation on raw open/close 10 and adjusted signals:
  // target sizing is 5,000 CNY per member at raw 10 → 500 shares each; 510300
  // (100-share lot) pays 5,001 + 5.001 commission, so 510880's 10-share lot is
  // cash-limited from 500 to 490. Cash 10,000 − 5,006.001 − 4,905.88098 =
  // 88.11802; close equity 88.11802 + 5,000 + 4,900 = 9,988.11802.
  expect(before.positions).toEqual({});
  expect(before.cash).toBe('10000');
  expect(first.positions['510300.XSHG'].quantity).toBe('500');
  expect(first.positions['510880.XSHG'].quantity).toBe('490');
  expect(first.cash).toBe('88.11802');
  expect(first.equity).toBe('9988.11802');
  // The next scheduled Decision (five Sessions after 2026-01-06) re-targets the
  // smaller equity and sells one 100-share lot of 510300 at 2026-01-13's open.
  expect(run.details.fills).toHaveLength(3);
  expect(run.details.fills[0].fill.order.quantity).toBe('500');
  expect(run.details.fills[0].fill.costs.commission).toBe('5.001');
  expect(run.details.fills[1].fill.order.quantity).toBe('490');
  expect(run.details.fills[1].fill.costs.commission).toBe('4.90098');
  expect(run.details.fills[2].fill.order.quantity).toBe('100');
  expect(run.details.fills[2].fill.order.side).toBe('Sell');
  expect(run.details.fills[2].fill.costs.commission).toBe('0.9998');

  const table = () => page.getByTestId('valuation-rows').locator('tbody tr').evaluateAll((body) =>
    body.map((row) => Array.from((row as HTMLTableRowElement).cells).map((cell) => cell.textContent ?? '')));
  await expect(page.getByTestId('valuation-rows')).toBeVisible();
  const rendered = await table();
  expect(rendered.some((row) => row.includes('2026-01-07') && row.includes('88.11802') && row.includes('9988.11802'))).toBe(true);
  await page.reload();
  await expect(page.getByTestId('valuation-rows')).toBeVisible();
  expect(await table()).toEqual(rendered);
  expect(errors).toEqual([]);
});

test('pool momentum ties select the lower InstrumentId and respect actual candidate count', async ({ page }) => {
  test.setTimeout(180_000);
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  const data = await fixture(page.request);
  // Both members are flat synthetic closes: the momentum scores tie exactly.
  const version = await publishPool(page.request, ['510880.XSHG', '159952.XSHE']);
  await select(page, version, data.dsv, {
    '510880.XSHG': data.versions['510880.XSHG'],
    '159952.XSHE': data.versions['159952.XSHE'],
  });
  await fillRotation(page, '1', '1', '5', '10000');
  await (await preflight(page)).json();
  const { taskId } = await submit(page);
  await page.goto(`/backtests?task=${encodeURIComponent(taskId)}`);
  await expect(page.getByTestId('task-status')).toContainText('已完成', { timeout: 90_000 });
  const result = await (await page.request.get(`/api/v1/tasks/experiments/${taskId}/result`)).json();
  const rows = result.data.runs[0].series.account_valuations as {
    session_date: string; valuation_point: string; cash: string; equity: string;
    positions: Record<string, { quantity: string }>;
  }[];
  const first = rows.find((row) => row.session_date === '2026-01-07' && row.valuation_point === 'close');
  // Tied scores break by ascending InstrumentId: 159952.XSHE wins the single slot.
  expect(first.positions['159952.XSHE'].quantity).toBe('900');
  expect(first.positions['510880.XSHG']).toBeUndefined();
  // 10,000/10 = 1,000 shares; 1,000 × 10.002 + 5 > cash, so the affordable
  // 100-share lot is 900 → 9,006.8 spent, cash 993.2, equity 9,993.2.
  expect(first.cash).toBe('993.2');
  expect(first.equity).toBe('9993.2');
  const task = JSON.parse(await readFile(join(lake(), 'tasks', taskId, 'experiment.json'), 'utf8'));
  expect(task.parameter_space.list[0].top_k).toBe(1);
  expect(task.adjusted_versions).toEqual({
    '510880.XSHG': data.versions['510880.XSHG'],
    '159952.XSHE': data.versions['159952.XSHE'],
  });
  expect(errors).toEqual([]);
});

test('the same production entry rejects an invalid rotation definition without a fabricated task result', async ({ page }) => {
  const data = await fixture(page.request);
  const version = await publishPool(page.request, ['510300.XSHG', '510880.XSHG']);
  const response = await page.request.post('/api/v1/tasks/selections', {
    data: {
      idempotency_key: 'invalid-pool-momentum',
      dsv: data.dsv,
      pool: { id: 'cross-asset', version },
      strategy: 'pool_momentum_topk',
      adjusted: { '510300.XSHG': data.versions['510300.XSHG'], '510880.XSHG': data.versions['510880.XSHG'] },
      parameter_space: { list: [{ params_version: 1, lookback: 0, top_k: 0, rebalance_every: 0 }] },
      result_level: 'full',
    },
  });
  expect(response.status()).toBe(400);
  expect((await response.json()).error.code).toBe('invalid_definition');
  expect((await (await page.request.get('/api/v1/runs')).json()).items).toEqual([]);
});
