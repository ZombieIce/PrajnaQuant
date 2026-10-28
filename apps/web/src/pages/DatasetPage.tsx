import { useEffect, useState } from 'react';
import { api, ErrorBox, Header, Panel } from '../shared';

type Envelope<T> = { schema_version: 1; data: T };
type Summary = { dsv: string; created_at: string; normalizer_id: string; normalizer_version: string; instruments: number; sessions: number; bars: number; first_session: string | null; last_session: string | null; venues: string[]; source: string | null; coverage_notice: string };
type Catalog = { datasets: Summary[]; invalid_manifests: { manifest: string; error: string }[] };
type Field = { state: string; value: string | null; reason: string | null; source_id: string | null; observed_at: string | null };
type Instrument = { instrument_id: string; venue: string; kind: string; native_symbol: string; base_currency: string | null; quote_currency: string | null; session_timezone: string; lot_size: string | null; price_increment: string | null; size_increment: string | null; lifecycle: { established_date: Field; listing_date: Field }; coverage: { bars: number; first_bar: string | null; last_bar: string | null; venue_sessions: number; sessions_in_bar_range: number; sessions_without_bar_in_observed_range: number; session_gaps: string[]; session_gaps_truncated: boolean; sessions_before_first_bar: number; sessions_after_last_bar: number; sources: string[]; bars_without_source: number } };
type Detail = { dsv: string; created_at: string; normalizer_id: string; normalizer_version: string; coverage: unknown; venues: unknown[]; instruments: Instrument[]; warnings: string[] };
type Bar = { trade_date: string; open: string; high: string; low: string; close: string; volume: string; amount: string | null; available_at: string | null; source: string | null; source_raw_sha256: string | null; reference_price: string | null };
type Bars = { dsv: string; total: number; offset: number; next_offset: number | null; price_adjustment: string; price_unit: string; volume_unit: string; amount_unit: string; items: Bar[]; warnings: string[] };
const known = (value: string | null | undefined) => value ?? 'Unknown（未知）';
const lifecycle = (field: Field) => field.state === 'known' ? known(field.value) : `${field.state} · ${known(field.reason)}`;
const json = (value: unknown) => JSON.stringify(value, null, 2);

export default function DatasetPage() {
  const [catalog, setCatalog] = useState<Catalog | null>(null);
  const [dsv, setDsv] = useState('');
  const [detail, setDetail] = useState<Detail | null>(null);
  const [error, setError] = useState('');
  const [detailError, setDetailError] = useState('');
  const [refresh, setRefresh] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    setCatalog(null); setError('');
    api<Envelope<Catalog>>('/api/v1/datasets', { signal: controller.signal })
      .then((r) => { if (!controller.signal.aborted) setCatalog(r.data); })
      .catch((e: Error) => { if (!controller.signal.aborted) setError(e.message); });
    return () => controller.abort();
  }, [refresh]);
  useEffect(() => {
    const controller = new AbortController();
    setDetail(null); setDetailError('');
    if (dsv) api<Envelope<Detail>>(`/api/v1/datasets/${encodeURIComponent(dsv)}`, { signal: controller.signal })
      .then((r) => { if (!controller.signal.aborted) setDetail(r.data); })
      .catch((e: Error) => { if (!controller.signal.aborted) setDetailError(e.message); });
    return () => controller.abort();
  }, [dsv]);
  return <><Header /><main className="dataset-page">
    <div className="page-title-row"><div><h1>研究数据预览</h1><p>选择已发布的 Dataset 版本，查看证券、生命周期、实际覆盖和原始未复权行情。预览只读本机数据。</p></div></div>
    <Panel title="已发布版本">
      <button type="button" className="secondary" onClick={() => { setDsv(''); setDetail(null); setRefresh((n) => n + 1); }}>刷新目录</button>
      {error && <ErrorBox>{error}</ErrorBox>}
      {!catalog && !error && <p role="status">正在读取已发布目录…</p>}
      {catalog && <><label>Dataset 版本<select aria-label="Dataset 版本" value={dsv} onChange={(e) => { setDetail(null); setDsv(e.target.value); }}>
        <option value="">请选择具体版本</option>
        {catalog.datasets.map((item) => <option key={item.dsv} value={item.dsv}>{item.created_at} · {item.venues.join('/')} · {item.instruments} 证券 / {item.bars} Bars · {item.dsv}</option>)}
      </select></label>
        {!catalog.datasets.length && <p role="status">没有可读取的已发布 Dataset。</p>}
        {catalog.invalid_manifests.length > 0 && <ErrorBox>存在不可读取的版本；它们未列入可选版本。<pre>{json(catalog.invalid_manifests)}</pre></ErrorBox>}
      </>}
    </Panel>
    {detailError && <ErrorBox>{detailError}</ErrorBox>}
    {dsv && !detail && !detailError && <p role="status">正在读取固定版本…</p>}
    {detail?.dsv === dsv && <DatasetDetail key={dsv} detail={detail} />}
  </main></>;
}

function DatasetDetail({ detail }: { detail: Detail }) {
  const [id, setId] = useState('');
  const instrument = detail.instruments.find((item) => item.instrument_id === id);
  return <>
    <Panel title="数据口径与覆盖">
      <p><code>{detail.dsv}</code></p><p>Normalizer：{detail.normalizer_id} @ {detail.normalizer_version} · 创建于 {detail.created_at}</p>
      <p>原始未复权价格；Session 覆盖不等于可交易行情覆盖，缺行情不视为停牌或零价。</p>
      {detail.warnings.map((warning, index) => <ErrorBox key={index}>{warning}</ErrorBox>)}
      <details><summary>发布声明与交易所 Session 覆盖</summary><pre>{json({ coverage: detail.coverage, venues: detail.venues })}</pre></details>
      <label>证券<select aria-label="证券" value={id} onChange={(e) => setId(e.target.value)}><option value="">请选择证券</option>{detail.instruments.map((item) => <option key={item.instrument_id} value={item.instrument_id}>{item.native_symbol} · {item.venue} · {item.kind}</option>)}</select></label>
      {!detail.instruments.length && <p>该版本未包含证券，不能据此宣称有行情覆盖。</p>}
    </Panel>
    {instrument && <><InstrumentDetail instrument={instrument} /><BarsPreview key={id} dsv={detail.dsv} instrument={instrument} /></>}
  </>;
}

function InstrumentDetail({ instrument: i }: { instrument: Instrument }) {
  const c = i.coverage;
  return <Panel title="证券、生命周期与实际行情覆盖">
    <p>{i.instrument_id} · {i.kind} · {i.venue} · {i.session_timezone}</p>
    <p>计价币种 {known(i.quote_currency)} · 基础币种 {known(i.base_currency)} · 手数 {known(i.lot_size)} · 价格增量 {known(i.price_increment)} · 数量增量 {known(i.size_increment)}</p>
    <p>成立日：{lifecycle(i.lifecycle.established_date)}；上市日：{lifecycle(i.lifecycle.listing_date)}</p>
    <p>实际 Bars：{c.bars}；实际起止：{known(c.first_bar)} 至 {known(c.last_bar)}；交易所 Sessions：{c.venue_sessions}。</p>
    <p>行情起止区间内 Sessions：{c.sessions_in_bar_range}，缺失：{c.sessions_without_bar_in_observed_range}。</p>
    <p>区间内缺口：{c.session_gaps.join('、') || (c.bars ? '无' : 'Unknown（无行情区间）')}{c.session_gaps_truncated ? '（日期列表已截断，以上计数为总数）' : ''}</p>
    <p>首根行情前 Sessions：{c.sessions_before_first_bar}；末根行情后 Sessions：{c.sessions_after_last_bar}。这些计数不代表停牌或已覆盖。</p>
    <p>行情来源：{c.sources.join('、') || 'Unknown（未知）'}；缺少来源的 Bars：{c.bars_without_source}。</p>
    <details><summary>冻结的生命周期证据与来源</summary><pre>{json(i.lifecycle)}</pre></details>
  </Panel>;
}

function BarsPreview({ dsv, instrument }: { dsv: string; instrument: Instrument }) {
  const [start, setStart] = useState('');
  const [end, setEnd] = useState('');
  const [request, setRequest] = useState({ start: '', end: '', offset: 0 });
  const [bars, setBars] = useState<Bars | null>(null);
  const [error, setError] = useState('');
  useEffect(() => {
    const controller = new AbortController();
    setBars(null); setError('');
    const params = new URLSearchParams({ instrument: instrument.instrument_id, limit: '100', offset: String(request.offset) });
    if (request.start) params.set('start', request.start);
    if (request.end) params.set('end', request.end);
    api<Envelope<Bars>>(`/api/v1/datasets/${encodeURIComponent(dsv)}/bars?${params}`, { signal: controller.signal })
      .then((r) => { if (!controller.signal.aborted) setBars(r.data); })
      .catch((e: Error) => { if (!controller.signal.aborted) setError(e.message); });
    return () => controller.abort();
  }, [dsv, instrument.instrument_id, request]);
  return <Panel title="原始未复权行情">
    <form className="market-range" onSubmit={(e) => { e.preventDefault(); setRequest({ start, end, offset: 0 }); }}>
      <label>开始日期<input aria-label="开始日期" type="date" value={start} onChange={(e) => setStart(e.target.value)} /></label>
      <label>结束日期<input aria-label="结束日期" type="date" value={end} onChange={(e) => setEnd(e.target.value)} /></label>
      <button type="submit" className="primary">查询日线</button>
    </form>
    {error && <ErrorBox>{error}</ErrorBox>}
    {!bars && !error && <p role="status">正在读取原始行情…</p>}
    {bars && <><p>价格：{bars.price_unit}；成交量：{bars.volume_unit}；成交额：{bars.amount_unit}。历史可用时刻缺失保持 Unknown，本地采集时间不作为历史发布时间。</p>
      {bars.warnings.map((warning, index) => <ErrorBox key={index}>{warning}</ErrorBox>)}
      <p>匹配 {bars.total} 根，当前从第 {bars.offset + (bars.items.length ? 1 : 0)} 根开始，显示 {bars.items.length} 根。</p>
      {!bars.items.length ? <p role="status">该范围没有行情，未补造零 Bar。</p> : <div style={{ overflowX: 'auto' }}><table><thead><tr>{['日期', '开', '高', '低', '收', '成交量', '成交额', '历史可用时刻', '来源'].map((x) => <th key={x}>{x}</th>)}</tr></thead><tbody>{bars.items.map((bar) => <tr key={bar.trade_date}>{[bar.trade_date, bar.open, bar.high, bar.low, bar.close, bar.volume, bar.amount, bar.available_at, bar.source].map((value, index) => <td key={index}>{known(value)}</td>)}</tr>)}</tbody></table></div>}
      <button type="button" disabled={bars.offset === 0} onClick={() => setRequest({ ...request, offset: Math.max(0, bars.offset - 100) })}>上一页</button>{' '}
      <button type="button" disabled={bars.next_offset === null} onClick={() => bars.next_offset !== null && setRequest({ ...request, offset: bars.next_offset })}>下一页</button>
      <details><summary>本页固定版本、原始来源与参考价</summary><pre>{json({ dsv: bars.dsv, adjustment: bars.price_adjustment, items: bars.items })}</pre></details>
    </>}
  </Panel>;
}
