import { test, expect } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

test('cross-section IC series, summary and sample reasons persist and render through the production page', async ({ page }) => {
  const lake = join(process.env.PRAJNA_E2E_ROOT!, 'fixture', 'lake');
  const request = JSON.parse(await readFile(join(lake, 'synthetic-ic-request.json'), 'utf8'));
  const registry = (await (await page.request.get('/api/v1/research-pools')).json()).data;
  const pool = registry.pools.find((item: { id: string }) => item.id === 'cross-asset');
  const originalDraft = pool.draft as string[];
  const members = Object.keys(request.adjusted).sort();
  let draft = originalDraft;
  try {
    const saved = await page.request.post('/api/v1/research-pools', {
      data: { op: 'save', id: 'cross-asset', expected: draft, members },
    });
    expect(saved.ok()).toBe(true);
    draft = members;
    const published = await page.request.post('/api/v1/research-pools', {
      data: { op: 'publish', id: 'cross-asset', expected: draft },
    });
    expect(published.ok()).toBe(true);
    const version = (await published.json()).data.pools
      .find((item: { id: string }) => item.id === 'cross-asset').versions.at(-1) as { version: number; members: string[] };
    expect([...version.members].sort()).toEqual(members);
    await page.goto('/research/momentum');
    await page.getByLabel('研究 Dataset', { exact: true }).selectOption(request.dsv);
    await page.getByLabel('研究对象', { exact: true }).selectOption(`pool:cross-asset:${version.version}`);
    for (const member of members) {
      await page.getByLabel(`${member} 研究复权版本`, { exact: true }).selectOption(request.adjusted[member]);
    }
    await page.getByLabel('研究窗口', { exact: true }).fill('1');
    await page.getByLabel('评价后续收益', { exact: true }).check();
    const response = page.waitForResponse(value => value.url().endsWith('/momentum-research') && value.request().method() === 'POST');
    await page.getByRole('button', { name: '计算并保存研究', exact: true }).click();
    const submitted = (await (await response).json()).task;
    await expect(page.getByTestId('momentum-task-status')).toContainText('succeeded', { timeout: 60_000 });
    const task = await (await page.request.get(`/api/v1/tasks/experiments/${submitted.task_id}`)).json();
    const result = (await (await page.request.post('/api/v1/momentum-research', { data: { op: 'read', id: task.result.report_id } })).json()).data;
    expect(result).toEqual(JSON.parse(await readFile(join(lake, 'momentum-research', `${result.id}.json`), 'utf8')));
    expect(result.report.semantics).toBe('factor-evaluation@2');
    expect(result.report.evaluation.statistics_version).toBe('cross_section_ic@1');
    const statistics = result.report.factors[0].statistics;
    expect(statistics).toHaveLength(3);
    const first = statistics[0];
    expect(first.horizon).toBe(1);
    expect(first.minimum_cross_section).toBe(3);
    expect(first.summary.evaluated_cross_sections).toBeGreaterThan(0);
    expect(first.series.filter((row: { status: string }) => row.status === 'evaluated')).toHaveLength(first.summary.evaluated_cross_sections);
    expect(first.series[0].pearson_ic).toBeNull();
    expect(first.series[0].status).toBe('insufficient_cross_section');
    for (const row of first.series) {
      if (row.status === 'evaluated') {
        expect(row.pearson_ic).not.toBeNull();
        expect(row.rank_ic).not.toBeNull();
      } else {
        expect(row.pearson_ic).toBeNull();
        expect(row.rank_ic).toBeNull();
      }
    }

    await expect(page.getByTestId('ic-contract').first()).toContainText('cross_section_ic@1');
    await expect(page.getByTestId('ic-sample').first()).toContainText(`可计算截面 ${first.summary.evaluated_cross_sections} / ${first.expected_cross_sections}`);
    const rows = page.getByTestId('ic-series').first().locator('tbody tr');
    await expect(rows).toHaveCount(130);
    for (const index of [0, 1, 129]) {
      const expected = first.series[index];
      const cells = rows.nth(index).locator('td');
      await expect(cells.nth(0)).toHaveText(expected.date);
      await expect(cells.nth(1)).toHaveText(`${expected.paired_count} / ${expected.expected_count}`);
      await expect(cells.nth(2)).toHaveText(expected.pearson_ic === null ? '—' : String(expected.pearson_ic));
      await expect(cells.nth(3)).toHaveText(expected.rank_ic === null ? '—' : String(expected.rank_ic));
      await expect(cells.nth(4)).toHaveText(expected.status);
    }
    const summary = page.getByTestId('ic-summary').first().locator('tbody tr');
    await expect(summary).toHaveCount(5);
    const summaryValues: [number, number | null][] = [
      [0, first.summary.pearson_ic_mean],
      [1, first.summary.rank_ic_mean],
      [2, first.summary.rank_ic_stddev],
      [3, first.summary.rank_ic_ir],
      [4, first.summary.rank_ic_positive_rate],
    ];
    for (const [rowIndex, value] of summaryValues) {
      await expect(summary.nth(rowIndex).locator('td').nth(1)).toHaveText(value === null ? '—' : String(value));
    }
    await expect(page.locator('canvas')).toHaveCount(8);
    await expect.poll(() => page.locator('canvas').nth(2).evaluate(canvas => (canvas as HTMLCanvasElement).toDataURL().length)).toBeGreaterThan(1000);
    await page.reload();
    await page.getByRole('button', { name: `打开 ${result.id}`, exact: true }).click();
    await expect(page.getByTestId('ic-series')).toHaveCount(3);
  } finally {
    const current = (await (await page.request.get('/api/v1/research-pools')).json()).data.pools
      .find((item: { id: string }) => item.id === 'cross-asset');
    if (JSON.stringify(current.draft) !== JSON.stringify(originalDraft)) {
      const restored = await page.request.post('/api/v1/research-pools', {
        data: { op: 'save', id: 'cross-asset', expected: current.draft, members: originalDraft },
      });
      expect(restored.ok()).toBe(true);
    }
  }
});
