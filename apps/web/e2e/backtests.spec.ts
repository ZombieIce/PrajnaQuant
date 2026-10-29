import { test, expect, type Page } from '@playwright/test';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

const lake = () => {
  if (!process.env.PRAJNA_E2E_ROOT) throw new Error('Run through npm run test:e2e');
  return join(process.env.PRAJNA_E2E_ROOT, 'fixture', 'lake');
};

async function chooseDataset(page: Page, bars: number) {
  const versions = page.getByLabel('Dataset 版本', { exact: true });
  await expect(versions.locator('option')).toHaveCount(6);
  const candidates = await versions.locator('option').evaluateAll((options, bars) => options
    .filter((option) => (option.textContent ?? '').includes(`1 证券 / ${bars} Bars`))
    .map((option) => option.getAttribute('value') ?? ''), bars);
  expect(candidates.length).toBeGreaterThan(0);
  let chosen = '';
  for (const dsv of candidates) {
    const response = await page.request.get(`/api/v1/datasets/${encodeURIComponent(dsv)}`);
    expect(response.status()).toBe(200);
    const detail = await response.json();
    const instruments = detail.data.instruments as { instrument_id: string; kind: string }[];
    if (instruments.some((item) => item.instrument_id === '510300.XSHG' && item.kind === 'etf')) { chosen = dsv; break; }
  }
  expect(chosen).not.toBe('');
  await versions.selectOption(chosen);
  const etf = page.getByLabel('ETF', { exact: true });
  await expect(etf.locator('option')).toHaveCount(2);
  await etf.selectOption('510300.XSHG');
}

async function valuationRows(page: Page) {
  return page.getByTestId('valuation-rows').locator('tbody tr').evaluateAll((rows) => rows
    .map((row) => Array.from((row as HTMLTableRowElement).cells).map((cell) => cell.textContent ?? '')));
}

const hex = (identity: string) => {
  const [, value] = identity.split(':sha256:');
  if (!value) throw new Error(`Unexpected identity ${identity}`);
  return value;
};

test('production selection submits, polls, reads the persisted result, and re-reads it', async ({ page }) => {
  test.setTimeout(120_000);
  const errors: string[] = [];
  const resultReads: number[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  page.on('response', (response) => { if (new URL(response.url()).pathname.endsWith('/result')) resultReads.push(response.status()); });
  await page.goto('/backtests');
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('单 ETF 回测');
  await expect(page.getByLabel('Dataset 版本', { exact: true }).locator('option')).toHaveCount(6);
  await chooseDataset(page, 3);
  await page.getByRole('button', { name: '提交买入持有回测', exact: true }).click();
  await expect(page.getByTestId('task-status')).toContainText('已完成', { timeout: 60_000 });
  await expect(page.getByTestId('valuation-rows')).toBeVisible();

  const taskId = new URL(page.url()).searchParams.get('task');
  expect(taskId).toBeTruthy();
  const taskResponse = await page.request.get(`/api/v1/tasks/experiments/${taskId}`);
  expect(taskResponse.status()).toBe(200);
  const task = await taskResponse.json();
  expect(task.status).toBe('succeeded');
  const resultResponse = await page.request.get(`/api/v1/tasks/experiments/${taskId}/result`);
  expect(resultResponse.status()).toBe(200);
  const result = await resultResponse.json();
  expect(result.schema_version).toBe(1);
  const run = result.data.runs[0];
  expect(run.status).toBe('completed');
  expect(run.result_level).toBe('standard');
  expect(run.accounting_verified).toBe(true);
  const valuations = [...run.series.account_valuations].sort((left, right) =>
    left.session_date.localeCompare(right.session_date) || left.valuation_point.localeCompare(right.valuation_point));
  expect(valuations).toHaveLength(6);
  const lastClose = valuations.find((row) => row.session_date === '2026-01-07' && row.valuation_point === 'close');
  if (!lastClose) throw new Error('missing 2026-01-07 close valuation');
  if (!run.summary) throw new Error('missing persisted summary');
  expect(lastClose.equity).toBe('1009490.44006');
  expect(lastClose.cash).toBe('500.44006');
  const lastPositions = lastClose.positions as Record<string, { quantity: string }>;
  expect(Object.keys(lastPositions)).toEqual(['510300.XSHG']);
  expect(lastPositions['510300.XSHG'].quantity).toBe('99900');
  const expectedRows = valuations.map((row) => {
    const positions = (row.positions ?? {}) as Record<string, { quantity: string }>;
    return [row.session_date, row.valuation_point, row.cash, row.equity,
      Object.values(positions).map((position) => position.quantity).join(' / ') || '无持仓'];
  });
  expect(await valuationRows(page)).toEqual(expectedRows);
  expect(run.summary.total_return).toBeCloseTo(0.00949044006, 10);
  await expect(page.getByTestId('result-metrics')).toContainText('0.95%');
  const legacy = await (await page.request.get('/api/v1/runs')).json();
  expect(legacy.items).toEqual([]);

  const readsBeforeReload = resultReads.length;
  await page.reload();
  await expect(page.getByTestId('valuation-rows')).toBeVisible();
  expect(await valuationRows(page)).toEqual(expectedRows);
  expect(resultReads.length).toBeGreaterThan(readsBeforeReload);

  await page.goto('/backtests');
  await expect(page.getByRole('button', { name: '查看此任务', exact: true }).first()).toBeVisible();
  await page.getByRole('button', { name: '查看此任务', exact: true }).first().click();
  await expect(page.getByTestId('valuation-rows')).toBeVisible();
  expect(await valuationRows(page)).toEqual(expectedRows);

  const table = join(lake(), 'experiments', hex(result.data.exp), 'executions', hex(result.data.exe), 'runs', hex(run.run_id), 'account_valuations.parquet');
  const original = await readFile(table);
  try {
    await writeFile(table, Buffer.from('synthetic corrupt NAV table'));
    await page.reload();
    await expect(page.getByRole('heading', { name: '持久结果不可用', exact: true })).toBeVisible();
    await expect(page.getByText('experiment_result_failed', { exact: true })).toBeVisible();
    await expect(page.getByTestId('valuation-rows')).toHaveCount(0);
  } finally {
    await writeFile(table, original);
  }
  await page.reload();
  await expect(page.getByTestId('valuation-rows')).toBeVisible();
  expect(await valuationRows(page)).toEqual(expectedRows);
  expect(errors).toEqual([]);
});

test('task failure is visible with its persisted error code and no result values', async ({ page }) => {
  test.setTimeout(120_000);
  await page.goto('/backtests');
  await chooseDataset(page, 2);
  await page.getByRole('button', { name: '提交买入持有回测', exact: true }).click();
  await expect(page.getByTestId('task-status')).toContainText('失败', { timeout: 60_000 });
  const taskId = new URL(page.url()).searchParams.get('task');
  expect(taskId).toBeTruthy();
  const task = await (await page.request.get(`/api/v1/tasks/experiments/${taskId}`)).json();
  expect(task.status).toBe('failed');
  expect(task.error.code).toBe('experiment_cli_failed');
  const details = task.error.details as { stderr?: string; cli_error?: { details?: { stderr?: string } } };
  expect(details.stderr ?? details.cli_error?.details?.stderr ?? '').toContain('execution status');
  await expect(page.getByTestId('task-status')).toContainText(task.error.code);
  await expect(page.getByTestId('task-status')).toContainText('未产生可读取的持久结果');
  await expect(page.getByTestId('valuation-rows')).toHaveCount(0);
  await expect(page.getByTestId('result-metrics')).toHaveCount(0);
});

test('legacy /api/v1/runs route and diagnostic page remain compatible', async ({ page }) => {
  const legacy = await page.request.get('/api/v1/runs');
  expect(legacy.status()).toBe(200);
  expect(await legacy.json()).toEqual({ items: [] });
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'ETF 回测作业', exact: true })).toBeVisible();
  await expect(page.getByText('暂无提交作业。', { exact: true })).toBeVisible();
  const tasks = await (await page.request.get('/api/v1/tasks/experiments')).json();
  expect(Array.isArray(tasks.items)).toBe(true);
  expect((await (await page.request.get('/api/v1/runs')).json()).items).toEqual([]);
});
