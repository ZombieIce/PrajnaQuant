import { useCallback, useEffect, useMemo, useState, type FormEvent } from 'react';
import { Chart } from '../Chart';
import { api, ErrorBox, Header, Panel, pct } from '../shared';

type Envelope<T> = { schema_version: 1; data: T };
type ApiFailure = { code?: string; message?: string; details?: unknown };
type ErrorWithCode = Error & { code?: string; details?: unknown };
type DatasetSummary = { dsv: string; created_at: string; instruments: number; bars: number; venues: string[] };
type Catalog = { datasets: DatasetSummary[]; invalid_manifests: { manifest: string; error: string }[] };
type Adjusted = { version: string; instrument_id: string; method_version: string };
type DatasetInstrument = { instrument_id: string; kind: string; native_symbol: string; venue: string };
type DatasetDetail = { dsv: string; instruments: DatasetInstrument[]; adjusted_series?: { versions: Adjusted[] }; warnings: string[] };
type ActionState = 'covered' | 'no_events' | 'missing_facts' | 'unknown';
type ActionScope = { instrument_id: string; state: ActionState; start: string; end: string; reason: string };
type ActionVersion = { version: string; observed_at: string; scopes: ActionScope[] };
type ActionsCatalog = { versions: ActionVersion[]; invalid_versions: unknown[] };
type PoolVersion = { version: number; universe: string; members: string[] };
type Pool = { id: string; name: string; versions: PoolVersion[] };
type PoolRegistry = { schema_version: 1; data: { pools: Pool[] } };
type TaskStatus = 'queued' | 'running' | 'succeeded' | 'failed' | 'interrupted';
type Task = { task_id: string; task_type: string; status: TaskStatus; stage: string; created_at: string; updated_at: string; result_level: string;
  error: { code: string; message: string; details?: unknown } | null;
  result: { strategy?: { exp?: string; exe?: string }; benchmark?: { exp?: string; exe?: string } } | null };
type Summary = { total_return: number | null; annualized_return: number | null; annualized_volatility: number | null;
  sharpe: number | null; max_drawdown: number | null; total_cost: number; executed_count: number;
  deferred_count: number; session_count: number; pending_at_end: boolean; insufficient_sessions: boolean;
  zero_volatility: boolean; return_basis?: string | null; corporate_action_completeness?: { is_complete: boolean; reasons: string[] } | null };
type ValuationRow = { session_date: string; valuation_point: 'open_after' | 'close'; cash: string; equity: string; positions?: Record<string, { quantity: string }> };
type ResultRun = { run_id: string; status: string; accounting_verified: boolean | null; error: string | null; summary: Summary | null; series: { account_valuations?: ValuationRow[] } };
type RoleResult = { role: string; result_level: string; kind: string; definition: Record<string, unknown>; exp: string; exe: string; runs: ResultRun[] };
type Comparability = { semantic_version: string; strategy_kind: string; benchmark_kind: string; dsv: string; universe: string;
  venues: string | string[] | null; initial_cash: string; minimum_commission: string; costs: Record<string, number>;
  sessions_per_year: number; adjusted_versions: Record<string, string>; evaluation: { start: string; end: string; warmup_sessions: number; common_start: string; common_end: string };
  corporate_actions: { version: string; dividend_tax_rate: string }; quantity_price_rules: { instrument_id: string; lot_size: string; price_increment: string; size_increment: string }[];
  return_basis: string; reinvestment_calendar: { policy: 'strategy_rebalance' | 'none'; decision_dates: string[] }; checks: string[] };
type Verification = { comparable: boolean; reasons: string[]; window: { start: string; end: string; session_count: number }; role_identities_verified: boolean };
type ComparisonResult = { semantic_version: string; comparability: Comparability; verification: Verification; strategy: RoleResult; benchmark: RoleResult };

const statusLabel: Record<TaskStatus, string> = { queued: '排队中', running: '运行中', succeeded: '已完成', failed: '失败', interrupted: '已中断' };
const isTerminal = (status: TaskStatus) => status === 'succeeded' || status === 'failed' || status === 'interrupted';
const shortId = (id: string) => (id.length > 20 ? `${id.slice(0, 18)}…` : id);
const detailsJson = (value: unknown) => (value === null || value === undefined ? '' : JSON.stringify(value, null, 2));
const actionStates: Record<ActionState, string> = { covered: '已覆盖', no_events: '明确无事件', missing_facts: '缺事实', unknown: '未知' };
const reasonText: Record<string, string> = { corporate_actions_incomplete: '公司行动事实不完整，结果不能作为完整总回报结论。', accounting_unverified: '账本恒等式未通过核验。', return_basis_missing: '未记录总回报口径。', corporate_action_version_mismatch: '保存结果的公司行动版本与冻结关联不一致。' };

async function call<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(url, { ...init, headers: { 'Content-Type': 'application/json', ...init?.headers } });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) {
    const failure = (body?.error ?? {}) as ApiFailure;
    const error = new Error(failure.message ?? `请求失败（${response.status}）`) as ErrorWithCode;
    error.code = failure.code ?? 'http_error';
    error.details = failure.details;
    throw error;
  }
  return body as T;
}

const errorDetails = (error: Error | null) => {
  if (!error) return null;
  const typed = error as ErrorWithCode;
  return <ErrorBox>{typed.code ? <><code>{typed.code}</code>：</> : null}{error.message}
    {detailsJson(typed.details) ? <details><summary>本地错误详情</summary><pre>{detailsJson(typed.details)}</pre></details> : null}
  </ErrorBox>;
};

const closeRows = (run?: ResultRun) => (run?.series?.account_valuations ?? []).filter((row) => row.valuation_point === 'close')
  .slice().sort((left, right) => left.session_date.localeCompare(right.session_date));
const positionText = (row: ValuationRow) => Object.entries(row.positions ?? {}).map(([id, position]) => `${id} ${position.quantity}`).join('；') || '无持仓';

export default function ComparisonsPage() {
  const [catalog, setCatalog] = useState<Catalog | null>(null);
  const [catalogError, setCatalogError] = useState<Error | null>(null);
  const [dsv, setDsv] = useState('');
  const [detail, setDetail] = useState<DatasetDetail | null>(null);
  const [detailError, setDetailError] = useState<Error | null>(null);
  const [mode, setMode] = useState<'single' | 'pool'>('single');
  const [instrument, setInstrument] = useState('');
  const [adjusted, setAdjusted] = useState<Record<string, string>>({});
  const [pools, setPools] = useState<Pool[]>([]);
  const [poolId, setPoolId] = useState('');
  const [poolVersion, setPoolVersion] = useState(1);
  const [actions, setActions] = useState<ActionsCatalog | null>(null);
  const [actionsError, setActionsError] = useState<Error | null>(null);
  const [actionsVersion, setActionsVersion] = useState('');
  const [dividendTaxRate, setDividendTaxRate] = useState('0.1');
  const [reinvestment, setReinvestment] = useState<'strategy_rebalance' | 'none'>('strategy_rebalance');
  const [maWindow, setMaWindow] = useState('60');
  const [frequency, setFrequency] = useState('5');
  const [parameterText, setParameterText] = useState('{"list":[{"short":20,"long":60,"vol":20,"w_s":0.5,"w_l":0.5,"w_v":0.0,"trend":null,"top_k":1,"rebalance_every":5}]}');
  const [settings, setSettings] = useState({ initial_cash: '1000000', commission_rate: '0.0003', minimum_commission: '5', buy_slippage_bps: '2', sell_slippage_bps: '2', buy_tax_rate: '0', sell_tax_rate: '0' });
  const [recent, setRecent] = useState<Task[]>([]);
  const [taskId, setTaskId] = useState(() => new URLSearchParams(location.search).get('task') ?? '');
  const [task, setTask] = useState<Task | null>(null);
  const [taskError, setTaskError] = useState<Error | null>(null);
  const [recentError, setRecentError] = useState<Error | null>(null);
  const [result, setResult] = useState<ComparisonResult | null>(null);
  const [resultError, setResultError] = useState<Error | null>(null);
  const [submitError, setSubmitError] = useState<Error | null>(null);
  const [busy, setBusy] = useState(false);
  const [readVersion, setReadVersion] = useState(0);
  const [listVersion, setListVersion] = useState(0);

  useEffect(() => {
    const controller = new AbortController();
    setCatalog(null); setCatalogError(null);
    api<Envelope<Catalog>>('/api/v1/datasets', { signal: controller.signal })
      .then((response) => { if (!controller.signal.aborted) setCatalog(response.data); })
      .catch((error: Error) => { if (!controller.signal.aborted) setCatalogError(error); });
    api<PoolRegistry>('/api/v1/research-pools', { signal: controller.signal })
      .then((response) => { if (!controller.signal.aborted) { setPools(response.data.pools); setPoolId((current) => current || response.data.pools[0]?.id || ''); } })
      .catch((error: Error) => { if (!controller.signal.aborted) setCatalogError(error); });
    api<Envelope<ActionsCatalog>>('/api/v1/corporate-actions', { signal: controller.signal })
      .then((response) => { if (!controller.signal.aborted) setActions(response.data); })
      .catch((error: Error) => { if (!controller.signal.aborted) setActionsError(error); });
    return () => controller.abort();
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    setDetail(null); setDetailError(null); setInstrument(''); setAdjusted({});
    if (!dsv) return () => controller.abort();
    api<Envelope<DatasetDetail>>(`/api/v1/datasets/${encodeURIComponent(dsv)}`, { signal: controller.signal })
      .then((response) => { if (!controller.signal.aborted) setDetail(response.data); })
      .catch((error: Error) => { if (!controller.signal.aborted) setDetailError(error); });
    return () => controller.abort();
  }, [dsv]);

  const selectedPool = pools.find((pool) => pool.id === poolId);
  const poolMembers = useMemo(() => selectedPool?.versions.find((version) => version.version === poolVersion)?.members ?? [], [selectedPool, poolVersion]);
  useEffect(() => { setPoolVersion(selectedPool?.versions.at(-1)?.version ?? 1); }, [poolId]);
  const members = mode === 'single' ? (instrument ? [instrument] : []) : poolMembers;
  const etfs = useMemo(() => detail?.instruments.filter((item) => item.kind === 'etf') ?? [], [detail]);
  const adjustedVersions = detail?.adjusted_series?.versions ?? [];
  const selectedActions = actions?.versions.find((version) => version.version === actionsVersion);
  const actionScopes = members.map((member) => ({ member, scope: selectedActions?.scopes.find((scope) => scope.instrument_id === member) }));
  const parameters = useMemo(() => { try { return JSON.parse(parameterText) as unknown; } catch { return null; } }, [parameterText]);
  const ready = Boolean(dsv && members.length && actionsVersion && members.every((member) => adjusted[member]) && (mode === 'single' || parameters));

  const loadTasks = useCallback(async (signal?: AbortSignal) => {
    try {
      const response = await call<{ items: Task[] }>('/api/v1/tasks/comparisons', { signal });
      if (!signal?.aborted) { setRecent(response.items); setRecentError(null); }
    } catch (error) {
      if (!signal?.aborted) setRecentError(error as Error);
    }
  }, []);
  useEffect(() => {
    const controller = new AbortController();
    void loadTasks(controller.signal);
    return () => controller.abort();
  }, [loadTasks, listVersion]);

  useEffect(() => {
    setTask(null); setResult(null); setTaskError(null); setResultError(null);
    if (!taskId) return;
    let cancelled = false;
    let timer: number | undefined;
    const tick = async () => {
      try {
        const current = await call<Task>(`/api/v1/tasks/comparisons/${encodeURIComponent(taskId)}`);
        if (cancelled) return;
        setTask(current); setTaskError(null);
        if (current.status === 'succeeded') {
          try {
            const response = await call<Envelope<ComparisonResult>>(`/api/v1/tasks/comparisons/${encodeURIComponent(taskId)}/result`);
            if (!cancelled) { setResult(response.data); setResultError(null); }
          } catch (error) {
            if (!cancelled) { setResult(null); setResultError(error as Error); }
          }
          setListVersion((value) => value + 1);
          return;
        }
        setResult(null);
        if (isTerminal(current.status)) { setListVersion((value) => value + 1); return; }
        timer = window.setTimeout(() => { void tick(); }, 1000);
      } catch (error) {
        if (cancelled) return;
        setTask(null); setResult(null); setResultError(null); setTaskError(error as Error);
      }
    };
    void tick();
    return () => { cancelled = true; if (timer !== undefined) window.clearTimeout(timer); };
  }, [taskId, readVersion]);

  const submit = async (event: FormEvent) => {
    event.preventDefault(); setSubmitError(null); setBusy(true);
    try {
      const common = {
        dsv,
        strategy: mode === 'single' ? 'single_etf_timing' : 's2_rotation',
        adjusted: Object.fromEntries(members.map((member) => [member, adjusted[member]])),
        corporate_actions: { version: actionsVersion, dividend_tax_rate: dividendTaxRate },
        settings: {
          ...settings,
          commission_rate: Number(settings.commission_rate),
          buy_slippage_bps: Number(settings.buy_slippage_bps),
          sell_slippage_bps: Number(settings.sell_slippage_bps),
          buy_tax_rate: Number(settings.buy_tax_rate),
          sell_tax_rate: Number(settings.sell_tax_rate),
        },
      };
      const selection = mode === 'single'
        ? { ...common, instrument, parameter_space: { list: [{ params_version: 1, ma_window: Number(maWindow), rebalance_every: Number(frequency) }] } }
        : { ...common, pool: { id: poolId, version: poolVersion }, parameter_space: parameters };
      const response = await call<{ task: Task; created: boolean }>('/api/v1/tasks/comparisons', {
        method: 'POST',
        body: JSON.stringify({ idempotency_key: crypto.randomUUID(), selection, reinvestment_calendar: reinvestment }),
      });
      setTaskId(response.task.task_id); setTask(response.task); setTaskError(null);
      setResult(null); setResultError(null);
      history.replaceState(null, '', `/comparisons?task=${encodeURIComponent(response.task.task_id)}`);
      setListVersion((value) => value + 1);
      document.getElementById('comparison-status')?.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
    } catch (error) {
      setSubmitError(error as Error);
    } finally {
      setBusy(false);
    }
  };

  const openTask = (id: string) => {
    setTaskId(id); setTask(null); setResult(null); setTaskError(null); setResultError(null);
    history.replaceState(null, '', `/comparisons?task=${encodeURIComponent(id)}`);
  };

  return <><Header /><main className="backtests-page">
    <div className="page-title-row"><div>
      <h1>策略—基准对照</h1>
      <p>从正式入口冻结一对策略与对应持有基准：单 ETF 择时对同证券买入持有，池内轮动对同池期初等权持有。基准只由 Rust 从冻结策略派生；页面读取服务端已保存的净值/权益，不计算费用、净值或绩效。</p>
    </div></div>

    <Panel title="提交冻结对照">
      <label>对照模式<select aria-label="对照模式" value={mode} onChange={(event) => { setMode(event.target.value as 'single' | 'pool'); setAdjusted({}); setSubmitError(null); }}>
        <option value="single">单 ETF 择时 vs 同证券买入持有</option><option value="pool">池内轮动 vs 同池期初等权持有</option>
      </select></label>
      {catalogError && errorDetails(catalogError)}
      {!catalog && !catalogError && <p role="status">正在读取已发布 Dataset 目录…</p>}
      {catalog && <>
        <label>Dataset 版本<select aria-label="Dataset 版本" value={dsv} onChange={(event) => { setDsv(event.target.value); setSubmitError(null); }}>
          <option value="">请选择具体已发布版本</option>
          {catalog.datasets.map((item) => <option key={item.dsv} value={item.dsv}>{item.created_at} · {item.venues.join('/')} · {item.instruments} 证券 / {item.bars} Bars · {item.dsv}</option>)}
        </select></label>
        {!catalog.datasets.length && <p role="status">没有可读取的已发布 Dataset。</p>}
      </>}
      {detailError && errorDetails(detailError)}
      {dsv && !detail && !detailError && <p role="status">正在读取固定版本…</p>}
      {detail?.dsv === dsv && <>
        {mode === 'single' ? <label>ETF<select aria-label="ETF" value={instrument} onChange={(event) => { setInstrument(event.target.value); setAdjusted({}); }} disabled={!etfs.length}>
          <option value="">{etfs.length ? '请选择单只 ETF' : '该版本没有 ETF'}</option>
          {etfs.map((item) => <option key={item.instrument_id} value={item.instrument_id}>{item.native_symbol} · {item.venue} · {item.instrument_id}</option>)}
        </select></label> : <>
          <label>研究池<select aria-label="研究池" value={poolId} onChange={(event) => { setPoolId(event.target.value); setAdjusted({}); }}>
            {pools.map((pool) => <option key={pool.id} value={pool.id}>{pool.name}</option>)}
          </select></label>
          <label>固定版本<select aria-label="固定版本" value={poolVersion} onChange={(event) => { setPoolVersion(Number(event.target.value)); setAdjusted({}); }}>
            {selectedPool?.versions.map((version) => <option key={version.version} value={version.version}>版本 {version.version} · {version.members.length} 只</option>)}
          </select></label>
          {selectedPool && <p className="hint">冻结成员：{poolMembers.join('、') || '无'}。基准使用同一冻结池成员的期初等权持有。</p>}
        </>}
        {members.map((member) => <label key={member}>复权版本 {member}<select aria-label={`复权版本 ${member}`} value={adjusted[member] ?? ''} onChange={(event) => { const next = { ...adjusted }; if (event.target.value) next[member] = event.target.value; else delete next[member]; setAdjusted(next); }}>
          <option value="">请选择固定 adjusted version</option>
          {adjustedVersions.filter((version) => version.instrument_id === member).map((version) => <option key={version.version} value={version.version}>{version.method_version} · {version.version}</option>)}
        </select></label>)}
        {detail.warnings.map((warning, index) => <ErrorBox key={index}>{warning}</ErrorBox>)}
        <label>公司行动事实版本<select aria-label="公司行动事实版本" value={actionsVersion} onChange={(event) => setActionsVersion(event.target.value)}>
          <option value="">请选择固定版本</option>
          {actions?.versions.map((version) => <option key={version.version} value={version.version}>{version.observed_at} · {version.scopes.length} 条范围 · {version.version}</option>)}
        </select></label>
        {actionsError && errorDetails(actionsError)}
        {actions && !actions.versions.length && <p role="status">没有可读取的公司行动事实版本。</p>}
        {actionsVersion && members.length > 0 && <p className="hint">行动事实覆盖：{actionScopes.map(({ member, scope }) => `${member} ${scope ? actionStates[scope.state] ?? scope.state : '未声明'}`).join('；')}。缺事实/未知不会被页面补成完整；执行结果会明确标记不完整。</p>}
        <label>股息税率<input aria-label="股息税率" type="number" min="0" max="0.999999999999999999" step="any" required value={dividendTaxRate} onChange={(event) => setDividendTaxRate(event.target.value)} /></label>
        <label>再投资日历<select aria-label="再投资日历" value={reinvestment} onChange={(event) => setReinvestment(event.target.value as 'strategy_rebalance' | 'none')}>
          <option value="strategy_rebalance">按策略调仓 Session 再投资</option><option value="none">不再投资（声明日历为空）</option>
        </select></label>
        <p className="hint">再投资日期由 Rust 从冻结策略的调仓 cadence 和评价窗口派生并在运行前保存；基准不卖出赢家恢复权重，分红发放前应收不用于开盘。策略与基准同本金、同窗口、同成本模型、同行动/复权版本。</p>
        {mode === 'single' ? <>
          <label>均线窗口（Session）<input aria-label="均线窗口" type="number" min="1" step="1" required value={maWindow} onChange={(event) => setMaWindow(event.target.value)} /></label>
          <label>调仓频率（Session）<input aria-label="调仓频率" type="number" min="1" step="1" required value={frequency} onChange={(event) => setFrequency(event.target.value)} /></label>
        </> : <label>参数组合 JSON<textarea aria-label="参数组合 JSON" rows={4} value={parameterText} onChange={(event) => setParameterText(event.target.value)} /></label>}
        {mode === 'pool' && !parameters && <p className="hint">请输入恰好一个 S2 参数组合；多个组合不能与单一份基准形成本票的固定比较。</p>}
        {(Object.entries({ initial_cash: '本金（CNY）', commission_rate: '佣金率', minimum_commission: '最低佣金（CNY）', buy_slippage_bps: '买入滑点（bp）', sell_slippage_bps: '卖出滑点（bp）', buy_tax_rate: '买入税率', sell_tax_rate: '卖出税率' }) as [keyof typeof settings, string][]).map(([key, label]) => <label key={key}>{label}<input aria-label={label} type="number" min={key === 'initial_cash' ? '0.00000001' : '0'} step="any" required value={settings[key]} onChange={(event) => setSettings({ ...settings, [key]: event.target.value })} /></label>)}
        <form onSubmit={submit}>
          <button type="submit" className="primary" disabled={busy || !ready}>{busy ? '正在经生产入口冻结对照…' : '提交策略—基准对照'}</button>
        </form>
        {submitError && errorDetails(submitError)}
      </>}
    </Panel>

    <section id="comparison-status" className="run-status" data-testid="comparison-status">
      {!taskId && <div className="empty-state">尚未提交或选择对照任务。可从下方最近对照重开。</div>}
      {taskId && !task && !taskError && <p role="status">正在读取对照任务 {shortId(taskId)}…</p>}
      {task && <>
        <span className={`status-tag status-${task.status}`}>{statusLabel[task.status]}</span>
        <strong>{task.stage}</strong>
        <span className="hint">任务 {shortId(task.task_id)} · 类型 {task.task_type} · 创建 {task.created_at} · 更新 {task.updated_at}</span>
        {!isTerminal(task.status) && <span role="status">状态来自持久任务记录；页面每秒重新读取，不显示伪造进度。</span>}
        {task.status === 'succeeded' && !result && !resultError && <span role="status">任务已完成，正在读取持久对照结果…</span>}
        {task.status === 'succeeded' && <p className="hint">策略 exp {task.result?.strategy?.exp ?? '缺失'} · 基准 exp {task.result?.benchmark?.exp ?? '缺失'}</p>}
      </>}
      {taskError && <>{errorDetails(taskError)}<button type="button" className="secondary" onClick={() => setReadVersion((value) => value + 1)}>重新读取任务</button></>}
      {(task?.status === 'failed' || task?.status === 'interrupted') && <ErrorBox>
        <strong>{statusLabel[task.status]}：未产生可读取的对照结果</strong>
        {task.error ? <p><code>{task.error.code}</code>：{task.error.message}</p> : <p>任务未保存错误详情，不能据此推断成功或无交易。</p>}
        {detailsJson(task.error?.details) ? <details><summary>本地任务错误详情</summary><pre>{detailsJson(task.error?.details)}</pre></details> : null}
      </ErrorBox>}
    </section>

    {task?.status === 'succeeded' && resultError && <Panel title="持久对照结果不可用">
      {errorDetails(resultError)}
      <p>任务已完成但策略/基准结果读取或核对失败；页面不展示替代数值、零交易或伪造成功。</p>
      <button type="button" className="secondary" onClick={() => setReadVersion((value) => value + 1)}>重新读取对照</button>
    </Panel>}
    {task?.status === 'succeeded' && result && <ComparisonView result={result} />}

    <Panel title="最近对照">
      <button type="button" className="secondary" onClick={() => setListVersion((value) => value + 1)}>刷新对照列表</button>
      {recentError && errorDetails(recentError)}
      {recent.length === 0 && <div className="empty-state">暂无已提交对照。</div>}
      <div className="result-list">{recent.slice(0, 20).map((item) => <div className="result-row" key={item.task_id}>
        <b><span className={`status-tag status-${item.status}`}>{statusLabel[item.status]}</span> {item.stage}</b>
        <small>{item.created_at} · {item.task_id}</small>
        {item.error && <small className="job-error">{item.error.code}：{item.error.message}</small>}
        <button type="button" className="secondary" onClick={() => openTask(item.task_id)} disabled={item.task_id === taskId}>查看此对照</button>
      </div>)}</div>
    </Panel>
  </main></>;
}

function ComparisonView({ result }: { result: ComparisonResult }) {
  const { comparability: c, verification: v, strategy, benchmark } = result;
  const strategyRows = closeRows(strategy.runs[0]);
  const benchmarkRows = closeRows(benchmark.runs[0]);
  const chart = useMemo(() => {
    const dates = [...new Set([...strategyRows, ...benchmarkRows].map((row) => row.session_date))].sort();
    const values = (rows: ValuationRow[]) => new Map(rows.map((row) => [row.session_date, Number(row.equity)]));
    const left = values(strategyRows), right = values(benchmarkRows);
    return { dates, strategy: dates.map((date) => left.get(date) ?? null), benchmark: dates.map((date) => right.get(date) ?? null) };
  }, [strategyRows, benchmarkRows]);
  const cost = c.costs;
  return <div data-testid="comparison-result">
    {!v.comparable && <div data-testid="comparison-verification"><ErrorBox><strong>结果不完整，不能作为完整有效结论</strong>
      <ul>{v.reasons.map((reason) => <li key={reason}>{reasonText[reason] ?? reasonText[reason.split(':').at(-1) ?? ''] ?? reason}</li>)}</ul>
      <p>仍显示已保存的权威数据；未把缺失值填零，也未重置首点或缩短窗口。</p>
    </ErrorBox></div>}
    <Panel title="冻结对照关系">
      <div data-testid="comparison-facts">
        <p>窗口：{c.evaluation.start} ～ {c.evaluation.end}（预热 {c.evaluation.warmup_sessions} Sessions；共同数据 {c.evaluation.common_start} ～ {c.evaluation.common_end}） · 初始本金：{c.initial_cash} CNY · {c.sessions_per_year} Sessions/年</p>
        <p>角色：策略 <code>{c.strategy_kind}</code> 对基准 <code>{c.benchmark_kind}</code> · 收益口径：{c.return_basis === 'total_return' ? '总回报（原始价成交/估值 + 净现金行动）' : c.return_basis} · 行动版本：<code>{c.corporate_actions.version}</code>（税率 {c.corporate_actions.dividend_tax_rate}）</p>
        <p>成本：佣金 {cost.commission_rate} · 最低佣金 {c.minimum_commission} CNY · 买入滑点 {cost.buy_slippage_bps}bp · 卖出滑点 {cost.sell_slippage_bps}bp · 买入税 {cost.buy_tax_rate} · 卖出税 {cost.sell_tax_rate}。策略与基准使用同一成本模型，基准实际成交时计费。</p>
        <p>再投资日历：{c.reinvestment_calendar.policy === 'none' ? '明确声明不再投资（空日历）' : `按策略调仓 Session（${c.reinvestment_calendar.decision_dates.join('、') || '评价窗口内无可用日期'}）`}。分红到账后的下一声明 normal decision 才能再投；应收不提前使用。</p>
        <p>已核对：{c.checks.join('、')} · 身份核对：{v.role_identities_verified ? '策略/基准持久 exp/Run 与冻结定义一致' : '未通过'} · 评价窗口 {v.window.start} ～ {v.window.end}（{v.window.session_count} Sessions）。</p>
      </div>
    </Panel>
    <div className="run-status" data-testid="comparison-metrics">
      <RoleMetrics title={`策略 · ${c.strategy_kind}`} run={strategy.runs[0]} />
      <RoleMetrics title={`基准 · ${c.benchmark_kind}`} run={benchmark.runs[0]} />
    </div>
    {chart.dates.length > 0 ? <Panel title="策略/基准账户权益（CNY，收盘估值）"><div data-testid="comparison-chart">
      <Chart option={{
        tooltip: { trigger: 'axis', valueFormatter: (value) => value === null ? '未保存' : Number(value).toFixed(4) },
        legend: { data: ['策略', '基准'] },
        xAxis: { type: 'category', data: chart.dates, boundaryGap: false },
        yAxis: { type: 'value', scale: true, name: '账户权益（CNY）' },
        series: [{ type: 'line', showSymbol: false, name: '策略', data: chart.strategy, connectNulls: false, color: '#2563eb' }, { type: 'line', showSymbol: false, name: '基准', data: chart.benchmark, connectNulls: false, color: '#dc2626' }],
      }} />
      <p className="hint">曲线直接使用持久 `account_valuations.close.equity`；缺失日期保持空点，不填零、不重置首点。下图列出首末权威估值。</p>
    </div></Panel> : <ErrorBox>未找到策略或基准的收盘权益序列；不显示零交易或伪造曲线。</ErrorBox>}
    <Panel title="首末权威估值">
      <div className="table-wrap"><table data-testid="comparison-valuations">
        <thead><tr><th>角色</th><th>日期</th><th>现金（CNY）</th><th>权益（CNY）</th><th>持仓数量</th></tr></thead>
        <tbody>{[strategy, benchmark].flatMap((role) => {
          const rows = closeRows(role.runs[0]);
          return [rows[0], rows.at(-1)].filter((row): row is ValuationRow => Boolean(row)).map((row) => <tr key={`${role.role}-${row.session_date}`}>
            <td>{role.role === 'strategy' ? '策略' : '基准'}（{role.kind}）</td><td>{row.session_date}</td><td>{row.cash}</td><td>{row.equity}</td><td>{positionText(row)}</td>
          </tr>);
        })}</tbody>
      </table></div>
    </Panel>
  </div>;
}

function RoleMetrics({ title, run }: { title: string; run?: ResultRun }) {
  const summary = run?.summary;
  const unavailable = !summary ? '不可计算（无 Summary）' : summary.insufficient_sessions ? '不可计算（有效收益样本不足 2）'
    : summary.zero_volatility ? '不可计算（收益波动为零）' : '不可计算';
  return <div>
    <strong>{title}</strong>
    {run?.error && <ErrorBox>Run 错误：{run.error}</ErrorBox>}
    {!summary ? <ErrorBox>该角色没有 Summary；不显示为成功、零交易或零值。</ErrorBox> : <>
      <section className="metrics">
        <Metric label="累计收益" value={summary.total_return === null ? unavailable : pct(summary.total_return)} />
        <Metric label="年化收益" value={summary.annualized_return === null ? unavailable : pct(summary.annualized_return)} />
        <Metric label="年化波动" value={summary.annualized_volatility === null ? unavailable : pct(summary.annualized_volatility)} />
        <Metric label="Sharpe" value={summary.sharpe === null ? unavailable : summary.sharpe.toFixed(3)} />
        <Metric label="最大回撤" value={summary.max_drawdown === null ? unavailable : pct(summary.max_drawdown)} />
        <Metric label="累计成本率" value={pct(summary.total_cost)} />
      </section>
      <p className="hint">成交 {summary.executed_count} · 延期 {summary.deferred_count} · Session {summary.session_count} · 收益口径 {summary.return_basis ?? '未记录'} · 账本核验 {run?.accounting_verified === null ? '未记录' : run?.accounting_verified ? '已核验' : '未通过'} · 行动事实 {summary.corporate_action_completeness?.is_complete ? '完整' : '不完整'}</p>
      {summary.pending_at_end && <p className="hint">期末仍有 pending 目标/风险退出；不代表已清仓。</p>}
    </>}
  </div>;
}

function Metric({ label, value }: { label: string; value: string }) {
  return <div className="metric"><small>{label}</small><strong>{value}</strong></div>;
}
