import { test, expect } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

test('pool edits publish immutable versions and freeze a real production task', async ({ page }) => {
  test.setTimeout(120_000);
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  const registry = await (await page.request.get('/api/v1/research-pools')).json();
  expect(registry.data.pools.map((p: { draft: string[] }) => p.draft.length)).toEqual([5, 4, 4]);
  const original = registry.data.pools[1];
  const catalog = await (await page.request.get('/api/v1/datasets')).json();
  const details = await Promise.all(catalog.data.datasets.map(async (d: { dsv: string }) => (await page.request.get(`/api/v1/datasets/${encodeURIComponent(d.dsv)}`)).json()));
  const tradable = details.find((d) => d.data.normalizer_version === '2' && d.data.instruments[0].instrument_id === '510300.XSHG').data.dsv;
  const varied = details.find((d) => d.data.instruments.some((i: { instrument_id: string; coverage: { bars: number } }) => i.instrument_id === '510880.XSHG' && i.coverage.bars === 3)).data.dsv;
  await page.goto('/pools');
  await page.getByLabel('研究池', { exact: true }).selectOption('dividend');
  await expect(page.getByTestId('pool-members').getByRole('listitem')).toHaveCount(4);
  await expect(page.getByTestId('pool-metadata').first()).toContainText('未选择 Dataset');
  await page.getByLabel('草稿成员', { exact: true }).fill('510300.XSHG');
  await expect(page.getByRole('button', { name: '发布固定版本', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: '保存草稿', exact: true }).click();
  await expect(page.getByRole('status')).toContainText('草稿已保存');
  await expect(page.getByTestId('pool-members').getByRole('listitem')).toHaveCount(4);
  await page.getByRole('button', { name: '发布固定版本', exact: true }).click();
  await expect(page.getByLabel('固定版本', { exact: true })).toHaveValue('2');
  await expect(page.getByTestId('pool-members')).toHaveText('510300.XSHG');
  await page.getByLabel('研究池 Dataset', { exact: true }).selectOption(tradable);
  await expect(page.getByTestId('pool-metadata')).toContainText('100 / 0.001 / 1');
  await expect(page.getByTestId('pool-metadata')).toContainText('2010-01-01 / 2020-01-01');
  const adjustedCatalog = (await (await page.request.get(`/api/v1/datasets/${encodeURIComponent(tradable)}`)).json()).data.adjusted_series.versions;
  const bound = adjustedCatalog.find((v: { coverage: { rows: number; last: string } }) => v.coverage.rows === 2 && v.coverage.last === '2026-01-06').version;
  await page.getByLabel('复权版本 510300.XSHG', { exact: true }).selectOption(bound);
  await expect(page.getByRole('button', { name: '提交固定池实验', exact: true })).toBeDisabled();
  const plannedResponse = page.waitForResponse(r => r.url().endsWith('/selections/preflight'));
  await page.getByRole('button', { name: '预检共同窗口', exact: true }).click();
  const planned = (await (await plannedResponse).json()).data;
  await expect(page.getByTestId('pool-preflight')).toContainText('评价窗口/可运行范围：2026-01-05 ～ 2026-01-06');
  await page.getByRole('button', { name: '提交固定池实验', exact: true }).click();
  const link = page.getByRole('link', { name: '查看固定池任务', exact: true });
  await expect(link).toBeVisible();
  const taskId = new URL((await link.getAttribute('href'))!, 'http://localhost').searchParams.get('task')!;
  const lake = join(process.env.PRAJNA_E2E_ROOT!, 'fixture', 'lake');
  const taskFile = join(lake, 'tasks', taskId, 'experiment.json');
  const frozen = await readFile(taskFile, 'utf8');
  const definition = JSON.parse(frozen);
  expect(definition.dsv).toBe(tradable);
  expect(definition.adjusted_versions).toEqual({ '510300.XSHG': bound });
  expect(definition.evaluation).toEqual({ semantic_version: 'common-window@1',
    common_start: planned.window.common_start, common_end: planned.window.common_end,
    start: planned.window.start, end: planned.window.end, warmup_sessions: planned.window.warmup_sessions });
  const version2 = (await (await page.request.get('/api/v1/research-pools')).json()).data.pools[1].versions[1];
  expect(definition.universe).toBe(version2.universe);
  await page.getByLabel('草稿成员', { exact: true }).fill('510300.XSHG\n510880.XSHG');
  await page.getByRole('button', { name: '保存草稿', exact: true }).click();
  await expect(page.getByRole('status')).toContainText('草稿已保存');
  await page.getByRole('button', { name: '发布固定版本', exact: true }).click();
  await expect(page.getByLabel('固定版本', { exact: true })).toHaveValue('3');
  await page.getByLabel('研究池 Dataset', { exact: true }).selectOption(varied);
  await expect(page.getByTestId('pool-metadata').filter({ hasText: '510880.XSHG' })).toContainText('10 / 0.01 / 1');
  await expect(page.getByTestId('pool-metadata').filter({ hasText: '510300.XSHG' })).toContainText('100 / 0.001 / 1');
  expect(await readFile(taskFile, 'utf8')).toBe(frozen);
  const finalRegistry = (await (await page.request.get('/api/v1/research-pools')).json()).data;
  expect(finalRegistry.pools[1].versions[0]).toEqual(original.versions[0]);
  expect(finalRegistry.pools[1].versions[1]).toEqual(version2);
  await page.reload();
  await expect(page.getByLabel('研究池 Dataset', { exact: true })).toHaveValue(varied);
  await page.getByLabel('固定版本', { exact: true }).selectOption('2');
  await expect(page.getByTestId('pool-members')).toHaveText('510300.XSHG');
  await page.getByLabel('固定版本', { exact: true }).selectOption('1');
  await expect(page.getByTestId('pool-members').getByRole('listitem')).toHaveCount(4);
  await page.goto(`/backtests?task=${encodeURIComponent(taskId)}`);
  await expect(page.getByTestId('task-status')).toContainText('已完成', { timeout: 60_000 });
  const result = await (await page.request.get(`/api/v1/tasks/experiments/${taskId}/result`)).json();
  const rows = result.data.runs[0].series.account_valuations;
  const first = rows.find((r: { session_date: string; valuation_point: string }) => r.session_date === '2026-01-05' && r.valuation_point === 'close');
  const last = rows.find((r: { session_date: string; valuation_point: string }) => r.session_date === '2026-01-06' && r.valuation_point === 'close');
  expect(first.positions).toEqual({});
  expect([...new Set(rows.map((r: { session_date: string }) => r.session_date))]).toEqual([planned.window.start, planned.window.end]);
  // Independent hand calculation: 99,900 at 10.002, commission 299.75994;
  // cash 500.44006 + 99,900 × 10.1 = 1,009,490.44006, receivables zero.
  expect(last.positions['510300.XSHG'].quantity).toBe('99900');
  expect(last.cash).toBe('500.44006');
  expect(last.equity).toBe('1009490.44006');
  expect(last.dividend_receivable).toBe('0');
  expect(await readFile(taskFile, 'utf8')).toBe(frozen);
  expect(errors).toEqual([]);
});

test('pool HTTP rejects stale edits, invalid fields and foreign origins', async ({ request }) => {
  const registry = await (await request.get('/api/v1/research-pools')).json();
  const pool = registry.data.pools[0];
  for (const members of [[], ['510300.XSHG', '510300.XSHG'], ['510300.SZSE']]) {
    expect((await request.post('/api/v1/research-pools', { data: { op: 'save', id: pool.id, expected: pool.draft, members } })).status()).toBe(400);
  }
  expect((await request.post('/api/v1/research-pools', { data: { op: 'list', extra: true } })).status()).toBe(400);
  expect((await request.post('/api/v1/research-pools', { data: { op: 'publish', id: pool.id, expected: [] } })).status()).toBe(409);
  expect((await request.post('/api/v1/research-pools', { data: { op: 'publish', id: 'missing', expected: [] } })).status()).toBe(404);
  const listed = await request.get('/api/v1/research-pools');
  expect(listed.headers()['cache-control']).toBe('no-store');
  expect((await request.get('/api/v1/research-pools', { headers: { Origin: 'https://foreign.invalid' } })).status()).toBe(403);
  expect((await request.post('/api/v1/tasks/selections', { data: { dsv: 'synthetic', pool: { id: pool.id, version: 999 }, strategy: 'buy_and_hold', idempotency_key: 'missing-pool' } })).status()).toBe(404);
});

test('production pool preflight preserves bindings, blockers, native combinations and Session gaps', async ({ page }) => {
  test.setTimeout(120_000);
  const errors: string[] = [];
  page.on('pageerror', e => errors.push(e.message));
  const catalog = (await (await page.request.get('/api/v1/datasets')).json()).data.datasets;
  const details = await Promise.all(catalog.map(async (d: { dsv: string }) => (await page.request.get(`/api/v1/datasets/${encodeURIComponent(d.dsv)}`)).json()));
  const varied = details.find(d => d.data.instruments.some((i: { instrument_id: string; coverage: { bars: number } }) => i.instrument_id === '510880.XSHG' && i.coverage.bars === 3)).data;
  const tradable = details.find(d => d.data.normalizer_version === '2' && d.data.instruments[0].instrument_id === '510300.XSHG').data;
  const publish = async (members: string[]) => {
    const registry = (await (await page.request.get('/api/v1/research-pools')).json()).data;
    const pool = registry.pools.find((p: { id: string }) => p.id === 'bonds');
    expect((await page.request.post('/api/v1/research-pools', { data: { op: 'save', id: 'bonds', expected: pool.draft, members } })).ok()).toBe(true);
    const response = await page.request.post('/api/v1/research-pools', { data: { op: 'publish', id: 'bonds', expected: [...members].sort() } });
    expect(response.ok()).toBe(true);
    return (await response.json()).data.pools.find((p: { id: string }) => p.id === 'bonds').versions.at(-1).version.toString();
  };
  const select = async (version: string, dsv: string) => {
    await page.goto('/pools');
    await page.getByLabel('研究池', { exact: true }).selectOption('bonds');
    await page.getByLabel('固定版本', { exact: true }).selectOption(version);
    await page.getByLabel('研究池 Dataset', { exact: true }).selectOption(dsv);
  };
  const check = async () => {
    const response = page.waitForResponse(r => r.url().endsWith('/selections/preflight'));
    await page.getByRole('button', { name: '预检共同窗口', exact: true }).click();
    return response;
  };
  const submit = page.getByRole('button', { name: '提交固定池实验', exact: true });
  // Published pool members absent from DSV fail visibly, without inventing coverage.
  await select('1', varied.dsv);
  await expect(submit).toBeDisabled();
  expect((await check()).status()).toBe(404);
  await expect(page.getByRole('alert')).toContainText('security is absent');
  // A mixed Venue version can be managed but this research entry refuses it.
  const mixed = await publish(['510300.XSHG', '159952.XSHE']);
  await select(mixed, varied.dsv);
  expect((await check()).status()).toBe(400);
  await expect(page.getByRole('alert')).toContainText('requires one venue');
  await expect(submit).toBeDisabled();
  // Each member chooses its own immutable version; a partial map cannot run.
  const pair = await publish(['510300.XSHG', '510880.XSHG']);
  await select(pair, varied.dsv);
  const bindings: Record<string, string> = {};
  for (const member of ['510300.XSHG', '510880.XSHG']) {
    bindings[member] = varied.adjusted_series.versions.find((v: { instrument_id: string }) => v.instrument_id === member).version;
  }
  await page.getByLabel('复权版本 510300.XSHG', { exact: true }).selectOption(bindings['510300.XSHG']);
  const partial = (await (await check()).json()).data;
  expect(partial.runnable).toBe(false);
  await expect(page.getByTestId('pool-preflight')).toContainText('missing adjusted version for 510880.XSHG');
  await expect(submit).toBeDisabled();
  await page.getByLabel('复权版本 510880.XSHG', { exact: true }).selectOption(bindings['510880.XSHG']);
  const paired = (await (await check()).json()).data;
  expect(paired.adjusted).toEqual(bindings);
  expect(paired.window.members).toHaveLength(2);
  await expect(submit).toBeEnabled();
  const single = await publish(['510300.XSHG']);
  await select(single, tradable.dsv);
  const missing = await (await check()).json();
  expect(missing.data.blocking[0].code).toBe('adjusted_version_missing');
  await expect(page.getByTestId('pool-preflight')).toContainText('adjusted_version_missing');
  await expect(submit).toBeDisabled();
  const versions = tradable.adjusted_series.versions;
  const full = versions.find((v: { coverage: { rows: number } }) => v.coverage.rows === 3).version;
  const gap = versions.find((v: { coverage: { rows: number; last: string } }) => v.coverage.rows === 2 && v.coverage.last === '2026-01-07').version;
  const binding = page.getByLabel('复权版本 510300.XSHG', { exact: true });
  await binding.selectOption(full);
  await expect(page.getByTestId('pool-preflight')).toHaveCount(0);
  const runnable = (await (await check()).json()).data;
  expect(runnable.runnable).toBe(true);
  await expect(submit).toBeEnabled();
  await expect(page.getByTestId('pool-identity')).toContainText(tradable.dsv);
  // Refresh keeps the chosen fixed pool/DSV/version; preflight must be rerun.
  await page.reload();
  await expect(page.getByLabel('固定版本', { exact: true })).toHaveValue(single);
  await expect(page.getByLabel('研究池 Dataset', { exact: true })).toHaveValue(tradable.dsv);
  await expect(binding).toHaveValue(full);
  await expect(submit).toBeDisabled();
  await page.getByLabel('预检策略', { exact: true }).selectOption('ma_crossover');
  await page.getByLabel('参数组合 JSON', { exact: true }).fill(JSON.stringify({ grid: { params_version: [1], short_window: [1], long_window: [2, 4] } }));
  const insufficient = (await (await check()).json()).data;
  expect(insufficient.window.warmup_sessions).toBe(3);
  expect(insufficient.window.parameters.map((p: { runnable: boolean }) => p.runnable)).toEqual([true, false]);
  await expect(page.getByTestId('pool-preflight')).toContainText('最大预热：3 Sessions');
  await expect(page.getByTestId('pool-preflight')).toContainText('insufficient_warmup');
  await expect(submit).toBeDisabled();
  await page.getByLabel('预检策略', { exact: true }).selectOption('buy_and_hold');
  await page.getByLabel('参数组合 JSON', { exact: true }).fill('{"list":[{"params_version":1}]}');
  await binding.selectOption(gap);
  const preserved = (await (await check()).json()).data;
  expect(preserved.window.common_start).toBe('2026-01-05');
  expect(preserved.window.common_end).toBe('2026-01-07');
  expect(preserved.window.members[0].research_sessions).toBe(2);
  expect(preserved.window.warnings.length).toBe(1);
  await expect(page.getByTestId('pool-preflight')).toContainText(preserved.window.warnings[0]);
  await expect(submit).toBeEnabled();
  await page.getByLabel('研究池 Dataset', { exact: true }).selectOption(varied.dsv);
  await expect(binding).toHaveValue('');
  await expect(submit).toBeDisabled();
  expect(errors).toEqual([]);
});
