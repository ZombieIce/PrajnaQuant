import { useEffect, useState } from 'react';
import { api, Header, Panel, ErrorBox } from '../shared';
import { Chart } from '../Chart';
type Pool = { id: string; name: string; versions: { version: number; members: string[] }[] };
type Adjusted = { version: string; instrument_id: string; method_version: string };
type Row = { instrument: string; date: string; value: number | null; status: string; available_at_ns: string | null };
type Factor = { definition: { kind: string; version: string; params: { n: number } }; rows: Row[]; distribution: { low: number; high: number; count: number }[]; status_counts: Record<string, number> };
type Report = { id: string; report: { factors: Factor[]; members: string[]; sources: unknown; sample_assumption: string; semantics: string; direction: string; request: unknown } };
type Reply = { data: Report };
type Kind = 'momentum' | 'volatility' | 'trend_filter_strict';
// Momentum stays the legacy request shape (no factor field) so old report identities and saved work remain reusable.
const factorKinds: { value: Kind; label: string; windows: string }[] = [
  { value: 'momentum', label: '动量', windows: '20,60,120' },
  { value: 'volatility', label: '波动率', windows: '20,60' },
  { value: 'trend_filter_strict', label: '均线趋势（严格 close > MA）', windows: '20,60' },
];
const factorLabel = (kind: string) => factorKinds.find(k => k.value === kind)?.label ?? kind;
const endpoint = '/api/v1/momentum-research';
export default function MomentumPage() {
  const [datasets, setDatasets] = useState<{ dsv: string }[]>([]);
  const [dsv, setDsv] = useState('');
  const [instruments, setInstruments] = useState<string[]>([]);
  const [versions, setVersions] = useState<Adjusted[]>([]);
  const [pools, setPools] = useState<Pool[]>([]);
  const [choice, setChoice] = useState('');
  const [adjusted, setAdjusted] = useState<Record<string, string>>({});
  const [kind, setKind] = useState<Kind>('momentum');
  const [windows, setWindows] = useState('20,60,120');
  const [report, setReport] = useState<Report | null>(null);
  const [recent, setRecent] = useState<{ id: string }[]>([]);
  const [error, setError] = useState('');
  const [task, setTask] = useState<{ task_id: string; status: string; stage: string; task_type: string; result?: { report_id: string }; error?: { message: string } } | null>(null);
  const [tasks, setTasks] = useState<NonNullable<typeof task>[]>([]);
  const [busy, setBusy] = useState(false);
  const reload = () => api<{ data: { reports: typeof recent } }>(endpoint).then(v => setRecent(v.data.reports));
  useEffect(() => {
    api<{ data: { datasets: typeof datasets } }>('/api/v1/datasets').then(v => setDatasets(v.data.datasets)).catch(e => setError(e.message));
    api<{ data: { pools: Pool[] } }>('/api/v1/research-pools').then(v => setPools(v.data.pools)).catch(e => setError(e.message));
    reload().catch(e => setError(e.message));
    api<{ items: NonNullable<typeof task>[] }>('/api/v1/tasks/experiments').then(v => setTasks(v.items.filter(t => t.task_type === 'prajna_momentum'))).catch(e => setError(e.message));
  }, []);
  useEffect(() => {
    setInstruments([]); setVersions([]); setChoice(''); setAdjusted({});
    if (!dsv) return;
    const controller = new AbortController();
    api<{ data: { instruments: { instrument_id: string; kind: string }[]; adjusted_series: { versions: Adjusted[] } } }>(`/api/v1/datasets/${encodeURIComponent(dsv)}`, { signal: controller.signal })
      .then(v => { if (!controller.signal.aborted) { setInstruments(v.data.instruments.filter(i => i.kind === 'etf').map(i => i.instrument_id)); setVersions(v.data.adjusted_series.versions); } })
      .catch(e => { if (!controller.signal.aborted) setError(e.message); });
    return () => controller.abort();
  }, [dsv]);
  const selectedPool = pools.flatMap(p => p.versions.map(v => ({ ...v, id: p.id }))).find(v => `pool:${v.id}:${v.version}` === choice);
  const members = selectedPool?.members ?? (choice ? [choice] : []);
  const open = async (id: string) => {
    setError(''); setReport(null); setBusy(true);
    try { const v = await api<Reply>(endpoint, { method: 'POST', body: JSON.stringify({ op: 'read', id }) }); setReport(v.data); }
    catch (e) { setError((e as Error).message); } finally { setBusy(false); }
  };
  useEffect(() => {
    if (!task || !['queued', 'running'].includes(task.status)) return;
    const timer = setTimeout(() => {
      api<NonNullable<typeof task>>(`/api/v1/tasks/experiments/${task.task_id}`).then(async value => {
        setTask(value);
        if (value.status === 'succeeded' && value.result?.report_id) { await open(value.result.report_id); await reload(); }
        else if (['failed', 'interrupted'].includes(value.status)) { setBusy(false); setError(value.error?.message ?? value.status); }
      }).catch(e => { setBusy(false); setError(e.message); });
    }, 400);
    return () => clearTimeout(timer);
  }, [task]);
  const reopenTask = async (value: NonNullable<typeof task>) => {
    const current = await api<NonNullable<typeof task>>(`/api/v1/tasks/experiments/${value.task_id}`);
    setTask(current); setReport(null); setError('');
    if (current.status === 'succeeded' && current.result?.report_id) await open(current.result.report_id);
    else if (['failed', 'interrupted'].includes(current.status)) setError(current.error?.message ?? current.status);
  };
  const submit = async () => {
    setError(''); setReport(null); setBusy(true);
    try {
      const ns = windows.split(/[,，\s]+/).filter(Boolean).map(Number);
      if (!ns.length || ns.some(n => !Number.isSafeInteger(n) || n < 1)) throw new Error('窗口必须为正整数 Session');
      const request = { dsv, ...(selectedPool ? { pool: { id: selectedPool.id, version: selectedPool.version } } : { instrument: choice }), windows: ns,
        adjusted: Object.fromEntries(members.map(m => [m, adjusted[m] ?? ''])), ...(kind === 'momentum' ? {} : { factor: kind }) };
      const v = await api<{ task: NonNullable<typeof task> }>(endpoint, { method: 'POST', body: JSON.stringify({ op: 'submit', request, idempotency_key: crypto.randomUUID() }) });
      setTask(v.task); setTasks(previous => [v.task, ...previous.filter(t => t.task_id !== v.task.task_id)]);
    } catch (e) { setError((e as Error).message); } finally { setBusy(false); }
  };
  return <><Header /><main><h1>因子研究</h1><p>新平台 Rust Factor / Cache · 动量 / 波动率 / 严格均线趋势 · 供应商复权研究价 · 收盘可用假设 · 历史发布时间 Unknown</p>
    <Panel title="提交研究">
      <label>Dataset<select aria-label="研究 Dataset" value={dsv} onChange={e => setDsv(e.target.value)}><option value="">选择固定版本</option>{datasets.map(d => <option key={d.dsv}>{d.dsv}</option>)}</select></label>
      <label>证券或已发布研究池<select aria-label="研究对象" value={choice} onChange={e => { setChoice(e.target.value); setAdjusted({}); }}><option value="">选择对象</option>{instruments.map(i => <option key={i}>{i}</option>)}{pools.flatMap(p => p.versions.map(v => <option key={`${p.id}:${v.version}`} value={`pool:${p.id}:${v.version}`}>{p.name} v{v.version}</option>))}</select></label>
      {members.map(m => <label key={m}>{m} 复权版本<select aria-label={`${m} 研究复权版本`} value={adjusted[m] ?? ''} onChange={e => setAdjusted({ ...adjusted, [m]: e.target.value })}><option value="">显式选择版本</option>{versions.filter(v => v.instrument_id === m).map(v => <option key={v.version} value={v.version}>{v.method_version} · {v.version}</option>)}</select></label>)}
      <label>研究因子<select aria-label="研究因子" value={kind} onChange={e => { const next = e.target.value as Kind; setKind(next); setWindows(factorKinds.find(k => k.value === next)!.windows); }}>{factorKinds.map(k => <option key={k.value} value={k.value}>{k.label}</option>)}</select></label>
      <label>窗口 Session<input aria-label="研究窗口" value={windows} onChange={e => setWindows(e.target.value)} /></label>
      <button disabled={busy || !choice || members.some(m => !adjusted[m])} onClick={submit}>{busy ? '处理中…' : '计算并保存研究'}</button>
    </Panel>
    {task && <p data-testid="momentum-task-status">{task.status} · {task.stage}</p>}
    <Panel title="研究任务">{tasks.map(t => <button key={t.task_id} onClick={() => reopenTask(t).catch(e => setError(e.message))}>任务 {t.task_id}</button>)}</Panel>
    {error && <ErrorBox>{error}</ErrorBox>}
    <Panel title="已保存研究">{recent.map(r => <button disabled={busy} key={r.id} onClick={() => open(r.id)}>打开 {r.id}</button>)}</Panel>
    {report && <><p>{report.report.sample_assumption}</p><p data-testid="factor-semantics">语义 {report.report.semantics} · 方向 {report.report.direction}</p><details><summary>固定输入、来源与可用性假设</summary><pre>{JSON.stringify({ request: report.report.request, sources: report.report.sources }, null, 2)}</pre></details>
      {report.report.factors.map((factor, index) => <Panel key={index} title={`${factorLabel(factor.definition.kind)} ${factor.definition.params.n} Session`}>
        <p data-testid="missing-counts">状态 / 缺失：{JSON.stringify(factor.status_counts)}</p>
        <Chart option={{ tooltip: { trigger: 'axis' }, legend: {}, xAxis: { type: 'category', data: [...new Set(factor.rows.map(r => r.date))] }, yAxis: { type: 'value' }, series: report.report.members.map(m => ({ name: m, type: 'line', connectNulls: false, data: factor.rows.filter(r => r.instrument === m).map(r => r.value) })) }} />
        <Chart option={{ tooltip: {}, xAxis: { type: 'category', data: factor.distribution.map(b => `${b.low.toFixed(4)}–${b.high.toFixed(4)}`) }, yAxis: { type: 'value' }, series: [{ type: 'bar', data: factor.distribution.map(b => b.count) }] }} />
        <table data-testid="momentum-values"><thead><tr><th>证券</th><th>日期</th><th>值</th><th>状态</th><th>可用时刻 ns</th></tr></thead><tbody>{factor.rows.map(r => <tr key={`${r.instrument}:${r.date}`}><td>{r.instrument}</td><td>{r.date}</td><td>{r.value ?? '—'}</td><td>{r.status}</td><td>{r.available_at_ns ?? 'Unknown'}</td></tr>)}</tbody></table>
      </Panel>)}</>}
  </main></>;
}
