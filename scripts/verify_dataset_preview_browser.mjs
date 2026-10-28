// Standalone synthetic production smoke; the reusable browser test framework is T04.
import { readFile } from 'node:fs/promises';
const { chromium } = await import(process.env.PRAJNA_PLAYWRIGHT_MODULE ?? 'playwright-core');
import assert from 'node:assert/strict';
assert.equal(await readFile(`${process.env.PRAJNA_PREVIEW_FIXTURE_ROOT}/synthetic-preview-fixture`, 'utf8'), 'synthetic only\n');
const url = process.argv[2] ?? 'http://127.0.0.1:7878';
assert(['127.0.0.1', 'localhost', '[::1]'].includes(new URL(url).hostname));
const browser = await chromium.launch({ executablePath: process.env.PRAJNA_CHROME_EXECUTABLE, headless: true, timeout: 15000 });
try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 1000 } });
  page.setDefaultTimeout(10000);
  const errors = [];
  page.on('pageerror', e => errors.push(e.message));
  const requests = [];
  page.on('request', req => { if (req.url().includes('/api/')) requests.push([req.method(), req.url()]); });
  await page.goto(`${url}/datasets`, { waitUntil: 'networkidle', timeout: 15000 });
  assert.match(await page.locator('h1').innerText(), /研究数据预览/);
  const versions = page.getByLabel('Dataset 版本', { exact: true });
  const values = await versions.locator('option').evaluateAll(options => options.map(o => o.value).filter(Boolean));
  assert.equal(values.length, 2);
  assert.equal(await versions.inputValue(), '');
  let known = false, unknown = false;
  for (const value of values) {
    await versions.selectOption(value);
    const instrument = page.getByLabel('证券', { exact: true });
    await instrument.locator('option').nth(1).waitFor({ state: 'attached' });
    await instrument.selectOption({ index: 1 });
    await page.getByRole('cell', { name: '2026-01-05', exact: true }).waitFor();
    assert.equal(await page.locator('tbody tr').count(), 2);
    const text = await page.locator('main').innerText();
    assert.match(text, /2026-01-06、2026-01-07/);
    assert.match(text, /实际 Bars：2/);
    assert.match(text, /原始未复权行情/);
    assert.match(text, /Unknown/);
    if (text.includes('000001.XSHE')) { unknown = true; assert.match(text, /上市日：unknown/); assert.match(text, /9\.875/); }
    if (text.includes('510300.XSHG')) { known = true; assert.match(text, /上市日：2020-01-01/); assert.match(text, /11\.123/); }
    if (process.env.PRAJNA_PREVIEW_SCREENSHOT_PREFIX) await page.screenshot({ path: `${process.env.PRAJNA_PREVIEW_SCREENSHOT_PREFIX}-${text.includes('000001.XSHE') ? 'stock' : 'etf'}.png`, fullPage: true });
    await page.getByLabel('开始日期', { exact: true }).fill('2030-01-01');
    await page.getByRole('button', { name: '查询日线', exact: true }).click();
    await page.getByText('该范围没有行情，未补造零 Bar。', { exact: true }).waitFor();
  }
  assert(known && unknown);
  await page.getByRole('button', { name: '刷新目录', exact: true }).click();
  await page.getByLabel('Dataset 版本', { exact: true }).waitFor();
  assert.equal(await versions.inputValue(), '');
  assert.equal(await page.locator('tbody tr').count(), 0);
  assert.deepEqual(errors, []);
  assert(requests.length >= 8 && requests.every(([method]) => method === 'GET'));
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth), false);
  const foreign = await browser.newContext({ permissions: ['local-network-access'] });
  await foreign.route('https://synthetic-review.invalid/**', route => route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>Synthetic origin</title>' }));
  const foreignPage = await foreign.newPage();
  await foreignPage.goto('https://synthetic-review.invalid');
  assert.equal(await foreignPage.evaluate(async target => { try { await fetch(target); return false; } catch { return true; } }, `${url}/api/v1/datasets`), true);
  await foreign.close();
  console.log('PASS: external synthetic origin cannot read Dataset API even with ephemeral local-network permission; production browser Dataset selection, ETF Known and stock Unknown lifecycle, gaps, original Bars, empty range and refresh; only GET APIs, no page errors or horizontal overflow.');
} finally { await browser.close(); }
