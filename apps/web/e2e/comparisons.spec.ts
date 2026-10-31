import { test, expect, type Page } from '@playwright/test';
import { readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const lake = () => {
  if (!process.env.PRAJNA_E2E_ROOT) throw new Error('Run through npm run test:e2e');
  return join(process.env.PRAJNA_E2E_ROOT, 'fixture', 'lake');
};
const target = () => process.env.CARGO_TARGET_DIR ?? resolve(process.cwd(), '../..', 'target');
const hex = (identity: string) => {
  const [, value] = identity.split(':sha256:');
  if (!value) throw new Error(`Unexpected identity ${identity}`);
  return value;
};
const resultRows = async (page: Page, testId: string) => page.getByTestId(testId).locator('tbody tr').evaluateAll((rows) => rows
  .map((row) => Array.from((row as HTMLTableRowElement).cells).map((cell) => cell.textContent ?? '')));
type CloseRow = { session_date: string; valuation_point: string; cash: string; equity: string; positions?: Record<string, { quantity: string }> };
const closeRows = (run: { series?: { account_valuations?: CloseRow[] } }): CloseRow[] =>
  (run.series?.account_valuations ?? []).filter((row) => row.valuation_point === 'close')
    .sort((left, right) => left.session_date.localeCompare(right.session_date));
const position = (row: { positions?: Record<string, { quantity: string }> }) => Object.entries(row.positions ?? {})
  .map(([id, value]) => `${id} ${value.quantity}`).join('；') || '无持仓';
const calendarRows = async (page: Page, testId: string) => page.getByTestId(testId).locator('tbody tr').evaluateAll((rows) => rows.map((row) => {
  const data = (row as HTMLTableRowElement).dataset;
  return { label: data.label, start: data.start, end: data.end, value: data.return };
}));
const expectedPeriods = (periods: { label: string; start_session: string | null; end_session: string; return_value: number }[]) =>
  periods.map((period) => ({ label: period.label, start: period.start_session ?? 'initial_capital', end: period.end_session, value: String(period.return_value) }));

async function catalogDetails(page: Page) {
  const catalog = (await (await page.request.get('/api/v1/datasets')).json()).data.datasets as { dsv: string; normalizer_version: string; sessions: number; bars: number; instruments: number }[];
  return Promise.all(catalog.map(async (item) => ({ ...item, detail: (await (await page.request.get(`/api/v1/datasets/${encodeURIComponent(item.dsv)}`)).json()).data })));
}

async function originalActions(page: Page) {
  const catalog = (await (await page.request.get('/api/v1/corporate-actions')).json()).data as { versions: { version: string; observed_at: string }[] };
  return [...catalog.versions].sort((left, right) => left.observed_at.localeCompare(right.observed_at))[0].version;
}

async function selectLabel(page: Page, label: string, value: string) {
  await page.getByLabel(label, { exact: true }).selectOption(value);
}

async function fillCosts(page: Page, { cash = '10000', commission = '0.0003', slippage = '10', buyTax = '0.001', sellTax = '0.002' } = {}) {
  await page.getByLabel('本金（CNY）', { exact: true }).fill(cash);
  await page.getByLabel('佣金率', { exact: true }).fill(commission);
  await page.getByLabel('最低佣金（CNY）', { exact: true }).fill('5');
  await page.getByLabel('买入滑点（bp）', { exact: true }).fill(slippage);
  await page.getByLabel('卖出滑点（bp）', { exact: true }).fill(slippage);
  await page.getByLabel('买入税率', { exact: true }).fill(buyTax);
  await page.getByLabel('卖出税率', { exact: true }).fill(sellTax);
}

test('frozen single-ETF comparison reads authoritative roles, reopens, and exposes corrupt or missing benchmark results', async ({ page }) => {
  test.setTimeout(120_000);
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  const details = await catalogDetails(page);
  const timing = details.find((item) => item.detail.adjusted_series?.versions?.some((version: { instrument_id: string; method_version: string }) =>
    version.instrument_id === '510300.XSHG' && version.method_version === 'synthetic-timing@1'));
  expect(timing).toBeTruthy();
  const adjusted = timing!.detail.adjusted_series.versions.find((version: { instrument_id: string; method_version: string }) =>
    version.instrument_id === '510300.XSHG' && version.method_version === 'synthetic-timing@1').version;
  const actions = await originalActions(page);

  await page.goto('/comparisons');
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('策略—基准对照');
  await selectLabel(page, 'Dataset 版本', timing!.dsv);
  await expect(page.getByLabel('ETF', { exact: true }).locator('option')).toHaveCount(2);
  await selectLabel(page, 'ETF', '510300.XSHG');
  await selectLabel(page, '复权版本 510300.XSHG', adjusted);
  await selectLabel(page, '公司行动事实版本', actions);
  await selectLabel(page, '再投资日历', 'strategy_rebalance');
  await page.getByLabel('均线窗口', { exact: true }).fill('2');
  await page.getByLabel('调仓频率', { exact: true }).fill('1');
  await fillCosts(page);
  const submitted = page.waitForResponse((response) => response.url().endsWith('/api/v1/tasks/comparisons') && response.request().method() === 'POST');
  await page.getByRole('button', { name: '提交策略—基准对照', exact: true }).click();
  const submission = await submitted;
  expect(submission.status()).toBe(202);
  expect(submission.headers()['cache-control']).toBe('no-store');
  const duplicate = await page.request.post('/api/v1/tasks/comparisons', { data: submission.request().postDataJSON() });
  expect(duplicate.status()).toBe(200);
  expect(duplicate.headers()['cache-control']).toBe('no-store');
  await expect(page.getByTestId('comparison-status')).toContainText('已完成', { timeout: 60_000 });
  await expect(page.getByTestId('comparison-valuations')).toBeVisible();

  const taskId = new URL(page.url()).searchParams.get('task');
  expect(taskId).toBeTruthy();
  for (const path of ['', `/${taskId}`, `/${taskId}/result`, '/invalid-id']) {
    const response = await page.request.get(`/api/v1/tasks/comparisons${path}`);
    expect(response.headers()['cache-control']).toBe('no-store');
  }
  const task = await (await page.request.get(`/api/v1/tasks/comparisons/${taskId}`)).json();
  expect(task.task_type).toBe('prajna_comparison');
  expect(task.status).toBe('succeeded');
  expect(task.result.strategy.exp).toBeTruthy();
  expect(task.result.benchmark.exp).toBeTruthy();
  const result = await (await page.request.get(`/api/v1/tasks/comparisons/${taskId}/result`)).json();
  expect(result.schema_version).toBe(1);
  expect(result.data.verification.comparable).toBe(true);
  expect(result.data.verification.role_identities_verified).toBe(true);
  expect(result.data.comparability.initial_cash).toBe('10000');
  expect(result.data.comparability.evaluation.start).toBe('2026-01-06');
  expect(result.data.comparability.evaluation.end).toBe('2026-01-07');
  expect(result.data.comparability.reinvestment_calendar.decision_dates).toEqual(['2026-01-07']);
  expect(result.data.comparability.corporate_actions.version).toBe(actions);
  const strategyRows = closeRows(result.data.strategy.runs[0]);
  const benchmarkRows = closeRows(result.data.benchmark.runs[0]);
  expect(strategyRows).toHaveLength(2);
  expect(benchmarkRows).toHaveLength(2);
  expect(benchmarkRows[0].cash).toBe('10000');
  expect(benchmarkRows[0].equity).toBe('10000');
  expect(benchmarkRows[1].equity).toBe('10066.991');
  expect(benchmarkRows[1].cash).toBe('976.991');
  expect(result.data.benchmark.runs[0].summary.total_return).toBeCloseTo(0.0066991, 10);
  const strategyComplete = result.data.strategy.runs[0].summary.complete;
  const benchmarkComplete = result.data.benchmark.runs[0].summary.complete;
  expect(strategyComplete.semantic_version).toBe('complete-performance@1');
  expect(benchmarkComplete.semantic_version).toBe('complete-performance@1');
  const calendar = strategyComplete.calendar;
  expect(calendar.semantic_version).toBe('calendar-performance@1');
  expect(calendar.valuation_point).toBe('close');
  expect(calendar.initial_capital).toBe('10000');
  expect(calendar.evaluation_start).toBe('2026-01-06');
  expect(calendar.evaluation_end).toBe('2026-01-07');
  expect(calendar.sessions).toBe(2);
  expect(calendar.available).toBe(true);
  expect(calendar.monthly).toHaveLength(1);
  expect(calendar.monthly[0].label).toBe('2026-01');
  expect(calendar.monthly[0].start_basis).toBe('initial_capital');
  expect(calendar.monthly[0].start_session).toBe(null);
  expect(calendar.monthly[0].end_session).toBe('2026-01-07');
  expect(calendar.max_drawdown).toBeCloseTo(result.data.strategy.runs[0].summary.max_drawdown, 12);
  const strategyMonthly = await calendarRows(page, 'comparison-monthly-strategy');
  const strategyAnnual = await calendarRows(page, 'comparison-annual-strategy');
  expect(strategyMonthly).toEqual(expectedPeriods(calendar.monthly));
  expect(strategyAnnual).toEqual(expectedPeriods(calendar.annual));
  expect(await calendarRows(page, 'comparison-monthly-benchmark')).toEqual(expectedPeriods(benchmarkComplete.calendar.monthly));
  await expect(page.getByTestId('comparison-performance-strategy')).toContainText('complete-performance@1');
  await expect(page.getByTestId('comparison-complete-metrics-strategy')).toContainText(`${(strategyComplete.metrics.total_return.value * 100).toFixed(2)}%`);
  expect(strategyComplete.metrics.calmar).toEqual({ value: null, reason: 'zero_drawdown' });
  expect(strategyComplete.metrics.max_drawdown.value).toBe(0);
  expect([calendar.drawdown_peak_session, calendar.drawdown_trough_session]).toEqual([undefined, undefined]);
  await expect(page.getByTestId('comparison-complete-metrics-strategy')).toContainText('最大回撤为零，Calmar 无定义');
  await expect(page.getByTestId('comparison-complete-metrics-strategy')).toContainText(`${(strategyComplete.metrics.benchmark_return.value * 100).toFixed(2)}%`);
  expect(await page.getByTestId('comparison-amounts-strategy').locator('td[data-amount="total_execution_cost"]').innerText()).toBe(strategyComplete.amounts.total_execution_cost);
  expect(await page.getByTestId('comparison-amounts-benchmark').locator('td[data-amount="initial_capital"]').innerText()).toBe(benchmarkComplete.amounts.initial_capital);
  await expect(page.getByTestId('comparison-calendar-strategy')).toContainText('calendar-performance@1');
  await expect(page.getByTestId('comparison-calendar-strategy')).toContainText(`最大回撤 ${(calendar.max_drawdown * 100).toFixed(2)}%`);
  await expect(page.getByTestId('comparison-facts')).toContainText('complete-performance@1');
  await expect(page.getByTestId('comparison-facts')).toContainText('2026-01-06 ～ 2026-01-07');
  await expect(page.getByTestId('comparison-facts')).toContainText('10000 CNY');
  await expect(page.getByTestId('comparison-facts')).toContainText('2026-01-07');
  await expect(page.getByTestId('comparison-metrics')).toContainText('0.67%');
  const expectedRows = [
    ['策略（single_etf_timing）', strategyRows[0].session_date, strategyRows[0].cash, strategyRows[0].equity, position(strategyRows[0])],
    ['策略（single_etf_timing）', strategyRows[1].session_date, strategyRows[1].cash, strategyRows[1].equity, position(strategyRows[1])],
    ['基准（single_etf）', benchmarkRows[0].session_date, benchmarkRows[0].cash, benchmarkRows[0].equity, position(benchmarkRows[0])],
    ['基准（single_etf）', benchmarkRows[1].session_date, benchmarkRows[1].cash, benchmarkRows[1].equity, position(benchmarkRows[1])],
  ];
  expect(await resultRows(page, 'comparison-valuations')).toEqual(expectedRows);

  await page.reload();
  await expect(page.getByTestId('comparison-valuations')).toBeVisible();
  expect(await resultRows(page, 'comparison-valuations')).toEqual(expectedRows);
  expect(await calendarRows(page, 'comparison-monthly-strategy')).toEqual(strategyMonthly);
  expect(await calendarRows(page, 'comparison-annual-strategy')).toEqual(strategyAnnual);
  await page.goto('/comparisons');
  await expect(page.getByRole('button', { name: '查看此对照', exact: true }).first()).toBeVisible();
  await page.getByRole('button', { name: '查看此对照', exact: true }).first().click();
  await expect(page.getByTestId('comparison-valuations')).toBeVisible();
  expect(await resultRows(page, 'comparison-valuations')).toEqual(expectedRows);

  const benchmarkTable = join(lake(), 'experiments', hex(result.data.benchmark.exp), 'executions', hex(result.data.benchmark.exe),
    'runs', hex(result.data.benchmark.runs[0].run_id), 'account_valuations.parquet');
  const originalTable = await readFile(benchmarkTable);
  try {
    await writeFile(benchmarkTable, Buffer.from('synthetic corrupt comparison benchmark table'));
    await page.reload();
    await expect(page.getByRole('heading', { name: '持久对照结果不可用', exact: true })).toBeVisible();
    await expect(page.getByText('experiment_result_failed', { exact: true })).toBeVisible();
    await expect(page.getByTestId('comparison-valuations')).toHaveCount(0);
  } finally {
    await writeFile(benchmarkTable, originalTable);
  }
  await page.reload();
  await expect(page.getByTestId('comparison-valuations')).toBeVisible();
  expect(await resultRows(page, 'comparison-valuations')).toEqual(expectedRows);

  const summaryTable = join(lake(), 'experiments', hex(result.data.strategy.exp), 'executions', hex(result.data.strategy.exe),
    'runs', hex(result.data.strategy.runs[0].run_id), 'summary.parquet');
  const originalSummary = await readFile(summaryTable);
  try {
    await writeFile(summaryTable, Buffer.from('synthetic corrupt complete summary report'));
    await page.reload();
    await expect(page.getByRole('heading', { name: '持久对照结果不可用', exact: true })).toBeVisible();
    await expect(page.getByTestId('comparison-complete-metrics-strategy')).toHaveCount(0);
    await expect(page.getByTestId('comparison-monthly-strategy')).toHaveCount(0);
  } finally {
    await writeFile(summaryTable, originalSummary);
  }
  await page.reload();
  await expect(page.getByTestId('comparison-monthly-strategy')).toBeVisible();
  expect(await calendarRows(page, 'comparison-monthly-strategy')).toEqual(strategyMonthly);

  const comparisonFile = join(lake(), 'tasks', taskId!, 'comparison.json');
  const originalComparison = await readFile(comparisonFile, 'utf8');
  const broken = JSON.parse(originalComparison);
  delete broken.benchmark.definition;
  try {
    await writeFile(comparisonFile, JSON.stringify(broken));
    await page.reload();
    await expect(page.getByText('comparison_benchmark_missing', { exact: true })).toBeVisible();
    await expect(page.getByTestId('comparison-valuations')).toHaveCount(0);
  } finally {
    await writeFile(comparisonFile, originalComparison);
  }
  await page.reload();
  await expect(page.getByTestId('comparison-valuations')).toBeVisible();
  expect(errors).toEqual([]);
});

test('equal-weight pool comparison reports incomplete action facts and keeps authoritative curves', async ({ page }) => {
  test.setTimeout(120_000);
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  const registry = (await (await page.request.get('/api/v1/research-pools')).json()).data;
  const pool = registry.pools.find((item: { id: string }) => item.id === 'cross-asset');
  const originalDraft = pool.draft as string[];
  const members = ['510300.XSHG', '510880.XSHG'];
  let draft = originalDraft;
  let version = 0;
  try {
    expect((await page.request.post('/api/v1/research-pools', { data: { op: 'save', id: 'cross-asset', expected: draft, members } })).ok()).toBe(true);
    draft = members;
    const published = await (await page.request.post('/api/v1/research-pools', { data: { op: 'publish', id: 'cross-asset', expected: draft } })).json();
    version = published.data.pools.find((item: { id: string }) => item.id === 'cross-asset').versions.at(-1).version;
    const details = await catalogDetails(page);
    const executable = details.find((item) => item.normalizer_version === 'synthetic-pool-executable@1' && item.bars === 650);
    expect(executable).toBeTruthy();
    const adjusted = executable!.detail.adjusted_series.versions as { instrument_id: string; version: string }[];
    const actions = await originalActions(page);

    await page.goto('/comparisons');
    await selectLabel(page, '对照模式', 'pool');
    await selectLabel(page, 'Dataset 版本', executable!.dsv);
    await selectLabel(page, '研究池', 'cross-asset');
    await selectLabel(page, '固定版本', String(version));
    await expect(page.getByLabel('复权版本 510300.XSHG', { exact: true }).locator('option')).not.toHaveCount(1);
    for (const member of members) {
      const bound = adjusted.find((item) => item.instrument_id === member);
      expect(bound).toBeTruthy();
      await selectLabel(page, `复权版本 ${member}`, bound!.version);
    }
    await selectLabel(page, '公司行动事实版本', actions);
    await page.getByLabel('参数组合 JSON', { exact: true }).fill(JSON.stringify({ list: [{ short: 1, long: 2, vol: 2, w_s: 0.5, w_l: 0.5, w_v: 0.0, trend: null, top_k: 1, rebalance_every: 1 }] }));
    await fillCosts(page, { cash: '10000', commission: '0', slippage: '0', buyTax: '0', sellTax: '0' });
    await page.getByRole('button', { name: '提交策略—基准对照', exact: true }).click();
    await expect(page.getByTestId('comparison-status')).toContainText('已完成', { timeout: 60_000 });
    await expect(page.getByTestId('comparison-verification')).toBeVisible();
    await expect(page.getByTestId('comparison-verification')).toContainText('不完整');
    await expect(page.getByTestId('comparison-valuations')).toBeVisible();
    const taskId = new URL(page.url()).searchParams.get('task');
    const result = await (await page.request.get(`/api/v1/tasks/comparisons/${taskId}/result`)).json();
    expect(result.data.verification.comparable).toBe(false);
    expect(result.data.verification.reasons.some((reason: string) => reason.includes('corporate_actions_incomplete'))).toBe(true);
    expect(result.data.comparability.benchmark_kind).toBe('equal_weight_pool');
    const incompleteCalendar = result.data.strategy.runs[0].summary.complete.calendar;
    expect(incompleteCalendar.available).toBe(false);
    expect(incompleteCalendar.reason).toBe('corporate_actions_incomplete');
    expect(incompleteCalendar.points).toEqual([]);
    expect(incompleteCalendar.monthly).toEqual([]);
    expect([incompleteCalendar.max_drawdown, incompleteCalendar.drawdown_peak_session, incompleteCalendar.drawdown_trough_session]).toEqual([undefined, undefined, undefined]);
    await expect(page.getByTestId('comparison-calendar-strategy')).toContainText('公司行动事实不完整');
    await expect(page.getByTestId('comparison-monthly-strategy')).toHaveCount(0);
    await expect(page.getByTestId('comparison-drawdown')).toContainText('均没有可用的权威回撤序列');
    const strategyRows = closeRows(result.data.strategy.runs[0]);
    const benchmarkRows = closeRows(result.data.benchmark.runs[0]);
    expect(strategyRows.map((row) => row.equity)).not.toEqual(benchmarkRows.map((row) => row.equity));
    const expectedRows = [
      ...[strategyRows[0], strategyRows.at(-1)!].map((row) => ['策略（s2_rotation）', row.session_date, row.cash, row.equity, position(row)]),
      ...[benchmarkRows[0], benchmarkRows.at(-1)!].map((row) => ['基准（equal_weight_pool）', row.session_date, row.cash, row.equity, position(row)]),
    ];
    expect(await resultRows(page, 'comparison-valuations')).toEqual(expectedRows);
    const reloadRows = await resultRows(page, 'comparison-valuations');
    await page.reload();
    await expect(page.getByTestId('comparison-verification')).toBeVisible();
    expect(await resultRows(page, 'comparison-valuations')).toEqual(reloadRows);
    expect(errors).toEqual([]);
  } finally {
    await page.request.post('/api/v1/research-pools', { data: { op: 'save', id: 'cross-asset', expected: draft, members: originalDraft } });
  }
});

test('comparison rejects incompatible price rules and shows real task failures without fabricated results', async ({ page }) => {
  test.setTimeout(120_000);
  const details = await catalogDetails(page);
  const failure = details.find((item) => item.bars === 2 && item.detail.instruments.some((instrument: { instrument_id: string; kind: string }) =>
    instrument.instrument_id === '510300.XSHG' && instrument.kind === 'etf'));
  expect(failure).toBeTruthy();
  const input = {
    schema_version: 1,
    raw_dsv: failure!.dsv,
    instrument_id: '510300.XSHG',
    method: 'forward',
    method_version: 'synthetic-comparison-failure@1',
    source: { source_id: 'synthetic', source_version: '1', source_ref: 'comparison-e2e' },
    observed_at: '2026-10-30T00:00:00Z',
    retrospective_assumption: 'synthetic fixture, not strict PIT',
    close_availability_assumption: 'synthetic close assumed known',
    start: '2026-01-05',
    end: '2026-01-08',
    rows: ['2026-01-05', '2026-01-08'].map((date) => ({ trade_date: date, open: '10', high: '10', low: '10', close: '10' })),
  };
  const file = join(process.env.PRAJNA_E2E_ROOT!, 'comparison-failure-adjusted.json');
  await writeFile(file, JSON.stringify(input));
  const imported = spawnSync(join(target(), 'debug', 'prajna-import-adjusted'), ['--root', lake(), '--input', file], { encoding: 'utf8' });
  expect(imported.status, imported.stderr).toBe(0);
  const adjusted = JSON.parse(imported.stdout).version;
  const actions = await originalActions(page);

  await page.goto('/comparisons');
  await selectLabel(page, 'Dataset 版本', failure!.dsv);
  await expect(page.getByLabel('ETF', { exact: true }).locator('option')).toHaveCount(2);
  await selectLabel(page, 'ETF', '510300.XSHG');
  await selectLabel(page, '复权版本 510300.XSHG', adjusted);
  await selectLabel(page, '公司行动事实版本', actions);
  await page.getByLabel('均线窗口', { exact: true }).fill('1');
  await page.getByLabel('调仓频率', { exact: true }).fill('1');
  await fillCosts(page);
  const rejected = page.waitForResponse((response) => response.url().endsWith('/api/v1/tasks/comparisons') && response.request().method() === 'POST');
  await page.getByRole('button', { name: '提交策略—基准对照', exact: true }).click();
  const rejection = await rejected;
  expect(rejection.status()).toBe(422);
  expect(rejection.headers()['cache-control']).toBe('no-store');
  await expect(page.getByText('comparison_price_rules_mismatch', { exact: true })).toBeVisible();
  await expect(page.getByTestId('comparison-status')).toContainText('尚未提交或选择对照任务');
  await fillCosts(page, { slippage: '0' });
  await page.getByRole('button', { name: '提交策略—基准对照', exact: true }).click();
  await expect(page.getByTestId('comparison-status')).toContainText('失败', { timeout: 60_000 });
  await expect(page.getByTestId('comparison-status')).toContainText('experiment_cli_failed');
  await expect(page.getByTestId('comparison-valuations')).toHaveCount(0);
  await expect(page.getByTestId('comparison-metrics')).toHaveCount(0);
});
