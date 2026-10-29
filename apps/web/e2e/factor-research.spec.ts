import { test, expect } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

// #299: the same production factor-research flow must select Volatility v1 and
// the new strict `close > MA` definition, persist the Rust report and plot it.
test('factor research selects volatility and strict MA trend, saves and plots persisted Rust reports', async ({ page }) => {
  const lake = join(process.env.PRAJNA_E2E_ROOT!, 'fixture', 'lake');
  const request = JSON.parse(await readFile(join(lake, 'synthetic-momentum-request.json'), 'utf8'));
  const errors: string[] = [];
  page.on('pageerror', e => errors.push(e.message));
  const submitOnPage = async () => {
    const response = page.waitForResponse(r => r.url().endsWith('/momentum-research') && r.request().method() === 'POST');
    await page.getByRole('button', { name: '计算并保存研究', exact: true }).click();
    return (await (await response).json()).task as { task_id: string };
  };
  const readTask = async (taskId: string) => (await (await page.request.get(`/api/v1/tasks/experiments/${taskId}`)).json());
  const readReport = async (id: string) => (await (await page.request.post('/api/v1/momentum-research', { data: { op: 'read', id } })).json()).data;
  const expectTableMatches = async (factorIndex: number, factor: any, indexes: number[]) => {
    const rows = page.getByTestId('momentum-values').nth(factorIndex).locator('tbody tr');
    await expect(rows).toHaveCount(130);
    for (const index of indexes) {
      const value = factor.rows[index].value;
      await expect(rows.nth(index)).toContainText(factor.rows[index].date);
      await expect(rows.nth(index).locator('td').nth(2)).toHaveText(value === null ? '—' : String(value));
      await expect(rows.nth(index)).toContainText(factor.rows[index].status);
    }
    await expect(page.getByTestId('missing-counts').nth(factorIndex)).toContainText(JSON.stringify(factor.status_counts));
  };

  await page.goto('/research/momentum');
  await page.getByLabel('研究 Dataset', { exact: true }).selectOption(request.dsv);
  await page.getByLabel('研究对象', { exact: true }).selectOption(request.instrument);
  await page.getByLabel(`${request.instrument} 研究复权版本`, { exact: true }).selectOption(request.adjusted[request.instrument]);

  // Volatility reuses Volatility v1 and the page's default 20/60 windows.
  await page.getByLabel('研究因子', { exact: true }).selectOption('volatility');
  await expect(page.getByLabel('研究窗口', { exact: true })).toHaveValue('20,60');
  const volatilityTask = await submitOnPage();
  await expect(page.getByTestId('momentum-task-status')).toContainText('succeeded', { timeout: 60_000 });
  const volatilityStatus = await readTask(volatilityTask.task_id);
  expect(volatilityStatus.task_type).toBe('prajna_momentum');
  const volatility = await readReport(volatilityStatus.result.report_id);
  expect(volatility.report.semantics).toBe('factor-research@1');
  expect(volatility.report.direction).toBe('lower_is_stronger');
  expect(volatility.report.request.factor).toBe('volatility');
  expect(volatility.report.request.windows).toEqual([20, 60]);
  expect(volatility.report.factors.map((factor: any) => factor.definition)).toEqual([
    { kind: 'volatility', version: '1', params: { n: 20 } },
    { kind: 'volatility', version: '1', params: { n: 60 } },
  ]);
  const volatilityBytes = await readFile(join(lake, 'momentum-research', `${volatility.id}.json`), 'utf8');
  expect(JSON.parse(volatilityBytes)).toEqual(volatility);
  await expect(page.getByTestId('factor-semantics')).toContainText('lower_is_stronger');
  await expect(page.getByTestId('momentum-values')).toHaveCount(2);
  await expect(page.locator('canvas')).toHaveCount(4);
  for (const [index, factor] of volatility.report.factors.entries()) {
    await expectTableMatches(index, factor, [0, 20, 129]);
  }

  // Strict MA trend: window 1 is always equal to its own mean, so every valid
  // value is 0; window 60 on the fixture's increasing adjusted closes is 1.
  await page.getByLabel('研究因子', { exact: true }).selectOption('trend_filter_strict');
  await expect(page.getByLabel('研究窗口', { exact: true })).toHaveValue('20,60');
  await page.getByLabel('研究窗口', { exact: true }).fill('1,60');
  const strictTask = await submitOnPage();
  await expect(page.getByTestId('momentum-task-status')).toContainText('succeeded', { timeout: 60_000 });
  const strictStatus = await readTask(strictTask.task_id);
  const strict = await readReport(strictStatus.result.report_id);
  expect(strict.report.semantics).toBe('factor-research@1');
  expect(strict.report.direction).toBe('binary_close_above_ma');
  expect(strict.report.request.factor).toBe('trend_filter_strict');
  expect(strict.report.request.windows).toEqual([1, 60]);
  expect(strict.report.factors.map((factor: any) => factor.definition)).toEqual([
    { kind: 'trend_filter_strict', version: '1', params: { n: 1 } },
    { kind: 'trend_filter_strict', version: '1', params: { n: 60 } },
  ]);
  expect(strict.report.factors[0].rows.every((row: any) => row.value === 0)).toBe(true);
  expect(strict.report.factors[1].rows[58].status).toBe('insufficient_window');
  expect(strict.report.factors[1].rows[59].value).toBe(1);
  expect(strict.report.factors[1].rows[129].value).toBe(1);
  await expect(page.getByTestId('factor-semantics')).toContainText('binary_close_above_ma');
  await expect(page.getByTestId('momentum-values')).toHaveCount(2);
  await expect(page.locator('canvas')).toHaveCount(4);
  await expectTableMatches(0, strict.report.factors[0], [0, 59, 129]);
  await expectTableMatches(1, strict.report.factors[1], [0, 59, 129]);

  // Volatility v1 requires n >= 2; the page surfaces the persisted task failure.
  await page.getByLabel('研究因子', { exact: true }).selectOption('volatility');
  await page.getByLabel('研究窗口', { exact: true }).fill('1');
  const invalidTask = await submitOnPage();
  await expect(page.getByTestId('momentum-task-status')).toContainText('failed', { timeout: 60_000 });
  const invalidStatus = await readTask(invalidTask.task_id);
  expect(invalidStatus.error.code).toBe('invalid_momentum');
  expect(invalidStatus.result).toBeNull();
  await expect(page.getByRole('alert')).toBeVisible();
  await expect(page.getByTestId('momentum-values')).toHaveCount(0);

  // Both saved reports can be reopened after a full page reload.
  await page.reload();
  await page.getByRole('button', { name: `打开 ${volatility.id}`, exact: true }).click();
  await expect(page.getByTestId('momentum-values')).toHaveCount(2);
  await expect(page.getByTestId('factor-semantics')).toContainText('lower_is_stronger');
  await page.getByRole('button', { name: `打开 ${strict.id}`, exact: true }).click();
  await expect(page.getByTestId('momentum-values')).toHaveCount(2);
  await expect(page.getByTestId('factor-semantics')).toContainText('binary_close_above_ma');
  expect(errors).toEqual([]);
});
