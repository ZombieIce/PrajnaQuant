import { test, expect } from '@playwright/test';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

test('production page pins adjusted revisions, gaps, raw role, empty and corrupt versions', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', e => errors.push(e.message));
  await page.goto('/datasets');
  const datasets = page.getByLabel('Dataset 版本', { exact: true });
  await expect(datasets.locator('option')).toHaveCount(7);
  const candidates = await datasets.locator('option').evaluateAll(options => options
    .filter(o => (o.textContent ?? '').includes('1 证券 / 2 Bars')).map(o => o.getAttribute('value')!));
  let dsv = '';
  for (const candidate of candidates) {
    await datasets.selectOption(candidate);
    const securities = page.getByLabel('证券', { exact: true });
    await expect(securities.locator('option')).toHaveCount(2);
    await securities.selectOption({ index: 1 });
    if ((await securities.inputValue()) === '510300.XSHG') { dsv = candidate; break; }
  }
  expect(dsv).not.toBe('');
  const prices = page.getByLabel('价格序列', { exact: true });
  await expect(prices.locator('option')).toHaveCount(4);
  await expect(page.getByRole('heading', { name: '原始未复权行情', exact: true })).toBeVisible();
  await expect(page.getByRole('cell', { name: '11.123', exact: true })).toHaveCount(2);
  const version = (label: string) => prices.locator('option').evaluateAll((options, label) => options
    .find(o => (o.textContent ?? '').includes(label))!.getAttribute('value')!, label);
  const original = await version('synthetic-half@1');
  const revised = await version('synthetic-revision@2');
  const empty = await version('synthetic-empty@1');
  const response = page.waitForResponse(r => r.url().includes('adjusted_version=') && r.ok());
  await prices.selectOption(original);
  const payload = await (await response).json();
  expect(payload.data.adjusted.version).toBe(original);
  expect(payload.data.dsv).toBe(dsv);
  expect(payload.data.items[0].close).toBe('5.5615');
  await expect(page.getByRole('heading', { name: '复权研究价格', exact: true })).toBeVisible();
  await expect(page.locator('tbody tr').first()).toContainText('5.5115');
  const metadata = page.getByTestId('adjusted-metadata');
  await expect(metadata).toContainText('synthetic-supplier');
  await expect(metadata).toContainText('2026-01-06、2026-01-07、2026-01-09');
  await expect(metadata).toContainText('有原始 Bar 但供应商未提供：0');
  await expect(metadata).toContainText('无原始 Bar：3');
  await expect(metadata).toContainText('不是严格 PIT');
  await expect(metadata).toContainText('不推导分红现金流');
  await prices.selectOption(revised);
  await expect(page.getByRole('cell', { name: '5.6', exact: true })).toBeVisible();
  await prices.selectOption(original);
  await expect(page.locator('tbody tr').first()).toContainText('5.5115');
  await expect(page.getByRole('cell', { name: '5.6', exact: true })).toHaveCount(0);
  await prices.selectOption(empty);
  await expect(metadata).toContainText('实际 0 根');
  await expect(metadata).toContainText('缺失 Sessions：5');
  await expect(metadata).toContainText('有原始 Bar 但供应商未提供：2');
  await expect(metadata).toContainText('无原始 Bar：3');
  await expect(page.getByRole('status')).toContainText('该范围没有行情');
  // Real on-disk corruption after catalog selection: API failure remains visible.
  const root = process.env.PRAJNA_E2E_ROOT;
  if (!root) throw new Error('Use npm run test:e2e');
  const object = join(root, 'fixture/lake/adjusted-series', original.replace('adj:sha256:', '') + '.json');
  const saved = await readFile(object);
  try {
    await writeFile(object, '{}');
    await prices.selectOption(original);
    await expect(page.getByRole('alert').filter({ hasText: 'invalid or corrupt adjusted version' })).toBeVisible();
    await expect(page.locator('tbody tr')).toHaveCount(0);
  } finally { await writeFile(object, saved); }
  await prices.selectOption('');
  await expect(page.getByRole('cell', { name: '11.123', exact: true })).toHaveCount(2);
  expect(errors).toEqual([]);
});
