import { test, expect, type Page } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

const first = '510300.XSHG';
const second = '510880.XSHG';
const members = [first, second];
const mode = 'daily_price_simulation_v1';

function choose(page: Page, label: string, value: string) {
  return page.getByLabel(label, { exact: true }).selectOption(value);
}

function fill(page: Page, label: string, value: string) {
  return page.getByLabel(label, { exact: true }).fill(value);
}

async function submit(page: Page, label: string, path: string) {
  const response = page.waitForResponse(value => new URL(value.url()).pathname === path && value.request().method() === 'POST');
  await page.getByRole('button', { name: label, exact: true }).click();
  return response;
}

async function readData(page: Page, path: string) {
  const response = await page.request.get(path);
  expect(response.ok()).toBe(true);
  return (await response.json()).data;
}

async function editDraft(page: Page, expected: string[], draft: string[]) {
  const response = await page.request.post('/api/v1/research-pools', {
    data: { op: 'save', id: 'cross-asset', expected, members: draft },
  });
  expect(response.ok()).toBe(true);
}

async function setCosts(page: Page) {
  await fill(page, '本金（CNY）', '10000');
  await fill(page, '买入滑点（bp）', '0');
  await fill(page, '卖出滑点（bp）', '0');
}

test('supplier replay reaches factor, daily price ledgers and a frozen price benchmark in the production workbench', async ({ page }) => {
  test.setTimeout(180_000);
  if (!process.env.PRAJNA_E2E_ROOT) throw new Error('Run through npm run test:e2e');
  const receipt = join(process.env.PRAJNA_E2E_ROOT, 'fixture', 'daily-price', 'receipt.json');
  const { dsv, adjustments } = JSON.parse(await readFile(receipt, 'utf8')) as { dsv: string; adjustments: Record<string, string> };
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));

  // Research consumes the adjusted version published from the same supplier replay.
  await page.goto('/research/momentum');
  await choose(page, '研究 Dataset', dsv);
  await choose(page, '研究对象', first);
  await choose(page, `${first} 研究复权版本`, adjustments[first]);
  await choose(page, '研究因子', 'trend_filter_strict');
  await fill(page, '研究窗口', '2');
  const factor = await submit(page, '计算并保存研究', '/api/v1/momentum-research');
  expect(factor.ok()).toBe(true);
  await expect(page.getByTestId('momentum-task-status')).toContainText('succeeded', { timeout: 60_000 });
  const values = page.getByTestId('momentum-values').locator('tbody tr');
  await expect(values.first()).toContainText('insufficient_window');
  await expect(values.nth(1).locator('td').nth(2)).toHaveText('1');

  await page.goto('/backtests');
  await choose(page, '策略模板', 'single_etf_timing');
  await choose(page, '执行模式', mode);
  await choose(page, 'Dataset 版本', dsv);
  await choose(page, 'ETF', first);
  await choose(page, '研究复权版本', adjustments[first]);
  await fill(page, '均线窗口', '999');
  const warmup = await submit(page, '提交趋势择时回测', '/api/v1/tasks/selections');
  expect(warmup.status()).toBe(422);
  expect((await warmup.json()).error.code).toBe('evaluation_window_not_runnable');
  await fill(page, '均线窗口', '2');
  await fill(page, '调仓频率', '1');
  await setCosts(page);
  const timingResponse = await submit(page, '提交趋势择时回测', '/api/v1/tasks/selections');
  expect(timingResponse.ok()).toBe(true);
  const timing = (await timingResponse.json()).task.task_id;
  await expect(page.getByTestId('task-status')).toContainText('已完成', { timeout: 60_000 });
  const strategy = (await readData(page, `/api/v1/tasks/experiments/${timing}/result`)).runs[0];
  expect(strategy.summary.execution_mode).toBe(mode);
  expect(strategy.summary.return_basis).toBe('price_return');
  expect(strategy.accounting_verified).toBe(true);
  const firstClose = strategy.series.account_valuations.find((row: { session_date: string; valuation_point: string }) =>
    row.session_date === '2026-01-07' && row.valuation_point === 'close');
  // Jan 6 adjusted close crosses the 2-Session MA. Jan 7 open is 1.000.
  // 10,000 CNY less 9,900 shares and 5 commission leaves 95 cash.
  // Raw close 1.010 marks 9,999; equity is 95 + 9,999 = 10,094.
  expect(firstClose.cash).toBe('95');
  expect(firstClose.equity).toBe('10094');
  expect(firstClose.positions[first].quantity).toBe('9900');
  expect(strategy.details.fills[0].fill.costs).toMatchObject({ raw_price: '1', total_fees: '5' });
  await expect(page.getByTestId('native-fills').locator('tbody tr').first()).toBeVisible();
  await page.reload();
  await expect(page.getByTestId('result-execution-mode')).toContainText('日线价格模拟');
  await expect(page.getByTestId('valuation-rows')).toContainText('10094');

  const registry = await readData(page, '/api/v1/research-pools');
  const original: string[] = registry.pools.find((pool: { id: string }) => pool.id === 'cross-asset').draft;
  await editDraft(page, original, members);
  try {
    const publish = await page.request.post('/api/v1/research-pools', {
      data: { op: 'publish', id: 'cross-asset', expected: members },
    });
    expect(publish.ok()).toBe(true);
    const published = (await publish.json()).data.pools.find((pool: { id: string }) => pool.id === 'cross-asset').versions.at(-1);
    expect(published.members).toEqual(members);

    await page.goto('/pools');
    await choose(page, '研究池', 'cross-asset');
    await choose(page, '固定版本', String(published.version));
    await choose(page, '研究池 Dataset', dsv);
    await choose(page, '预检策略', 'pool_momentum_topk');
    await choose(page, '执行模式', mode);
    for (const member of members) await choose(page, `复权版本 ${member}`, adjustments[member]);
    await fill(page, '动量窗口', '1');
    await fill(page, 'Top-K', '1');
    await fill(page, '调仓频率', '1');
    await setCosts(page);
    const plan = await submit(page, '预检共同窗口', '/api/v1/tasks/selections/preflight');
    expect((await plan.json()).data.runnable).toBe(true);
    const rotationResponse = await submit(page, '提交固定池实验', '/api/v1/tasks/selections');
    expect(rotationResponse.ok()).toBe(true);
    const rotation = await rotationResponse.json();
    expect(rotation.selection.definition.execution_mode).toBe(mode);
    await page.getByRole('link', { name: '查看固定池任务', exact: true }).click();
    await expect(page.getByTestId('task-status')).toContainText('已完成', { timeout: 60_000 });
    const rotated = (await readData(page, `/api/v1/tasks/experiments/${rotation.task.task_id}/result`)).runs[0];
    expect(rotated.summary.execution_mode).toBe(mode);
    expect(rotated.details.fills[0].fill.order.instrument_id).toBe(first);
    expect(rotated.details.fills[0].fill.order.quantity).toBe('9900');

    await page.goto('/comparisons');
    await choose(page, 'Dataset 版本', dsv);
    await choose(page, '对照模式', 'pool');
    await choose(page, '研究池', 'cross-asset');
    await choose(page, '固定版本', String(published.version));
    await choose(page, '池内模板', 'pool_momentum_topk');
    await choose(page, '执行模式', mode);
    await expect(page.getByLabel('公司行动事实版本', { exact: true })).toHaveCount(0);
    for (const member of members) await choose(page, `复权版本 ${member}`, adjustments[member]);
    await fill(page, '动量窗口', '1');
    await fill(page, 'Top-K', '1');
    await fill(page, '调仓频率', '1');
    await setCosts(page);
    const comparisonPath = '/api/v1/tasks/comparisons';
    const compare = await submit(page, '提交策略—基准对照', comparisonPath);
    expect(compare.ok()).toBe(true);
    const comparison = await compare.json();
    expect(comparison.comparison.comparability).toMatchObject({ dsv, adjusted_versions: adjustments,
      execution_mode: mode, return_basis: 'price_return', corporate_actions: null, reinvestment_calendar: { policy: 'none' } });
    await expect(page.getByTestId('comparison-status')).toContainText('已完成', { timeout: 60_000 });
    const path = `${comparisonPath}/${comparison.task.task_id}/result`;
    const result = await readData(page, path);
    expect(result.verification).toMatchObject({ comparable: true, role_identities_verified: true });
    for (const role of [result.strategy, result.benchmark]) {
      expect(role.runs[0].summary).toMatchObject({ execution_mode: mode, return_basis: 'price_return' });
      expect(role.runs[0].summary.complete.fill_count).toBeGreaterThan(0);
      expect(role.runs[0].summary.complete.calendar.available).toBe(true);
    }
    await expect(page.getByTestId('comparison-facts')).toContainText('日线价格模拟');
    await expect(page.getByTestId('comparison-chart')).toBeVisible();
    await page.reload();
    await expect(page.getByTestId('comparison-facts')).toContainText('扣费后价格收益');
    expect(await readData(page, path)).toEqual(result);

    // Fresh submission key exercises the same fixed inputs beyond a deduplicated request.
    const repeat = await page.request.post(comparisonPath, {
      data: { ...compare.request().postDataJSON(), idempotency_key: 'synthetic-daily-price-repeat' },
    });
    expect(repeat.ok()).toBe(true);
    const repeatTask = (await repeat.json()).task.task_id;
    expect(repeatTask).not.toBe(comparison.task.task_id);
    await page.goto(`/comparisons?task=${encodeURIComponent(repeatTask)}`);
    await expect(page.getByTestId('comparison-status')).toContainText('已完成', { timeout: 60_000 });
    const repeated = await readData(page, `${comparisonPath}/${repeatTask}/result`);
    for (const role of ['strategy', 'benchmark']) {
      expect(repeated[role].exp).toBe(result[role].exp);
      expect(repeated[role].runs).toEqual(result[role].runs);
    }
  } finally {
    await editDraft(page, members, original);
  }
  expect(errors).toEqual([]);
});
