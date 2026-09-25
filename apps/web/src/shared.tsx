import { Component, useEffect, useMemo, useState, type ReactNode } from 'react';

export type Signal = { key: string; display_name: string; description: string };
export type Daily = { date: string; rank_ic: number | null; pearson_ic: number | null; count: number; expected_count: number; factor_count: number; missing_factor_count: number; missing_label_count: number; quantile_returns: number[]; long_short_return: number | null; factor_autocorrelation_lag1: number | null; top_quantile_turnover: number | null };
export type Report = { forward_days: number; label_method: 'close_to_close' | 'next_open_to_forward_open'; evaluation_status: 'evaluated' | 'insufficient_cross_section' | 'no_valid_ic'; mean_pearson_ic: number | null; mean_rank_ic: number | null; rank_ic_ir: number | null; rank_ic_positive_rate: number | null; rank_ic_stddev: number | null; mean_long_short_return: number | null; mean_factor_autocorrelation_lag1: number | null; mean_top_quantile_turnover: number | null; observation_count: number; labeled_observation_count: number; missing_label_count: number; missing_factor_count: number; expected_observation_count: number; coverage: number | null; evaluated_dates: number; daily: Daily[] };
export type Payload = { report: { definition: Signal; calendar_basis: string; observation_count: number; first_date: string | null; last_date: string | null; reports: Report[] } };
export type StrategySummary = { experiment_id: string; name: string; created_at: string; total_return: number; annualized_return: number; max_drawdown: number; sharpe_ratio: number; calmar_ratio: number; mean_rank_ic: number | null; lookback_days: number; score_mode?: string; top_n: number; rebalance_every: number; universe_id?: string | null; version_id?: string | null; universe_name?: string | null; universe_status?: string; universe_pit_status?: string | null; universe_coverage?: string | null };
export type Equity = { date: string; equity: number; drawdown: number };
export type PositionHolding = { symbol: string; quantity: number; mark_price: number; market_value: number; mark_date?: string | null; stale_calendar_days?: number | null };
export type PositionPoint = { date: string; cash: number; invested_value: number; capital_utilization: number; holdings: PositionHolding[] };
export type InstrumentPerformance = { symbol: string; buy_quantity: number; sell_quantity: number; trade_count: number; realized_pnl: number; unrealized_pnl: number; total_pnl: number; final_quantity: number; mark_price: number | null; market_value: number };
export type RebalanceDeferral = { decision_date: string; attempt_date: string; target_symbols: string[]; blocked_symbols: string[]; reason: string };
export type UnexecutedOrder = { decision_date: string; attempt_date: string; symbol: string; side: string; desired_quantity?: number | null; trade_status?: string | null; status_sources?: string | null; reason: string };
export type Experiment = { experiment_id: string; name: string; assumptions?: string[]; snapshot?: { snapshot_id: string; sha256: string }; config: { strategy: { lookback_days: number; momentum_short_days?: number | null; momentum_long_days?: number | null; top_n: number; rebalance_every: number } }; universe?: { universe_id: string; version_id: string; name: string; source_kind: string; source_ref: string; asset_scope: string; content_hash: string; coverage: string; pit_status: string; capabilities: Capability[]; warnings: string[] } | null; backtest: { metrics: { total_return: number; annualized_return: number; sharpe_ratio: number; calmar_ratio: number; max_drawdown: number; trade_count: number }; equity_curve: Equity[]; position_curve?: PositionPoint[]; instrument_performance?: InstrumentPerformance[]; benchmark_curve: { date: string; close: number }[]; final_positions: Record<string, number>; rebalance_deferrals?: RebalanceDeferral[]; unexecuted_orders?: UnexecutedOrder[]; execution_status_mode?: string | null } };
export type UniverseSummary = { universe_id: string; name: string; description: string; asset_scope: 'stock' | 'etf' | 'mixed'; source_kind: 'manual' | 'index_history'; source_ref: string; status: string; version_count: number; updated_at: string };
export type Instrument = { instrument_id: string; exchange: string; code: string; name: string; asset_type: 'stock' | 'etf' };
export type ManualMember = { instrument: Instrument; effective_from: string; effective_to: string | null; source_ref: string };
export type UniverseVersion = { version_id: string; name: string; description: string; asset_scope: string; source_kind: string; source_ref: string; content_hash: string };
export type UniverseDraft = { name: string; description: string; asset_scope: 'stock' | 'etf' | 'mixed'; source_kind: 'manual' | 'index_history'; source_ref: string; members: ManualMember[] };
export type UniverseDetail = { universe: UniverseSummary; versions: UniverseVersion[]; draft: UniverseDraft | null };
export type Capability = { kind: string; ready: boolean; blockers: string[] };
export type Coverage = { coverage: string; membership_coverage?: { status: string }; market_data_coverage?: { status: string; calendar_basis: string; calendar_status: string; calendar_missing_dates: string[]; expected_dates: string[]; symbols: { symbol: string; rows: number; first_date: string | null; last_date: string | null; missing_dates: string[]; unexpected_dates: string[]; duplicate_dates: string[]; invalid_ohlc_dates: string[] }[] }; snapshot?: { snapshot_id: string; sha256: string; created_at: string; first_date: string; last_date: string; selection_rule: string } | null; calendar_snapshot?: { source: string; sha256: string; rows: number; first_date: string; last_date: string; file: string } | null; pit_status: string; capabilities: Capability[]; gaps: { start?: string | null; end?: string | null; source_ref: string; symbol?: string; reason: string; missing_dates?: string[]; unexpected_dates?: string[]; duplicate_dates?: string[]; invalid_ohlc_dates?: string[] }[]; requested_range?: { start: string | null; end: string | null } };
export type ResolvedMember = { instrument: Instrument; effective_from: string; effective_to: string | null; published_at: string | null; source_ref: string };
export type MemberSnapshot = { as_of: string; members: ResolvedMember[]; pit_status: string; coverage: string; warnings: string[]; membership_hash: string };
export type SavedFactorReport = Record<string, unknown>;
export type FactorReportRow = { id: string; signal: string; date: string; universeId: string | null; versionId: string | null; pit: string; coverage: string; raw: SavedFactorReport };

export const pct = (n: number) => `${(n * 100).toFixed(2)}%`;
export const scopeLabel = (scope: string) => ({ etf: 'ETF', stock: '股票', mixed: '混合' }[scope] ?? scope);
export const sourceLabel = (source: string) => source === 'manual' ? '手工名单' : '历史指数';
export const lifecycleLabel = (item: UniverseSummary) => item.status === 'archived' ? '已归档' : item.version_count ? '已发布' : '草稿';
export async function api<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(url, { ...init, headers: { 'Content-Type': 'application/json', ...init?.headers } });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body?.error?.message ?? `请求失败（${response.status}）`);
  return body as T;
}

export function Panel({ title, children }: { title: string; children: ReactNode }) { return <section className="panel"><h2>{title}</h2>{children}</section>; }

export function Header() { return <header><b>Prajna Quant</b><nav><a href="/">策略绩效</a><a href="/factors">因子研究</a><a href="/universes">Universe</a><a href="/market">证券行情</a></nav></header>; }

export class PageErrorBoundary extends Component<{ children: ReactNode }, { error: string | null }> {
  state = { error: null as string | null };
  static getDerivedStateFromError(error: Error) { return { error: error.message }; }
  componentDidCatch(error: Error) { console.error('Page render failed:', error); }
  render() { return this.state.error ? <><Header /><main><ErrorBox>页面渲染失败：{this.state.error}。请检查服务返回数据并刷新页面。</ErrorBox></main></> : this.props.children; }
}

export function useSort<T>(rows: T[], initial: keyof T) { const [sort, setSort] = useState<{ key: keyof T; desc: boolean }>({ key: initial, desc: true }); const sorted = [...rows].sort((a, b) => { const x = a[sort.key], y = b[sort.key]; return (typeof x === 'number' && typeof y === 'number' ? x - y : String(x ?? '').localeCompare(String(y ?? ''))) * (sort.desc ? -1 : 1); }); const by = (key: keyof T) => setSort((s) => ({ key, desc: s.key === key ? !s.desc : true })); return { sorted, by, sort }; }

export function ErrorBox({ children }: { children: ReactNode }) { return <div className="notice" role="alert">{children}</div>; }


export function resultRow(item: SavedFactorReport, index: number): FactorReportRow {
  const report = (item.report && typeof item.report === 'object' ? item.report : item) as Record<string, unknown>; const definition = (report.definition ?? {}) as Record<string, unknown>; const universe = (item.universe ?? report.universe) as Record<string, unknown> | undefined;
  return { id: String(item.report_id ?? item.id ?? index), signal: String(definition.display_name ?? definition.key ?? '因子报告'), date: String(item.created_at ?? item.generated_at ?? '已保存'), universeId: typeof universe?.universe_id === 'string' ? universe.universe_id : null, versionId: typeof universe?.version_id === 'string' ? universe.version_id : null, pit: String(universe?.pit_status ?? 'unknown'), coverage: String(universe?.coverage ?? 'unverified'), raw: item };
}

export function SavedResultTools({ kind }: { kind: 'strategy' | 'factor' }) {
  const [universes, setUniverses] = useState<UniverseSummary[]>([]); const [universeId, setUniverseId] = useState(''); const [versionId, setVersionId] = useState(''); const [versions, setVersions] = useState<UniverseVersion[]>([]); const [rows, setRows] = useState<(StrategySummary | FactorReportRow)[]>([]); const [loading, setLoading] = useState(false); const [error, setError] = useState('');
  useEffect(() => { api<{ items: UniverseSummary[] }>('/api/v1/universes?limit=200').then((r) => setUniverses(r.items)).catch((e) => setError(`Universe 列表加载失败：${e.message}`)); }, []);
  useEffect(() => { if (!universeId) { setVersions([]); setVersionId(''); return; } api<UniverseDetail>(`/api/v1/universes/${encodeURIComponent(universeId)}`).then((r) => { setVersions(r.versions); setVersionId(''); }).catch((e) => setError(`版本列表加载失败：${e.message}`)); }, [universeId]);
  const query = useMemo(() => { if (!universeId || !versionId) return ''; return `?universe_id=${encodeURIComponent(universeId)}&version_id=${encodeURIComponent(versionId)}`; }, [universeId, versionId]);
  const loadResults = async () => { setError(''); setLoading(true); try { if (universeId && !versionId) { setRows([]); return; } if (kind === 'strategy') { const response = await api<StrategySummary[]>(`/api/experiments${query}`); setRows(response); } else { const response = await api<{ items: SavedFactorReport[] }>(`/api/v1/factor-reports${query}`); setRows(response.items.map(resultRow)); } } catch (e) { setError(`已保存结果加载失败：${(e as Error).message}`); } finally { setLoading(false); } };
  useEffect(() => { void loadResults(); // Filter changes only read saved result APIs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kind, query]);
  const selected = universes.find((u) => u.universe_id === universeId); const selectedVersion = versions.find((v) => v.version_id === versionId);
  return <section className="universe-tools"><div className="tool-head"><div><h2>筛选已保存{kind === 'strategy' ? '策略' : '因子'}结果</h2><p>筛选只读取已保存结果，与发起新运行相互独立。未带 Universe 身份的旧结果标记为未知。</p></div><span className="status-tag">服务端数据</span></div><div className="tool-columns"><div className="tool-box"><h3>结果筛选</h3><label>Universe<select value={universeId} onChange={(e) => { setUniverseId(e.target.value); setVersionId(''); }}><option value="">全部 Universe（含未知旧结果）</option>{universes.map((u) => <option key={u.universe_id} value={u.universe_id}>{u.name} · {lifecycleLabel(u)}</option>)}</select></label><label>不可变版本<select value={versionId} onChange={(e) => setVersionId(e.target.value)} disabled={!universeId}><option value="">{universeId ? '选择具体版本以筛选' : '全部版本'}</option>{versions.map((v, index) => <option key={v.version_id} value={v.version_id}>版本 {index + 1}</option>)}</select></label>{selected && <p className="hint">{selected.name} · {selectedVersion ? `${sourceLabel(selectedVersion.source_kind)} / ${scopeLabel(selectedVersion.asset_scope)}` : '选择版本后调用精确身份筛选 API。'}</p>}{error && <ErrorBox>{error}</ErrorBox>}{loading ? <div className="empty-state">正在读取已保存结果…</div> : universeId && !versionId ? <div className="empty-state">请选择版本；服务端要求 universe_id 与 version_id 成对精确匹配。</div> : rows.length === 0 ? <div className="empty-state">没有匹配的已保存结果。</div> : <div className="result-list">{kind === 'strategy' ? (rows as StrategySummary[]).map((x) => <div className="result-row" key={x.experiment_id}><b><a href={`/strategies/${x.experiment_id}`}>{x.name}</a></b><small>{x.created_at.slice(0, 10)} · {x.universe_id ? `${x.universe_name ?? 'Universe'} · ${x.universe_pit_status ?? 'unknown'} / ${x.universe_coverage ?? 'unverified'}` : 'Universe 未知（旧实验）'}</small></div>) : (rows as FactorReportRow[]).map((x) => <div className="result-row" key={x.id}><b>{x.signal}</b><small>{x.date} · {x.universeId ? `${universes.find((u) => u.universe_id === x.universeId)?.name ?? 'Universe'} · ${x.pit} / ${x.coverage}` : 'Universe 未知（旧报告）'}</small></div>)}</div>}</div><div className="tool-box run-box"><h3>发起新运行</h3><p>新运行需要异步作业创建与状态查询 API。</p><div className="blocked"><strong>当前不可用</strong><span>服务端尚未实现 /api/v1/runs 与作业状态存储；此操作保持禁用，不会用前端模拟回测。</span></div><button className="primary" type="button" disabled title="等待服务端运行 API 实现">发起新运行（API 未实现）</button></div></div></section>;
}
