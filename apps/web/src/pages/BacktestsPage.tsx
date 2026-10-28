import { useCallback, useEffect, useMemo, useState, type FormEvent } from 'react';
import { Chart } from '../Chart';
import { api, ErrorBox, Header, Panel, pct } from '../shared';

type Envelope<T> = { schema_version: 1; data: T };
type ApiFailure = { code?: string; message?: string; details?: unknown };
type ErrorWithCode = Error & { code?: string; details?: unknown };
type DatasetSummary = { dsv: string; created_at: string; normalizer_version: string; instruments: number; sessions: number; bars: number; venues: string[] };
type Catalog = { datasets: DatasetSummary[]; invalid_manifests: { manifest: string; error: string }[] };
type LifecycleField = { state: string; value: string | null; reason: string | null };
type DatasetInstrument = {
  instrument_id: string; venue: string; kind: string; native_symbol: string; quote_currency: string | null;
  lifecycle: { listing_date: LifecycleField };
  coverage: { bars: number; first_bar: string | null; last_bar: string | null; sources: string[]; bars_without_source: number };
};
type DatasetDetail = { dsv: string; normalizer_version: string; instruments: DatasetInstrument[]; warnings: string[] };
type TaskStatus = 'queued' | 'running' | 'succeeded' | 'failed' | 'interrupted';
type Task = {
  task_id: string; status: TaskStatus; stage: string; created_at: string; updated_at: string; result_level: string;
  error: { code: string; message: string; details?: unknown } | null;
  result: { exp?: string; exe?: string; run_count?: number; failed_count?: number } | null;
};
type Summary = {
  total_return: number | null; annualized_return: number | null; annualized_volatility: number | null;
  sharpe: number | null; max_drawdown: number | null; n_returns: number; mean_return: number | null; std_return: number | null;
  total_turnover: number; total_cost: number; executed_count: number; deferred_count: number; skipped_buy_count: number;
  session_count: number; pending_at_end: boolean; insufficient_sessions: boolean; zero_volatility: boolean;
  assumptions: string[]; availability_assumption: string; return_basis?: string | null;
};
type Position = { quantity: string; market_value: string; price?: string };
type ValuationRow = {
  session_date: string; valuation_point: 'open_after' | 'close'; scope?: string; portfolio_id?: number | string | null; cash: string; equity: string;
  positions?: Record<string, Position>; accounting_verified?: boolean;
};
type SessionRow = { session_date: string; nav: number; net_return?: number };
type ResultRun = {
  run_id: string; status: string; result_level: string; accounting_verified: boolean | null; error: string | null;
  summary: Summary | null;
  series: { sessions?: SessionRow[]; account_valuations?: ValuationRow[] };
  tables: { saved: string[]; not_saved: string[]; not_produced_by_engine: string[] };
};
type ResultData = { exp: string; exe: string; default_result_level: string; runs: ResultRun[] };

const statusLabel: Record<TaskStatus, string> = { queued: '排队中', running: '运行中', succeeded: '已完成', failed: '失败', interrupted: '已中断' };
const isTerminal = (status: TaskStatus) => status === 'succeeded' || status === 'failed' || status === 'interrupted';
const shortId = (id: string) => (id.length > 20 ? `${id.slice(0, 18)}…` : id);
const detailsJson = (value: unknown) => (value === null || value === undefined ? '' : JSON.stringify(value, null, 2));

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

export default function BacktestsPage() {
  const [catalog, setCatalog] = useState<Catalog | null>(null);
  const [catalogError, setCatalogError] = useState<Error | null>(null);
  const [dsv, setDsv] = useState('');
  const [detail, setDetail] = useState<DatasetDetail | null>(null);
  const [detailError, setDetailError] = useState<Error | null>(null);
  const [instrumentId, setInstrumentId] = useState('');
  const [tasks, setTasks] = useState<Task[]>([]);
  const [tasksError, setTasksError] = useState<Error | null>(null);
  const [taskId, setTaskId] = useState(() => new URLSearchParams(location.search).get('task') ?? '');
  const [task, setTask] = useState<Task | null>(null);
  const [taskError, setTaskError] = useState<Error | null>(null);
  const [result, setResult] = useState<ResultData | null>(null);
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
    return () => controller.abort();
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    setDetail(null); setDetailError(null); setInstrumentId('');
    if (!dsv) return;
    api<Envelope<DatasetDetail>>(`/api/v1/datasets/${encodeURIComponent(dsv)}`, { signal: controller.signal })
      .then((response) => { if (!controller.signal.aborted) setDetail(response.data); })
      .catch((error: Error) => { if (!controller.signal.aborted) setDetailError(error); });
    return () => controller.abort();
  }, [dsv]);

  const loadTasks = useCallback(async (signal?: AbortSignal) => {
    try {
      const response = await api<{ items: Task[] }>('/api/v1/tasks/experiments', { signal });
      if (!signal?.aborted) { setTasks(response.items); setTasksError(null); }
    } catch (error) {
      if (!signal?.aborted) setTasksError(error as Error);
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
        const current = await call<Task>(`/api/v1/tasks/experiments/${encodeURIComponent(taskId)}`);
        if (cancelled) return;
        setTask(current); setTaskError(null);
        if (current.status === 'succeeded') {
          try {
            const response = await call<Envelope<ResultData>>(`/api/v1/tasks/experiments/${encodeURIComponent(taskId)}/result`);
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

  const etfs = useMemo(() => detail?.instruments.filter((item) => item.kind === 'etf') ?? [], [detail]);
  const selected = etfs.find((item) => item.instrument_id === instrumentId);
  const selectedVersion = catalog?.datasets.find((item) => item.dsv === dsv);

  const submit = async (event: FormEvent) => {
    event.preventDefault(); setSubmitError(null); setBusy(true);
    try {
      const response = await call<{ task: Task; created: boolean }>('/api/v1/tasks/selections', {
        method: 'POST',
        body: JSON.stringify({ idempotency_key: crypto.randomUUID(), dsv, instrument: instrumentId, strategy: 'buy_and_hold', result_level: 'standard' }),
      });
      setTaskId(response.task.task_id); setTask(response.task); setTaskError(null);
      setResult(null); setResultError(null);
      history.replaceState(null, '', `/backtests?task=${encodeURIComponent(response.task.task_id)}`);
      setListVersion((value) => value + 1);
      document.getElementById('task-status')?.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
    } catch (error) {
      setSubmitError(error as Error);
    } finally {
      setBusy(false);
    }
  };

  const openTask = (id: string) => {
    setTaskId(id); setTask(null); setResult(null); setTaskError(null); setResultError(null);
    history.replaceState(null, '', `/backtests?task=${encodeURIComponent(id)}`);
  };

  return <><Header /><main className="backtests-page">
    <div className="page-title-row"><div>
      <h1>单 ETF 回测</h1>
      <p>选择已发布 Dataset 与单只 ETF，经冻结选择入口提交买入持有任务；状态与结果均读取服务端持久记录。浏览器不计算费用、净值或绩效。</p>
    </div></div>

    <Panel title="提交买入持有回测">
      {catalogError && errorDetails(catalogError)}
      {!catalog && !catalogError && <p role="status">正在读取已发布 Dataset 目录…</p>}
      {catalog && <>
        <label>Dataset 版本
          <select aria-label="Dataset 版本" value={dsv} onChange={(event) => { setDsv(event.target.value); setSubmitError(null); }}>
            <option value="">请选择具体已发布版本</option>
            {catalog.datasets.map((item) => <option key={item.dsv} value={item.dsv}>
              {item.created_at} · {item.venues.join('/')} · {item.instruments} 证券 / {item.bars} Bars · {item.dsv}
            </option>)}
          </select>
        </label>
        {!catalog.datasets.length && <p role="status">没有可读取的已发布 Dataset。</p>}
        {catalog.invalid_manifests.length > 0 && <ErrorBox>存在不可读取的版本：{catalog.invalid_manifests.map((item) => item.manifest).join('、')}</ErrorBox>}
      </>}
      {detailError && errorDetails(detailError)}
      {dsv && !detail && !detailError && <p role="status">正在读取固定版本…</p>}
      {detail?.dsv === dsv && <>
        <label>ETF
          <select aria-label="ETF" value={instrumentId} onChange={(event) => setInstrumentId(event.target.value)} disabled={!etfs.length}>
            <option value="">{etfs.length ? '请选择单只 ETF' : '该版本没有 ETF'}</option>
            {etfs.map((item) => <option key={item.instrument_id} value={item.instrument_id}>{item.native_symbol} · {item.venue} · {item.instrument_id}</option>)}
          </select>
        </label>
        {detail.warnings.map((warning, index) => <ErrorBox key={index}>{warning}</ErrorBox>)}
        {selected && <p className="hint">
          上市日：{selected.lifecycle.listing_date.state === 'known' ? selected.lifecycle.listing_date.value : `${selected.lifecycle.listing_date.state} · ${selected.lifecycle.listing_date.reason ?? '未知'}`}
          {' '}· 实际 Bars：{selected.coverage.bars}（{selected.coverage.first_bar ?? '无'} 至 {selected.coverage.last_bar ?? '无'}）
          {' '}· 来源：{selected.coverage.sources.join('、') || '未记录'} · 缺来源 Bars：{selected.coverage.bars_without_source}
        </p>}
        {selectedVersion && <p className="hint">Normalizer：{selectedVersion.normalizer_version} · DSV：<code>{selectedVersion.dsv}</code></p>}
        <p className="hint">Rust 默认参数：本金 1,000,000 CNY、佣金 3bp、最低佣金 5 CNY、买卖滑点各 2bp、税 0；浏览器不覆盖隐含值。提交使用 Standard 级别以持久化 NAV/权益表。</p>
        <form onSubmit={submit}>
          <button type="submit" className="primary" disabled={busy || !dsv || !selected}>提交买入持有回测</button>
          {busy && <span role="status">正在经生产入口冻结选择并提交任务…</span>}
        </form>
      </>}
      {submitError && errorDetails(submitError)}
    </Panel>

    <section id="task-status" className="run-status" data-testid="task-status">
      {!taskId && <div className="empty-state">尚未提交或选择任务。可从下方最近任务重开已提交任务。</div>}
      {taskId && !task && !taskError && <p role="status">正在读取任务 {shortId(taskId)}…</p>}
      {task && <>
        <span className={`status-tag status-${task.status}`}>{statusLabel[task.status]}</span>
        <strong>{task.stage}</strong>
        <span className="hint">任务 {shortId(task.task_id)} · 级别 {task.result_level} · 创建 {task.created_at} · 更新 {task.updated_at}</span>
        {!isTerminal(task.status) && <span role="status">状态来自持久任务记录；页面每秒重新读取，不显示伪造进度。</span>}
        {task.status === 'succeeded' && !result && !resultError && <span role="status">任务已完成，正在读取持久结果…</span>}
        {task.status === 'succeeded' && <p className="hint">结果身份：exp {task.result?.exp ?? '缺失'} · exe {task.result?.exe ?? '缺失'} · Run {task.result?.run_count ?? '未知'} · 失败 {task.result?.failed_count ?? '未知'}</p>}
      </>}
      {taskError && <>{errorDetails(taskError)}<button type="button" className="secondary" onClick={() => setReadVersion((value) => value + 1)}>重新读取任务</button></>}
      {(task?.status === 'failed' || task?.status === 'interrupted') && <ErrorBox>
        <strong>{statusLabel[task.status]}：未产生可读取的持久结果</strong>
        {task.error ? <p><code>{task.error.code}</code>：{task.error.message}</p> : <p>任务未保存错误详情，不能据此推断成功或无交易。</p>}
        {detailsJson(task.error?.details) ? <details><summary>本地任务错误详情</summary><pre>{detailsJson(task.error?.details)}</pre></details> : null}
      </ErrorBox>}
    </section>

    {task?.status === 'succeeded' && resultError && <Panel title="持久结果不可用">
      {errorDetails(resultError)}
      <p>任务已完成但结果读取失败；页面不展示替代数值、零交易或伪造成功。</p>
      <button type="button" className="secondary" onClick={() => setReadVersion((value) => value + 1)}>重新读取结果</button>
    </Panel>}
    {task?.status === 'succeeded' && result && <>
      {((task.result?.failed_count ?? 0) > 0 || result.runs.some((run) => run.status !== 'completed' || run.summary === null)) &&
        <ErrorBox><strong>任务完成但存在失败/未产生 Summary 的 Run</strong><p>失败 Run 不显示为成功、零交易或零值；以下逐 Run 展示原始状态与错误。</p></ErrorBox>}
      <p className="hint">结果读取自 <code>GET /api/v1/tasks/experiments/{task.task_id}/result</code>，Summary 与 NAV/权益序列为 Rust 已持久化值；默认级别 {result.default_result_level}。</p>
      {result.runs.map((run) => <RunResult key={run.run_id} run={run} />)}
    </>}

    <Panel title="最近任务">
      <button type="button" className="secondary" onClick={() => setListVersion((value) => value + 1)}>刷新任务列表</button>
      {tasksError && errorDetails(tasksError)}
      {!tasksError && tasks.length === 0 && <div className="empty-state">暂无已提交任务。</div>}
      <div className="result-list">{tasks.slice(0, 20).map((item) => <div className="result-row" key={item.task_id}>
        <b><span className={`status-tag status-${item.status}`}>{statusLabel[item.status]}</span> {item.stage}</b>
        <small>{item.created_at} · {item.result_level} · {item.task_id}</small>
        {item.error && <small className="job-error">{item.error.code}：{item.error.message}</small>}
        <button type="button" className="secondary" onClick={() => openTask(item.task_id)} disabled={item.task_id === taskId}>查看此任务</button>
      </div>)}</div>
    </Panel>
  </main></>;
}

function RunResult({ run }: { run: ResultRun }) {
  const valuations = useMemo(() => (run.series.account_valuations ?? []).slice().sort((left, right) =>
    left.session_date.localeCompare(right.session_date) || left.valuation_point.localeCompare(right.valuation_point)), [run]);
  const sessions = run.series.sessions ?? [];
  const closeRows = valuations.filter((row) => row.valuation_point === 'close');
  const equityPoints = (closeRows.length ? closeRows : []).map((row) => ({ date: row.session_date, value: Number(row.equity) }))
    .filter((point) => Number.isFinite(point.value));
  const navPoints = sessions.map((row) => ({ date: row.session_date, value: row.nav })).filter((point) => Number.isFinite(point.value));
  const chart = equityPoints.length ? { title: '账户权益（CNY，收盘估值）', points: equityPoints }
    : navPoints.length ? { title: '净值', points: navPoints } : null;
  const summary = run.summary;
  const unavailable = summary?.insufficient_sessions ? '不可计算（有效收益样本不足 2）'
    : summary?.zero_volatility ? '不可计算（收益波动为零）' : '不可计算';
  const positionText = (positions: Record<string, Position> | undefined) => {
    const entries = Object.entries(positions ?? {});
    return entries.length ? entries.map(([id, position]) => Object.keys(positions ?? {}).length === 1 ? position.quantity : `${id} ${position.quantity}`).join('；') : '无持仓';
  };
  return <Panel title={`RUN ${shortId(run.run_id)} · ${run.status}`}>
    <div className="run-status">
      <span className={`status-tag ${run.status === 'completed' ? 'status-succeeded' : 'status-failed'}`}>{run.status === 'completed' ? 'Run 已完成' : `Run ${run.status}`}</span>
      <span>结果级别 {run.result_level}</span>
      <span>账本核验：{run.accounting_verified === null ? '未记录（该引擎无此标记）' : run.accounting_verified ? '已核验' : '未通过'}</span>
    </div>
    {run.error && <ErrorBox>Run 错误：{run.error}</ErrorBox>}
    <p className="hint">已保存表：{run.tables.saved.join('、') || '无'} · 未保存：{run.tables.not_saved.join('、') || '无'} · 引擎不产生：{run.tables.not_produced_by_engine.join('、') || '无'}</p>
    {!summary ? <ErrorBox>该 Run 没有 Summary；不显示为成功、零交易或零值。</ErrorBox> : <>
      <section className="metrics" data-testid="result-metrics">
        <Metric label="累计收益" value={summary.total_return === null ? unavailable : pct(summary.total_return)} />
        <Metric label="年化收益" value={summary.annualized_return === null ? unavailable : pct(summary.annualized_return)} />
        <Metric label="年化波动" value={summary.annualized_volatility === null ? unavailable : pct(summary.annualized_volatility)} />
        <Metric label="Sharpe" value={summary.sharpe === null ? unavailable : summary.sharpe.toFixed(3)} />
        <Metric label="最大回撤" value={summary.max_drawdown === null ? unavailable : pct(summary.max_drawdown)} />
        <Metric label="收益样本 / Session" value={`${summary.n_returns} / ${summary.session_count}`} />
      </section>
      <p className="hint">成交 {summary.executed_count} · 延期 {summary.deferred_count} · 跳过买单 {summary.skipped_buy_count} · 累计成本率 {pct(summary.total_cost)} · 换手 {pct(summary.total_turnover)} · 收益口径 {summary.return_basis ?? '未记录'}</p>
      {summary.pending_at_end && <ErrorBox>期末仍有 pending 目标/风险退出：最后交易日产生的待执行目标未成交，不代表已清仓。</ErrorBox>}
      {summary.insufficient_sessions && <p className="hint">有效收益样本不足 2，年化收益、波动与 Sharpe 不可计算；未以 0 代替。</p>}
      {summary.zero_volatility && <p className="hint">收益波动为零，Sharpe 不可计算；未以 0 代替。</p>}
      <details><summary>收益假设与可用时刻</summary><p>{summary.availability_assumption}</p><ul>{(summary.assumptions ?? []).map((item, index) => <li key={index}>{item}</li>)}</ul></details>
    </>}
    {chart ? <Chart option={{
      tooltip: { trigger: 'axis', valueFormatter: (value) => Number(value).toFixed(4) },
      xAxis: { type: 'category', data: chart.points.map((point) => point.date), boundaryGap: false },
      yAxis: { type: 'value', scale: true, name: chart.title },
      series: [{ type: 'line', showSymbol: false, name: chart.title, data: chart.points.map((point) => point.value), color: '#2563eb' }],
    }} /> : <div className="empty-state">该 Run 未保存 NAV/权益序列表；不显示零交易或伪造曲线。</div>}
    {valuations.length > 0 && <div className="table-wrap"><table data-testid="valuation-rows">
      <thead><tr><th>日期</th><th>估值点</th><th>现金（CNY）</th><th>权益（CNY）</th><th>持仓数量</th></tr></thead>
      <tbody>{valuations.map((row) => <tr key={`${row.session_date}-${row.valuation_point}-${row.scope ?? ''}-${row.portfolio_id ?? ''}`}>
        <td>{row.session_date}</td><td>{row.valuation_point}</td><td>{row.cash}</td><td>{row.equity}</td><td>{positionText(row.positions)}</td>
      </tr>)}</tbody>
    </table></div>}
    {valuations.length === 0 && sessions.length > 0 && <div className="table-wrap"><table data-testid="session-rows">
      <thead><tr><th>日期</th><th>净值</th><th>净收益</th></tr></thead>
      <tbody>{sessions.map((row) => <tr key={row.session_date}><td>{row.session_date}</td><td>{row.nav}</td><td>{row.net_return ?? '未保存'}</td></tr>)}</tbody>
    </table></div>}
  </Panel>;
}

function Metric({ label, value }: { label: string; value: string }) {
  return <div className="metric"><small>{label}</small><strong>{value}</strong></div>;
}
