import { test, expect } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

test('strict timing uses configured adjusted signal, freezes costs, reads and reloads the raw-price ledger', async ({ page }) => {
  test.setTimeout(120_000);
  await page.goto('/backtests');
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.getByLabel('策略模板', { exact: true }).selectOption('single_etf_timing');
  const datasets = page.getByLabel('Dataset 版本', { exact: true });
  await expect(datasets.locator('option')).toHaveCount(8);
  const catalog = await (await page.request.get('/api/v1/datasets')).json();
  let dsv = ''; let adjusted = '';
  for (const item of catalog.data.datasets) {
    const detail = await (await page.request.get(`/api/v1/datasets/${encodeURIComponent(item.dsv)}`)).json();
    const version = detail.data.adjusted_series.versions.find((v: { instrument_id: string; method_version: string }) => v.instrument_id === '510300.XSHG' && v.method_version === 'synthetic-timing@1');
    if (version) { dsv = item.dsv; adjusted = version.version; break; }
  }
  expect(dsv).not.toBe('');
  await datasets.selectOption(dsv);
  await page.getByLabel('ETF', { exact: true }).selectOption('510300.XSHG');
  await page.getByLabel('研究复权版本', { exact: true }).selectOption(adjusted);
  await page.getByRole('button', { name: '提交趋势择时回测', exact: true }).click();
  await expect(page.getByText('evaluation_window_not_runnable', { exact: true })).toBeVisible();
  await page.getByLabel('均线窗口', { exact: true }).fill('2');
  await page.getByLabel('调仓频率', { exact: true }).fill('1');
  await page.getByLabel('本金（CNY）', { exact: true }).fill('10000');
  await page.getByLabel('买入滑点（bp）', { exact: true }).fill('10');
  await page.getByLabel('卖出滑点（bp）', { exact: true }).fill('10');
  await page.getByLabel('买入税率', { exact: true }).fill('0.001');
  await page.getByLabel('卖出税率', { exact: true }).fill('0.002');
  await page.getByRole('button', { name: '提交趋势择时回测', exact: true }).click();
  await expect(page.getByTestId('task-status')).toContainText('已完成', { timeout: 60_000 });
  const task = new URL(page.url()).searchParams.get('task');
  expect(task).toBeTruthy();
  if (!process.env.PRAJNA_E2E_ROOT) throw new Error('owned E2E root required');
  const root = join(process.env.PRAJNA_E2E_ROOT, 'fixture', 'lake');
  const frozen = JSON.parse(await readFile(join(root, 'tasks', task!, 'experiment.json'), 'utf8'));
  expect(frozen.strategy).toBe('single_etf_timing');
  expect(frozen.parameter_space.list).toEqual([{ params_version: 1, ma_window: 2, rebalance_every: 1 }]);
  expect(frozen.adjusted_versions).toEqual({ '510300.XSHG': adjusted });
  expect(frozen.fast_event.initial_cash).toBe('10000');
  expect(frozen.fast_event.minimum_commission).toBe('5');
  expect(frozen.fast_event.lot_size_source).toEqual({ kind: 'instrument' });
  expect(frozen.costs).toEqual({ commission_rate: 0.0003, buy_slippage_bps: 10, sell_slippage_bps: 10, buy_tax_rate: 0.001, sell_tax_rate: 0.002 });
  expect(frozen.evaluation.start).toBe('2026-01-06');
  const response = await page.request.get(`/api/v1/tasks/experiments/${task}/result`);
  expect(response.status()).toBe(200);
  const result = await response.json();
  const run = result.data.runs[0];
  expect(run.accounting_verified).toBe(true);
  expect(run.result_level).toBe('full');
  expect(run.tables.saved).toEqual(expect.arrayContaining(['fills', 'orders', 'fee_settlements']));
  expect(run.summary.executed_count).toBe(1);
  expect(run.summary.pending_at_end).toBe(true); // equal at final close -> cash pending
  const rows = [...run.series.account_valuations].sort((a, b) => a.session_date.localeCompare(b.session_date) || a.valuation_point.localeCompare(b.valuation_point));
  expect(rows).toHaveLength(4);
  const last = rows.find(row => row.session_date === '2026-01-07' && row.valuation_point === 'close');
  // Independent raw-price hand calculation: buy 900 @10.01, fee5 + tax9.009;
  // mark at raw10.1, never adjusted12: 976.991+9090=10066.991.
  expect(last.cash).toBe('976.991'); expect(last.equity).toBe('10066.991');
  expect(last.positions['510300.XSHG'].quantity).toBe('900');
  const expected = rows.map(row => [row.session_date, row.valuation_point, row.cash, row.equity,
    Object.values(row.positions ?? {}).map((p: { quantity: string }) => p.quantity).join(' / ') || '无持仓']);
  const table = () => page.getByTestId('valuation-rows').locator('tbody tr').evaluateAll(rows => rows.map(row => Array.from((row as HTMLTableRowElement).cells).map(cell => cell.textContent ?? '')));
  await expect(page.getByTestId('valuation-rows')).toBeVisible(); expect(await table()).toEqual(expected);
  await page.reload(); await expect(page.getByTestId('valuation-rows')).toBeVisible(); expect(await table()).toEqual(expected);
  // Server rejects an invalid native definition through the same production endpoint.
  const invalid = await page.request.post('/api/v1/tasks/selections', { data: { idempotency_key: 'invalid-timing', dsv, instrument: '510300.XSHG', strategy: 'single_etf_timing', adjusted: { '510300.XSHG': adjusted }, parameter_space: { list: [{ params_version: 1, ma_window: 0, rebalance_every: 1 }] } } });
  expect(invalid.status()).toBe(400);
  expect((await invalid.json()).error.code).toBe('invalid_definition');
  expect((await (await page.request.get('/api/v1/runs')).json()).items).toEqual([]);
  expect(errors).toEqual([]);
});
