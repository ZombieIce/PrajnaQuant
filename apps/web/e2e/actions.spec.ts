import { test, expect } from '@playwright/test';
import { readFile, writeFile, rename } from 'node:fs/promises';
import { join } from 'node:path';
const lake = () => {
  if (!process.env.PRAJNA_E2E_ROOT) throw new Error('Run through npm run test:e2e');
  return join(process.env.PRAJNA_E2E_ROOT, 'fixture', 'lake');
};

type Version = { version: string; observed_at: string };
const catalog = async (page: import('@playwright/test').Page) => {
  const response = await page.request.get('/api/v1/corporate-actions');
  expect(response.status()).toBe(200);
  return (await response.json()).data as { versions: Version[]; invalid_versions: unknown[] };
};

test('production facts page shows four scoped states, revision and old version', async ({ page }) => {
  const errors: string[] = [], requests: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('request', request => { if (request.url().includes('/api/v1/corporate-actions')) requests.push(request.url()); });
  const listed = await catalog(page);
  expect(listed.versions).toHaveLength(2);
  const revision = listed.versions[0], original = listed.versions[1];
  expect(revision.observed_at > original.observed_at).toBe(true);
  await page.goto('/datasets');
  const versions = page.getByLabel('公司行动事实版本', { exact: true });
  await expect(versions.locator('option')).toHaveCount(3);
  const values = await versions.locator('option').evaluateAll(options => options.map(option => option.getAttribute('value') ?? ''));
  expect([...values].sort()).toEqual(['', original.version, revision.version].sort());
  const security = page.getByLabel('行动事实证券', { exact: true });
  const detail = page.getByTestId('action-detail');

  await versions.selectOption(original.version);
  await expect(security.locator('option')).toHaveCount(5);
  await security.selectOption('510300.XSHG');
  await expect(detail).toContainText('状态：已覆盖');
  await expect(detail).toContainText('dividend-1');
  await expect(detail).toContainText('split-1');
  await expect(detail).toContainText('0.123456789012345678');
  await expect(detail).toContainText('1.5');
  for (const date of ['2026-01-05', '2026-01-06', '2026-02-01', '2026-01-20']) {
    await expect(detail).toContainText(date);
  }
  await expect(detail).toContainText('synthetic-facts @ 1 · fixture://corporate-actions');
  await expect(detail).toContainText('2025-12-31T12:00:00+08:00');
  const persisted = await page.request.get(`/api/v1/corporate-actions/${encodeURIComponent(original.version)}?instrument=510300.XSHG&start=2026-01-01&end=2026-01-31`);
  expect(persisted.status()).toBe(200);
  const persistedData = (await persisted.json()).data;
  expect(persistedData.state).toBe('covered');
  expect(persistedData.coverage.facts[0].cash_per_share).toBe('0.123456789012345678');
  expect(await detail.textContent()).toContain(persistedData.coverage.facts[1].share_ratio);

  await security.selectOption('000001.XSHE');
  await expect(detail).toContainText('状态：明确无事件');
  await expect(detail).toContainText('明确声明该证券/区间无公司行动事件');
  await security.selectOption('159915.XSHE');
  await expect(detail).toContainText('状态：缺事实');
  await expect(detail).toContainText('available_at、cash_per_share');
  await expect(detail).toContainText('缺失（未提供）');
  await security.selectOption('600000.XSHG');
  await expect(detail).toContainText('状态：未知');
  await expect(detail).toContainText('该声明范围没有已提供事件');

  await security.selectOption('510300.XSHG');
  await expect(detail).toContainText('状态：已覆盖');
  await page.getByLabel('行动事实开始日期', { exact: true }).fill('2025-12-01');
  await page.getByLabel('行动事实结束日期', { exact: true }).fill('2025-12-31');
  const outside = page.waitForResponse(response => response.url().includes('/api/v1/corporate-actions/') && response.status() === 200);
  await page.getByRole('button', { name: '读取固定版本', exact: true }).click();
  await outside;
  await expect(detail).toContainText('状态：未知');
  await expect(detail).toContainText('不完整落在任何声明覆盖内');

  await versions.selectOption(revision.version);
  await security.selectOption('510300.XSHG');
  await expect(detail).toContainText('状态：已覆盖');
  await expect(detail).toContainText('0.2');
  await versions.selectOption(original.version);
  await security.selectOption('510300.XSHG');
  await expect(detail).toContainText('0.123456789012345678');
  await expect(detail).toContainText('fixture://corporate-actions');

  const datasets = page.getByLabel('Dataset 版本', { exact: true });
  await expect(datasets.locator('option')).toHaveCount(7);
  await datasets.selectOption({ index: 1 });
  await expect(page.locator('main')).toContainText('数据口径与覆盖');
  expect(errors).toEqual([]);
  expect(requests.length).toBeGreaterThan(0);
  expect(requests.every(url => new URL(url).origin === new URL(page.url()).origin)).toBe(true);
});

test('corrupt, unavailable and empty fact versions are explicit without blocking Datasets', async ({ page }) => {
  const listed = await catalog(page);
  expect(listed.versions).toHaveLength(2);
  const original = listed.versions[1];
  const directory = join(lake(), 'corporate-facts');
  const file = join(directory, `${original.version.replace('caf:sha256:', '')}.json`);
  const bytes = await readFile(file);
  await page.goto('/datasets');
  const versions = page.getByLabel('公司行动事实版本', { exact: true });
  await expect(versions.locator('option')).toHaveCount(3);
  try {
    await writeFile(file, '{}');
    await versions.selectOption(original.version);
    const failed = page.waitForResponse(response => response.url().includes('/api/v1/corporate-actions/') && response.status() >= 500);
    await page.getByLabel('行动事实证券', { exact: true }).selectOption('510300.XSHG');
    await failed;
    await expect(page.getByRole('alert')).toContainText('invalid or corrupt fact version');
    await expect(page.getByTestId('action-detail')).toHaveCount(0);
    await expect(page.getByLabel('Dataset 版本', { exact: true }).locator('option')).toHaveCount(7);
    const refreshed = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/corporate-actions' && response.status() === 200);
    await page.getByRole('button', { name: '刷新行动事实', exact: true }).click();
    await refreshed;
    await expect(versions.locator('option')).toHaveCount(2);
    await expect(page.getByRole('alert')).toContainText('存在不可读取的公司行动事实版本');
  } finally {
    await writeFile(file, bytes);
  }
  await rename(directory, `${directory}-saved`);
  try {
    const refreshed = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/corporate-actions' && response.status() === 200);
    await page.getByRole('button', { name: '刷新行动事实', exact: true }).click();
    await refreshed;
    await expect(page.getByText('没有可读取的公司行动事实版本。', { exact: true })).toBeVisible();
    const datasets = page.getByLabel('Dataset 版本', { exact: true });
    await expect(datasets.locator('option')).toHaveCount(7);
    await datasets.selectOption({ index: 1 });
    await expect(page.locator('main')).toContainText('数据口径与覆盖');
  } finally {
    await rename(`${directory}-saved`, directory);
  }
  const root = lake();
  await rename(root, `${root}-unavailable`);
  try {
    await page.getByRole('button', { name: '刷新行动事实', exact: true }).click();
    await expect(page.locator('section.panel', { hasText: '公司行动事实覆盖' }).getByRole('alert')).toBeVisible();
  } finally {
    await rename(`${root}-unavailable`, root);
  }
});
