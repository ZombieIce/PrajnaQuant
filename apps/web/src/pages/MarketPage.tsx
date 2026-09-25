import { Suspense, lazy, useState } from 'react';
import type { EChartsOption } from 'echarts';
import { api, ErrorBox, Header } from '../shared';

const Chart = lazy(() => import('../Chart').then((module) => ({ default: module.Chart })));
type Snapshot = { snapshot_id: string; sha256: string; manifest_sha256: string; created_at: string; published_at: string | null; data_cutoff_date: string; first_date: string; last_date: string; price_adjustment: string; coverage_status: string; source_status: string };
type Instrument = { instrument_id: string; symbol: string; code: string; name: string | null; exchange: string; asset_type: string; classification_notice?: string };
type SearchResult = { items: Instrument[]; next_cursor: string | null; snapshot: Snapshot; data_cutoff_date: string };
type Bar = { trade_date: string; open: number | null; high: number | null; low: number | null; close: number | null; volume_shares: number | null; amount_cny: number | null; source: string | null; observed_at: string | null };
type BarsResult = { snapshot: Snapshot; instrument: Instrument; data_cutoff_date: string; range: { start: string; end: string }; price_adjustment: string; price_unit: string; volume_unit: string; amount_unit: string; items: Bar[]; next_cursor: string | null };

const dateGap = (last: string) => Math.floor((Date.now() - Date.parse(`${last}T00:00:00+08:00`)) / 86_400_000);
const assetLabel = (type: string) => ({
  ETF: 'ETF · 当前分类（非 PIT）',
  EQUITY: '股票 · 当前分类（非 PIT）',
  EQUITY_CANDIDATE: '股票候选，分类未核验',
}[type] ?? `资产分类未知（${type}）`);

export default function MarketPage() {
  const [query, setQuery] = useState('');
  const [assetType, setAssetType] = useState('');
  const [matches, setMatches] = useState<Instrument[]>([]);
  const [searchCursor, setSearchCursor] = useState<string | null>(null);
  const [searchSnapshot, setSearchSnapshot] = useState<Snapshot | null>(null);
  const [selected, setSelected] = useState<Instrument | null>(null);
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [start, setStart] = useState('');
  const [end, setEnd] = useState('');
  const [bars, setBars] = useState<Bar[]>([]);
  const [barCursor, setBarCursor] = useState<string | null>(null);
  const [priceUnit, setPriceUnit] = useState('unknown');
  const [loadingSearch, setLoadingSearch] = useState(false);
  const [loadingBars, setLoadingBars] = useState(false);
  const [error, setError] = useState('');
  const [emptyLoaded, setEmptyLoaded] = useState(false);

  const search = async (cursor?: string) => {
    if (query.trim().length < 2) { setError('请输入至少两个字符进行搜索。'); return; }
    setError(''); setLoadingSearch(true);
    try {
      const params = new URLSearchParams({ q: query.trim(), limit: '20' });
      if (assetType) params.set('asset_type', assetType);
      if (cursor) {
        params.set('cursor', cursor);
        if (searchSnapshot) params.set('snapshot_id', searchSnapshot.snapshot_id);
      }
      const result = await api<SearchResult>(`/api/v1/instruments?${params}`);
      setMatches((current) => cursor ? [...current, ...result.items] : result.items);
      setSearchCursor(result.next_cursor);
      setSearchSnapshot(result.snapshot);
    } catch (e) { setError(`证券搜索失败：${(e as Error).message}`); }
    finally { setLoadingSearch(false); }
  };

  const loadBars = async (instrument: Instrument, rangeStart: string, rangeEnd: string, snapshotId: string, cursor?: string, append = false) => {
    setError(''); setLoadingBars(true); setEmptyLoaded(false);
    try {
      const params = new URLSearchParams({ instrument_id: instrument.instrument_id, snapshot_id: snapshotId, start: rangeStart, end: rangeEnd, limit: '1000' });
      if (cursor) params.set('cursor', cursor);
      const result = await api<BarsResult>(`/api/v1/daily-bars?${params}`);
      setSnapshot(result.snapshot); setBars((current) => append ? [...current, ...result.items] : result.items);
      setPriceUnit(result.price_unit);
      setBarCursor(result.next_cursor); setEmptyLoaded(true);
    } catch (e) { setError(`日线加载失败：${(e as Error).message}`); setBars([]); setBarCursor(null); }
    finally { setLoadingBars(false); }
  };

  const chooseInstrument = (instrument: Instrument) => {
    setSelected(instrument); setBars([]); setBarCursor(null); setError('');
    const first = searchSnapshot?.first_date ?? '';
    const last = searchSnapshot?.last_date ?? '';
    const boundedStart = first && last && Date.parse(`${last}T00:00:00+08:00`) - Date.parse(`${first}T00:00:00+08:00`) > 3660 * 86_400_000
      ? new Date(Date.parse(`${last}T00:00:00+08:00`) - 3660 * 86_400_000).toLocaleDateString('sv-SE', { timeZone: 'Asia/Shanghai' })
      : first;
    setStart(boundedStart); setEnd(last);
    if (searchSnapshot && boundedStart && last) void loadBars(instrument, boundedStart, last, searchSnapshot.snapshot_id);
  };

  const chartOption: EChartsOption = {
    animation: false,
    legend: { data: ['日 K', '成交量'], top: 6 },
    grid: [{ left: 58, right: 26, top: 42, height: '58%' }, { left: 58, right: 26, top: '75%', height: '15%' }],
    tooltip: { trigger: 'axis', confine: true, position: 'top', axisPointer: { type: 'cross' }, formatter: (params: unknown) => {
      const values = Array.isArray(params) ? params as { axisValue: string; seriesName: string; data: number[] }[] : [];
      const row = bars.find((item) => item.trade_date === values[0]?.axisValue);
      if (!row) return '';
      return `${row.trade_date}<br/>开 ${row.open ?? '缺失'} · 高 ${row.high ?? '缺失'} · 低 ${row.low ?? '缺失'} · 收 ${row.close ?? '缺失'}<br/>成交量 ${row.volume_shares ?? '缺失'} 股/份<br/>成交额 ${row.amount_cny ?? '来源未提供'} CNY<br/>来源 ${row.source ?? '未知'} · 采集 ${row.observed_at ?? '未知'}`;
    } },
    xAxis: [{ type: 'category', data: bars.map((bar) => bar.trade_date), boundaryGap: true, axisLine: { onZero: false }, splitLine: { show: false }, min: 'dataMin', max: 'dataMax' }, { type: 'category', gridIndex: 1, data: bars.map((bar) => bar.trade_date), axisLabel: { show: false }, axisTick: { show: false }, splitLine: { show: false } }],
    yAxis: [{ scale: true, splitArea: { show: true }, name: priceUnit }, { scale: true, gridIndex: 1, splitNumber: 2, axisLabel: { show: false }, axisLine: { show: false }, axisTick: { show: false }, splitLine: { show: false } }],
    dataZoom: [{ type: 'inside', xAxisIndex: [0, 1], start: 65, end: 100 }, { type: 'slider', xAxisIndex: [0, 1], top: '94%', height: 20, start: 65, end: 100 }],
    series: [
      { name: '日 K', type: 'candlestick', xAxisIndex: 0, yAxisIndex: 0, data: bars.map((bar) => [bar.open, bar.close, bar.low, bar.high]), itemStyle: { color: '#dc2626', color0: '#059669', borderColor: '#dc2626', borderColor0: '#059669' } },
      { name: '成交量', type: 'bar', xAxisIndex: 1, yAxisIndex: 1, data: bars.map((bar) => ({ value: bar.volume_shares, itemStyle: { color: bar.open !== null && bar.close !== null && bar.close >= bar.open ? '#dc2626' : '#059669' } })) },
    ],
  };
  const sourceUnknown = snapshot?.source_status === 'unknown' || bars.some((bar) => !bar.source);
  const stale = snapshot ? dateGap(snapshot.data_cutoff_date) > 7 : false;

  return <><Header /><main>
    <div className="page-title-row"><div><h1>证券行情</h1><p>浏览不可变日频快照中的股票与 ETF 原始 OHLCV。行情查询不开放股票回测能力。</p></div><span className="status-tag">服务端快照</span></div>
    <section className="universe-tools market-tools"><div className="tool-box"><h3>查找证券</h3><form className="market-search" onSubmit={(event) => { event.preventDefault(); setMatches([]); void search(); }}>
      <label>名称、代码或证券 ID<input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="例如：600000 或 沪深300ETF" /></label>
      <label>资产类型<select value={assetType} onChange={(event) => setAssetType(event.target.value)}><option value="">股票与 ETF</option><option value="stock">股票</option><option value="etf">ETF</option></select></label>
      <button className="primary" type="submit" disabled={loadingSearch}>{loadingSearch ? '搜索中…' : '搜索'}</button>
    </form>
    {error && !selected && <ErrorBox>{error}</ErrorBox>}
    {matches.length > 0 && <div className="market-results" role="listbox" aria-label="证券搜索结果">{matches.map((instrument) => <button type="button" role="option" aria-selected={selected?.instrument_id === instrument.instrument_id} key={instrument.instrument_id} onClick={() => chooseInstrument(instrument)}><b>{instrument.name ?? instrument.code} <small>{instrument.code}</small></b><span>{instrument.exchange} · {assetLabel(instrument.asset_type)}</span></button>)}</div>}
    {loadingSearch && <div className="empty-state" role="status">正在读取快照目录…</div>}
    {searchCursor && <button className="secondary" type="button" disabled={loadingSearch} onClick={() => void search(searchCursor)}>更多证券</button>}
    {!matches.length && !loadingSearch && query.length >= 2 && !error && <div className="empty-state">快照中没有匹配证券。</div>}
    </div></section>
    {selected && <>
      <section className="market-selection"><div><small>当前证券</small><h2>{selected.name ?? '名称未知'} <code>{selected.code}</code></h2><p>{selected.instrument_id} · {selected.exchange} · {assetLabel(selected.asset_type)}</p></div>
        <div className="market-range"><label>开始日期<input type="date" value={start} onChange={(event) => setStart(event.target.value)} /></label><label>结束日期<input type="date" value={end} onChange={(event) => setEnd(event.target.value)} /></label><button className="primary" disabled={loadingBars || !snapshot} onClick={() => snapshot && void loadBars(selected, start, end, snapshot.snapshot_id)}>查询日线</button></div></section>
      {snapshot && <><section className="market-notices" aria-live="polite">
        <div className={snapshot.coverage_status === 'complete' ? 'success-notice' : 'warning-card warning'}><b>{snapshot.coverage_status === 'complete' ? '快照覆盖已发布为 complete' : `覆盖状态：${snapshot.coverage_status}`}</b><span>按当前快照覆盖标记展示，未推断缺失日期的零价格或可交易性。</span></div>
        {stale && <div className="warning-card warning"><b>数据可能陈旧</b><span>数据截止日为 {snapshot.data_cutoff_date}，早于最近 7 个自然日。</span></div>}
        {sourceUnknown && <div className="warning-card warning"><b>行情来源未知或未完整记录</b><span>逐日 source/observed_at 按快照原值显示；缺失来源不会由选择规则补造。</span></div>}
      </section>
      <details className="snapshot-details market-snapshot" open><summary>数据身份与口径</summary><dl><dt>Snapshot ID</dt><dd><code>{snapshot.snapshot_id}</code></dd><dt>行情文件 SHA-256</dt><dd><code>{snapshot.sha256}</code></dd><dt>Manifest SHA-256</dt><dd><code>{snapshot.manifest_sha256}</code></dd><dt>价格口径 / 单位</dt><dd>原始未复权 · {priceUnit}</dd><dt>成交量 / 成交额</dt><dd>股/份 · CNY</dd><dt>数据截止日</dt><dd>{snapshot.data_cutoff_date}</dd><dt>数据来源状态</dt><dd>{snapshot.source_status === 'unknown' ? '未知' : snapshot.source_status}</dd></dl></details></>}
      {error && <ErrorBox>{error}</ErrorBox>}
      {loadingBars && <div className="notice" role="status">正在从固定快照读取日线…</div>}
      {!loadingBars && bars.length > 0 && <section className="panel market-chart-panel"><h2>{selected.name ?? selected.code} · 日 K / 成交量 · 共加载 {bars.length} 根</h2><Suspense fallback={<div className="empty-state">正在载入图表模块…</div>}><Chart option={chartOption} /></Suspense></section>}
      {emptyLoaded && !bars.length && !loadingBars && <div className="notice" role="status">该日期范围没有日线数据。</div>}
      {barCursor && <button className="secondary market-more" disabled={loadingBars} onClick={() => void loadBars(selected, start, end, snapshot!.snapshot_id, barCursor, true)}>加载下一页（保持同一快照）</button>}
      {bars.length >= 1000 && <p className="hint">每页最多 1000 根，页面保留完整交易日 OHLC；缩放只改变视窗，不降采样数据。</p>}
    </>}
  </main></>;
}
