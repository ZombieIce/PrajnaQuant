import { test, expect } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

test('evaluation production submit persists Rust endpoints and relationship chart samples', async ({ page }) => {
  const lake = join(process.env.PRAJNA_E2E_ROOT!, 'fixture', 'lake');
  const request = JSON.parse(await readFile(join(lake, 'synthetic-momentum-request.json'), 'utf8'));
  const errors: string[] = []; page.on('pageerror', e => errors.push(e.message));
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
  await page.goto('/research/momentum');
  await page.getByLabel('研究 Dataset', {exact:true}).selectOption(request.dsv);
  await page.getByLabel('研究对象', {exact:true}).selectOption(request.instrument);
  await page.getByLabel(`${request.instrument} 研究复权版本`, {exact:true}).selectOption(request.adjusted[request.instrument]);
  await page.getByLabel('研究窗口', {exact:true}).fill('1');
  await page.getByLabel('评价后续收益', {exact:true}).check();
  const response = page.waitForResponse(r => r.url().endsWith('/momentum-research') && r.request().method() === 'POST');
  await page.getByRole('button', {name:'计算并保存研究', exact:true}).click();
  const submitted = (await (await response).json()).task;
  await expect(page.getByTestId('momentum-task-status')).toContainText('succeeded', {timeout:60_000});
  const task = await (await page.request.get(`/api/v1/tasks/experiments/${submitted.task_id}`)).json();
  const result = (await (await page.request.post('/api/v1/momentum-research', {data:{op:'read',id:task.result.report_id}})).json()).data;
  expect(result).toEqual(JSON.parse(await readFile(join(lake,'momentum-research',`${result.id}.json`),'utf8')));
  expect(result.report.request.evaluation).toBe('next_open_to_forward_open_v1');
  await expect(page.getByTestId('evaluation-contract')).toContainText('next_open_to_forward_open@1');
  const labels = result.report.evaluation.labels;
  await expect(page.getByTestId('evaluation-labels').locator('tbody tr')).toHaveCount(390);
  expect(labels[0].value).toBeCloseTo(12/11-1,12);
  expect(labels[1].value).toBeCloseTo(16/11-1,12);
  expect(labels[2].value).toBeCloseTo(31/11-1,12);
  for (const index of [0,1,2,389]) {
    const cells = page.getByTestId('evaluation-labels').locator('tbody tr').nth(index).locator('td');
    await expect(cells.nth(3)).toHaveText(labels[index].entry_date ?? '—');
    await expect(cells.nth(4)).toHaveText(labels[index].exit_date ?? '—');
    await expect(cells.nth(5)).toHaveText(labels[index].value === null ? '—' : String(labels[index].value));
    await expect(cells.nth(6)).toHaveText(labels[index].status);
  }
  for (const [i,relation] of result.report.factors[0].relationships.entries()) {
    const rows = page.getByTestId('relationship-values').nth(i).locator('tbody tr');
    await expect(rows).toHaveCount(relation.effective_count);
    await expect(page.getByTestId('relationship-counts').nth(i)).toContainText(`有效样本 ${relation.effective_count} / 130`);
    expect(relation.effective_count + relation.missing_count).toBe(130);
    const p = relation.points[0];
    await expect(rows.first().locator('td').nth(2)).toHaveText(String(p.factor_value));
    await expect(rows.first().locator('td').nth(3)).toHaveText(String(p.forward_return));
  }
  await expect(page.locator('canvas')).toHaveCount(5);
  // Canvas existence and tables alone cannot prove the scatter renderer was registered.
  await expect.poll(() => page.locator('canvas').nth(2).evaluate(canvas => {
    if (!(canvas instanceof HTMLCanvasElement)) throw new Error('expected canvas');
    const image = canvas.getContext('2d')!.getImageData(0, 0, canvas.width, canvas.height).data;
    let blue = 0;
    for (let i = 0; i < image.length; i += 4) {
      if (image[i] < 120 && image[i+1] < 160 && image[i+2] > 140 && image[i+3] > 128) blue++;
    }
    return blue;
  })).toBeGreaterThan(100);
  await page.getByRole('heading', {name:'因子与 1 Session 后续收益关系',exact:true}).scrollIntoViewIfNeeded();
  await page.screenshot({path:test.info().outputPath('evaluation-relationship.png')});
  await page.reload();
  await page.getByRole('button',{name:`打开 ${result.id}`,exact:true}).click();
  await expect(page.getByTestId('relationship-values')).toHaveCount(3);
  expect(errors).toEqual([]);
});
