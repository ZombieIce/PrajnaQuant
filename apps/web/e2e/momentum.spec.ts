import { test, expect } from '@playwright/test';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
test('momentum production page persists Rust values, charts, provenance and missing states', async ({ page }) => {
  const lake = join(process.env.PRAJNA_E2E_ROOT!, 'fixture', 'lake');
  const request = JSON.parse(await readFile(join(lake, 'synthetic-momentum-request.json'), 'utf8'));
  const errors: string[] = []; page.on('pageerror', e => errors.push(e.message));
  await page.goto('/research/momentum');
  await page.getByLabel('研究 Dataset', { exact: true }).selectOption(request.dsv);
  await page.getByLabel('研究对象', { exact: true }).selectOption(request.instrument);
  await page.getByLabel(`${request.instrument} 研究复权版本`, { exact: true }).selectOption(request.adjusted[request.instrument]);
  const response = page.waitForResponse(r => r.url().endsWith('/momentum-research') && r.request().method() === 'POST');
  await page.getByRole('button', { name: '计算并保存研究', exact: true }).click();
  const submitted = (await (await response).json()).task;
  await expect(page.getByTestId('momentum-task-status')).toContainText('succeeded', { timeout: 60_000 });
  const task = await (await page.request.get(`/api/v1/tasks/experiments/${submitted.task_id}`)).json();
  expect(task.task_type).toBe('prajna_momentum');
  const result = (await (await page.request.post('/api/v1/momentum-research', {data:{op:'read', id:task.result.report_id}})).json()).data;
  const file = join(lake, 'momentum-research', `${result.id}.json`);
  const persistedBytes = await readFile(file, 'utf8');
  const persisted = JSON.parse(persistedBytes);
  expect(result).toEqual(persisted);
  expect(persisted.report.execution.rustc_version).toContain('rustc');
  expect(persisted.report.execution.git_revision).toBeTruthy();
  expect(persisted.report.request.windows).toEqual([20, 60, 120]);
  await expect(page.getByTestId('momentum-values')).toHaveCount(3);
  for (const [i, factor] of persisted.report.factors.entries()) {
    const rows = page.getByTestId('momentum-values').nth(i).locator('tbody tr');
    await expect(rows).toHaveCount(130);
    for (const index of [0, 120, 129]) {
      await expect(rows.nth(index)).toContainText(factor.rows[index].date);
      await expect(rows.nth(index).locator('td').nth(2)).toHaveText(factor.rows[index].value === null ? '—' : String(factor.rows[index].value));
      await expect(rows.nth(index)).toContainText(factor.rows[index].status);
    }
    await expect(page.getByTestId('missing-counts').nth(i)).toContainText(JSON.stringify(factor.status_counts));
  }
  await expect(page.locator('canvas')).toHaveCount(6);
  await page.getByText('固定输入、来源与可用性假设', { exact: true }).click();
  await expect(page.locator('pre')).toContainText('historical_publication_time');
  await expect(page.locator('pre')).toContainText('Unknown');
  await page.reload();
  await page.getByRole('button', { name: `打开 ${result.id}`, exact: true }).click();
  await expect(page.getByTestId('momentum-values')).toHaveCount(3);
  try {
    await writeFile(file, '{}');
    const failed = page.waitForResponse(r => r.url().endsWith('/momentum-research') && r.status() === 500);
    await page.getByRole('button', { name: `打开 ${result.id}`, exact: true }).click();
    expect((await (await failed).json()).error.code).toBe('momentum_store_failed');
    await expect(page.getByTestId('momentum-values')).toHaveCount(0);
    await expect(page.getByRole('alert')).toContainText('corrupt momentum report');
  } finally { await writeFile(file, persistedBytes); }
  const bad = { op: 'submit', idempotency_key: 'synthetic-invalid-window', request: { ...request, windows: [0] } };
  const failed = (await (await page.request.post('/api/v1/momentum-research', {data:bad})).json()).task;
  const duplicate = await (await page.request.post('/api/v1/momentum-research', {data:bad})).json();
  expect(duplicate.created).toBe(false); expect(duplicate.task.task_id).toBe(failed.task_id);
  expect((await page.request.post('/api/v1/momentum-research', {data:{...bad, request:{...request, windows:[1]}}})).status()).toBe(409);
  await expect.poll(async () => (await (await page.request.get(`/api/v1/tasks/experiments/${failed.task_id}`)).json()).status).toBe('failed');
  const failedTask = JSON.parse(await readFile(join(lake, 'tasks', failed.task_id, 'task.json'), 'utf8'));
  expect(failedTask.error.code).toBe('invalid_momentum'); expect(failedTask.result).toBeNull();
  const frozen = JSON.parse(await readFile(join(lake, 'tasks', submitted.task_id, 'request.json'), 'utf8'));
  expect(frozen.definition.request.adjusted).toEqual(request.adjusted);
  await page.reload();
  await page.getByRole('button', {name:`任务 ${failed.task_id}`, exact:true}).click();
  await expect(page.getByTestId('momentum-task-status')).toContainText('failed');
  await expect(page.getByRole('alert')).toBeVisible();
  await expect(page.getByTestId('momentum-values')).toHaveCount(0);
  expect(errors).toEqual([]);
});
