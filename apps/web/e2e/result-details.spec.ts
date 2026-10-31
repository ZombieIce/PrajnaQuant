import { test, expect } from '@playwright/test';
import { readFile, readdir, rm, appendFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

test('saved native details distinguish fills, deferral, skipped buys, empty tables, and missing levels', async ({ page }) => {
  test.setTimeout(240_000);
  if (!process.env.PRAJNA_E2E_ROOT) throw new Error('owned synthetic E2E root required');
  const local = process.env.PRAJNA_E2E_ROOT;
  const lake = join(local, 'fixture', 'lake');
  const manifests = join(lake, 'manifests');
  const before = new Set(await readdir(manifests));
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  try {
  const prepared = spawnSync('cargo', ['test', '-p', 'prajna-experiment', '--test', 'timing', '--locked', '--offline', 'export_result_details_fixture', '--', '--ignored', '--exact'],
    { cwd: resolve(process.cwd(), '../..'), env: { ...process.env, PRAJNA_DETAILS_FIXTURE_ROOT: lake }, encoding: 'utf8', timeout: 120_000 });
  await appendFile(join(local, 'details-fixture.log'), `${prepared.stdout ?? ''}${prepared.stderr ?? ''}`);
  expect(prepared.status, 'fixture preparation; diagnostics remain local').toBe(0);
    const definitions = JSON.parse(await readFile(join(lake, 'result-details-definitions.json'), 'utf8'));
    async function run(definition: unknown, level: string, key: string) {
      const submitted = await page.request.post('/api/v1/tasks/experiments', { data: { definition, result_level: level, idempotency_key: key } });
      expect(submitted.status()).toBe(202);
      const task = (await submitted.json()).task;
      await page.goto(`/backtests?task=${encodeURIComponent(task.task_id)}`);
      await expect(page.getByTestId('task-status')).toContainText('已完成', { timeout: 60_000 });
      const result = await page.request.get(`/api/v1/tasks/experiments/${task.task_id}/result`);
      expect(result.status()).toBe(200);
      await expect(page.getByRole('heading', { name: '持仓、现金和执行明细', exact: true })).toBeVisible();
      return (await result.json()).data.runs[0];
    }
    const traded = await run(definitions[0], 'full', 'details-deferred');
    expect(traded.units).toMatchObject({ currency: 'CNY', weight: 'fraction_of_total_equity', timestamp: 'unix_nanoseconds' });
    expect(traded.summary.deferred_count).toBe(1);
    expect(traded.summary.pending_at_end).toBe(true);
    expect(traded.details.fills).toHaveLength(2);
    const fill = traded.details.fills[0].fill;
    // Independent literal golden shared with Rust's ten-Session hand ledger.
    expect(fill.order.quantity).toBe('900'); expect(fill.price).toBe('10.01');
    expect(fill.costs).toEqual({ commission: '2.7027', minimum_commission_top_up: '2.2973', tax: '9.009' });
    expect(fill.slippage).toBe('9');
    expect(fill.executed_at).toBe('1767749400000000000');
    const fills = page.getByTestId('native-fills');
    for (const amount of [fill.price, fill.costs.commission, fill.costs.minimum_commission_top_up, fill.costs.tax, fill.slippage]) {
      await expect(fills).toContainText(amount);
    }
    await expect(fills).toContainText('最低佣金补足');
    await fills.scrollIntoViewIfNeeded();
    await page.screenshot({ path: join(local, 'result-details.png') });
    await expect(fills).toContainText('数量（份）');
    await expect(page.getByLabel('持仓现金与执行明细')).toContainText('金额单位：CNY');
    const deferred = traded.details.orders.find((row: { kind: string }) => row.kind === 'deferred');
    expect(deferred.session_date).toBe('2026-01-09');
    await expect(page.getByTestId('native-orders')).toContainText('延期');
    await expect(page.getByTestId('native-orders')).toContainText(deferred.session_date);
    const pending = traded.details.orders.find((row: { kind: string }) => row.kind === 'pending_at_end');
    expect(pending.decision_session).toBe('2026-01-16');
    await expect(page.getByTestId('native-pending')).toContainText(pending.decision_session);
    const close = traded.allocations.find((row: { session_date: string; valuation_point: string }) => row.session_date === '2026-01-07' && row.valuation_point === 'close');
    expect(close.cash).toBe('976.991'); expect(close.equity).toBe('9976.991');
    expect(close.positions['510300.XSHG'].market_value).toBe('9000');
    await page.getByLabel('持仓估值日期', { exact: true }).selectOption('2026-01-07');
    await expect(page.getByTestId('native-allocations')).toContainText(close.cash_weight);
    await expect(page.getByTestId('native-allocations')).toContainText(close.positions['510300.XSHG'].weight);
    await page.reload();
    await expect(page.getByTestId('native-pending')).toContainText(pending.decision_session);
    await expect(page.getByTestId('native-fills')).toContainText(fill.costs.tax);

    const skipped = await run(definitions[1], 'full', 'details-skipped');
    expect(skipped.summary.skipped_buy_count).toBe(1);
    expect(skipped.details.fills).toEqual([]);
    expect(skipped.details.orders.some((row: { skipped_buys?: string[] }) => row.skipped_buys?.includes('510300.XSHG'))).toBe(true);
    await expect(page.getByTestId('native-fills')).toContainText('已保存成交表为空：该表确认无成交');
    await expect(page.getByTestId('native-orders')).toContainText('跳过买入');
    await expect(page.getByTestId('native-orders')).toContainText('510300.XSHG');
    await expect(page.getByTestId('native-fee_settlements')).toContainText('无独立费用结算');
    const cash = await run(definitions[2], 'full', 'details-cash');
    expect(cash.summary.executed_count).toBe(4); // successful cash targets are execution events, not fills
    expect(cash.details.fills).toEqual([]);
    expect(cash.allocations.every((row: { cash_weight: string }) => row.cash_weight === '1')).toBe(true);
    await expect(page.getByTestId('native-fills')).toContainText('确认无成交');

    // Distinct definitions avoid replaying the Full results into a lower-level task.
    const lower = structuredClone(definitions[2]); lower.fast_event.initial_cash = '20000'; lower.fast_event.portfolios[0].capital = '20000';
    const standard = await run(lower, 'standard', 'details-standard');
    expect(standard.details.fills).toBeUndefined();
    await expect(page.getByTestId('native-fills')).toContainText('未保存');
    await expect(page.getByTestId('native-pending')).toContainText('不能推断为零交易');
    lower.fast_event.initial_cash = '30000'; lower.fast_event.portfolios[0].capital = '30000';
    const summary = await run(lower, 'summary', 'details-summary');
    expect(summary.allocations).toBeNull();
    await expect(page.getByTestId('native-allocations')).toContainText('未保存');
    const vector = await run(definitions[3], 'standard', 'details-vector');
    expect(vector.tables.not_produced_by_engine).toContain('fills');
    expect(vector.details.fills).toBeUndefined();
    await expect(page.getByTestId('native-fills')).toContainText('引擎不产生');
    await expect(page.getByTestId('native-weights_after_execution')).toContainText('未保存');
    await expect(page.getByTestId('native-sessions')).toBeVisible();
    expect(errors).toEqual([]);
  } finally {
    // Only manifests created by this invocation's test are removed from catalog;
    // retained task/results and all logs remain in the owned local synthetic lake.
    for (const name of await readdir(manifests)) if (!before.has(name)) await rm(join(manifests, name));
  }
});
