import { test, expect } from '@playwright/test';
import { readFile, writeFile, rename } from 'node:fs/promises';
import { join } from 'node:path';
const lake = () => {
  if (!process.env.PRAJNA_E2E_ROOT) throw new Error('Run through npm run test:e2e');
  return join(process.env.PRAJNA_E2E_ROOT, 'fixture', 'lake');
};

test('production Dataset page reads synthetic ETF and stock, lifecycle, gaps, Bars and refresh', async ({ page }) => {
  const errors: string[] = [], requests: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('request', request => { if (request.url().includes('/api/')) requests.push(request.url()); });
  await page.goto('/datasets');
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('研究数据预览');
  const versions = page.getByLabel('Dataset 版本', { exact: true });
  // The shared synthetic lake also holds two #350 @2 selection Datasets; this
  // spec only exercises the v1 preview fixtures (2 Bars each). The momentum
  // fixture adds a separate 130-Session Dataset.
  await expect(versions.locator('option')).toHaveCount(7);
  const values = await versions.locator('option').evaluateAll(options => options
    .filter(o => (o.textContent ?? '').includes('1 证券 / 2 Bars'))
    .map(o => o.getAttribute('value') ?? ''));
  expect(values).toHaveLength(2);
  const seen = new Set();
  for (const dsv of values) {
    await versions.selectOption(dsv);
    const securities = page.getByLabel('证券', { exact: true });
    await expect(securities.locator('option')).toHaveCount(2);
    await securities.selectOption({ index: 1 });
    const id = await securities.inputValue(); seen.add(id);
    await expect(page.getByRole('cell', { name: '2026-01-05', exact: true })).toBeVisible();
    await expect(page.locator('tbody tr')).toHaveCount(2);
    await expect(page.locator('main')).toContainText('实际 Bars：2');
    await expect(page.locator('main')).toContainText('2026-01-06、2026-01-07');
    await expect(page.getByRole('heading', { name: '原始未复权行情', exact: true })).toBeVisible();
    await expect(page.locator('main')).toContainText(id === '510300.XSHG' ? '上市日：2020-01-01' : '上市日：unknown');
    await expect(page.getByRole('cell', { name: id === '510300.XSHG' ? '11.123' : '9.875', exact: true }).first()).toBeVisible();
    await page.getByLabel('开始日期', { exact: true }).fill('2030-01-01');
    await page.getByRole('button', { name: '查询日线', exact: true }).click();
    await expect(page.getByText('该范围没有行情，未补造零 Bar。', { exact: true })).toBeVisible();
  }
  expect([...seen].sort()).toEqual(['000001.XSHE', '510300.XSHG']);
  const refreshed = page.waitForResponse(r => new URL(r.url()).pathname === '/api/v1/datasets');
  await page.getByRole('button', { name: '刷新目录', exact: true }).click();
  expect((await refreshed).status()).toBe(200);
  await expect(versions).toHaveValue('');
  await expect(versions.locator('option')).toHaveCount(7);
  await expect(page.locator('tbody tr')).toHaveCount(0);
  expect(errors).toEqual([]);
  expect(requests.some(url => url.includes('/bars?'))).toBe(true);
  expect(requests.every(url => new URL(url).origin === new URL(page.url()).origin)).toBe(true);
});

test('corrupt version is visible on selection and catalog refresh', async ({ page }) => {
  await page.goto('/datasets');
  const versions = page.getByLabel('Dataset 版本', { exact: true });
  await expect(versions.locator('option')).toHaveCount(7);
  const preview = await versions.locator('option').evaluateAll(options => options
    .filter(o => (o.textContent ?? '').includes('1 证券 / 2 Bars'))
    .map(o => o.getAttribute('value') ?? ''));
  const dsv = preview[1];
  if (!dsv) throw new Error('Expected published preview version');
  const manifest = join(lake(), 'manifests', `${dsv.replace('dsv:sha256:', '')}.json`);
  const original = await readFile(manifest);
  try {
    await writeFile(manifest, '{}');
    const failed = page.waitForResponse(r => r.url().includes('/api/v1/datasets/') && r.status() >= 400);
    await versions.selectOption(dsv);
    await failed;
    await expect(page.getByRole('alert')).toBeVisible();
    await expect(page.getByLabel('证券', { exact: true })).toHaveCount(0);
    await page.getByRole('button', { name: '刷新目录', exact: true }).click();
    await expect(page.getByRole('alert')).toContainText('存在不可读取的版本');
    await expect(versions.locator('option')).toHaveCount(6);
  } finally { await writeFile(manifest, original); }
});

test('empty catalog and unavailable lake are visible without skipping', async ({ page }) => {
  const root = lake();
  await rename(join(root, 'manifests'), join(root, 'saved-manifests'));
  try {
    await page.goto('/datasets');
    await expect(page.getByText('没有可读取的已发布 Dataset。', { exact: true })).toBeVisible();
  } finally { await rename(join(root, 'saved-manifests'), join(root, 'manifests')); }
  await rename(root, `${root}-unavailable`);
  try {
    await page.reload();
    // The independent corporate-action panel also reports the missing root.
    await expect(page.getByRole('alert').first()).toBeVisible();
    await expect(page.getByLabel('Dataset 版本', { exact: true })).toHaveCount(0);
  } finally { await rename(`${root}-unavailable`, root); }
});
