import { useCallback, useEffect, useMemo, useState, type FormEvent } from 'react';
import { api, ErrorBox, scopeLabel, type UniverseDetail, type UniverseSummary } from '../shared';

type Snapshot = { snapshot_id: string; sha256: string; first_date: string; last_date: string; rows: number; symbols: number; execution_status_mode: string; hash_verified_on_submit: boolean };
type Run = { run_id: string; status: 'queued' | 'running' | 'succeeded' | 'failed'; stage: string; created_at: string; updated_at: string; universe_id: string; version_id: string; universe_content_hash: string; snapshot_id: string; snapshot_sha256: string; config_sha256: string; diagnostic: boolean; experiment_id: string | null; error: { code: string; message: string; details?: unknown } | null };
type Preflight = { ready: boolean; diagnostic_allowed: boolean; blockers: { code: string; message: string }[]; universe_content_hash: string | null; snapshot_sha256: string | null };

export default function RunJobsPanel() {
  const [universes, setUniverses] = useState<UniverseSummary[]>([]);
  const [universeId, setUniverseId] = useState('');
  const [versionId, setVersionId] = useState('');
  const [versions, setVersions] = useState<UniverseDetail['versions']>([]);
  const [snapshots, setSnapshots] = useState<Snapshot[]>([]);
  const [snapshotId, setSnapshotId] = useState('');
  const [runs, setRuns] = useState<Run[]>([]);
  const [name, setName] = useState('ETF 轮动诊断');
  const [start, setStart] = useState('');
  const [end, setEnd] = useState('');
  const [lookback, setLookback] = useState(60);
  const [topN, setTopN] = useState(3);
  const [rebalance, setRebalance] = useState(5);
  const [diagnostic, setDiagnostic] = useState(false);
  const [preflight, setPreflight] = useState<Preflight | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [key, setKey] = useState(() => crypto.randomUUID());

  const refreshRuns = useCallback(async () => {
    const response = await api<{ items: Run[] }>('/api/v1/runs');
    setRuns(response.items);
  }, []);

  useEffect(() => {
    Promise.all([
      api<{ items: UniverseSummary[] }>('/api/v1/universes?asset_scope=etf&status=active&limit=200'),
      api<{ items: Snapshot[] }>('/api/v1/research-snapshots'),
      refreshRuns(),
    ]).then(([u, s]) => {
      setUniverses(u.items);
      setSnapshots(s.items);
      const latest = s.items[0];
      if (latest) { setSnapshotId(latest.snapshot_id); setStart(latest.first_date); setEnd(latest.last_date); }
      if (u.items[0]) setUniverseId(u.items[0].universe_id);
    }).catch((e) => setError(e.message));
  }, [refreshRuns]);

  useEffect(() => {
    if (!universeId) { setVersions([]); setVersionId(''); return; }
    api<UniverseDetail>(`/api/v1/universes/${encodeURIComponent(universeId)}`).then((detail) => {
      setVersions(detail.versions);
      setVersionId((current) => detail.versions.some((version) => version.version_id === current) ? current : detail.versions.at(-1)?.version_id ?? '');
    }).catch((e) => setError(e.message));
  }, [universeId]);

  useEffect(() => {
    const pending = runs.some((run) => run.status === 'queued' || run.status === 'running');
    if (!pending) return;
    const timer = window.setInterval(() => { void refreshRuns().catch((e) => setError(e.message)); }, 1200);
    return () => window.clearInterval(timer);
  }, [runs, refreshRuns]);

  const version = useMemo(() => versions.find((item) => item.version_id === versionId), [versions, versionId]);
  const selectedSnapshot = useMemo(() => snapshots.find((item) => item.snapshot_id === snapshotId), [snapshots, snapshotId]);
  const body = () => ({
    kind: 'etf_strategy', universe_id: universeId, version_id: versionId, snapshot_id: snapshotId,
    idempotency_key: key, diagnostic_mode: diagnostic,
    config: {
      name, universe: { universe_id: universeId, version_id: versionId, asset_scope: 'etf' }, start, end,
      initial_cash: 1000000, lot_size: 100,
      strategy: { lookback_days: lookback, top_n: topN, rebalance_every: rebalance },
      research: { forward_days: 5, quantiles: 5, label_method: 'next_open_to_forward_open' },
      costs: { commission_rate: 0.0003, minimum_commission: 5, buy_tax_rate: 0, sell_tax_rate: 0, buy_slippage_bps: 2, sell_slippage_bps: 2 },
    },
  });
  const check = async () => {
    setError(''); setBusy(true); setPreflight(null);
    try { setPreflight(await api<Preflight>('/api/v1/runs/preflight', { method: 'POST', body: JSON.stringify(body()) })); }
    catch (e) { setError((e as Error).message); }
    finally { setBusy(false); }
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault(); setError(''); setBusy(true);
    try {
      await api<{ run: Run; created: boolean }>('/api/v1/runs', { method: 'POST', body: JSON.stringify(body()) });
      await refreshRuns(); setKey(crypto.randomUUID()); setPreflight(null);
    } catch (e) { setError((e as Error).message); }
    finally { setBusy(false); }
  };

  return <section id="run-jobs" className="universe-tools run-jobs">
    <div className="tool-head"><div><h2>ETF 回测作业</h2><p>提交到 Rust 作业队列；作业绑定明确的 Universe 版本和不可变研究快照。可信历史运行当前受数据证据门槛阻断。</p></div><span className="status-tag">Rust runner</span></div>
    <form className="run-form" onSubmit={submit}>
      <label>Universe<select value={universeId} onChange={(e) => { setUniverseId(e.target.value); setPreflight(null); }}><option value="">选择 ETF Universe</option>{universes.map((item) => <option key={item.universe_id} value={item.universe_id}>{item.name} · {item.status}</option>)}</select></label>
      <label>已发布版本<select value={versionId} onChange={(e) => { setVersionId(e.target.value); setPreflight(null); }}><option value="">选择不可变版本</option>{versions.map((item, index) => <option key={item.version_id} value={item.version_id}>版本 {index + 1} · {item.content_hash.slice(0, 12)}</option>)}</select></label>
      <label>固定研究快照<select value={snapshotId} onChange={(e) => { const next = snapshots.find((item) => item.snapshot_id === e.target.value); setSnapshotId(e.target.value); if (next) { setStart(next.first_date); setEnd(next.last_date); } setPreflight(null); }}><option value="">选择固定快照（预检时校验 hash）</option>{snapshots.map((item) => <option key={item.snapshot_id} value={item.snapshot_id}>{item.snapshot_id.slice(0, 8)} · {item.first_date}—{item.last_date} · {item.rows.toLocaleString()} 行</option>)}</select></label>
      <label>实验名称<input value={name} onChange={(e) => setName(e.target.value)} maxLength={120} required /></label>
      <label>开始日期<input type="date" min={selectedSnapshot?.first_date} max={end || selectedSnapshot?.last_date} value={start} onChange={(e) => { setStart(e.target.value); setPreflight(null); }} required /></label>
      <label>结束日期<input type="date" min={start || selectedSnapshot?.first_date} max={selectedSnapshot?.last_date} value={end} onChange={(e) => { setEnd(e.target.value); setPreflight(null); }} required /></label>
      <label>动量窗口<input type="number" min={1} max={1000} value={lookback} onChange={(e) => setLookback(Number(e.target.value))} /></label>
      <label>Top N<input type="number" min={1} max={20} value={topN} onChange={(e) => setTopN(Number(e.target.value))} /></label>
      <label>调仓间隔<input type="number" min={1} max={250} value={rebalance} onChange={(e) => setRebalance(Number(e.target.value))} /></label>
      <div className="run-identity">{version ? <span>{version.name} · {scopeLabel(version.asset_scope)} · content hash {version.content_hash}</span> : <span>需选择已发布 ETF 版本。</span>}{selectedSnapshot && <span>快照 {selectedSnapshot.snapshot_id} · SHA-256 {selectedSnapshot.sha256} · {selectedSnapshot.rows.toLocaleString()} 行</span>}</div>
      <label className="diagnostic-choice"><input type="checkbox" checked={diagnostic} onChange={(e) => { setDiagnostic(e.target.checked); setPreflight(null); }} />我明确选择诊断实验（结果将标记 legacy_bar_only、回溯静态名单、零分红、原始价格）</label>
      <div className="run-actions"><button className="secondary" type="button" disabled={busy || !version || !selectedSnapshot} onClick={() => void check()}>检查运行门槛</button><button className="primary" type="submit" disabled={busy || !diagnostic || !version || !selectedSnapshot || !start || !end}>提交诊断作业</button><small>可信历史作业当前关闭。每次用户提交使用独立幂等键；重试同一请求不会重复运行。</small></div>
    </form>
    {error && <ErrorBox>{error}</ErrorBox>}
    {preflight && <div className={preflight.ready || (diagnostic && preflight.diagnostic_allowed) ? 'notice' : 'blocked'}><strong>{preflight.ready ? '可信运行门槛通过' : diagnostic && preflight.diagnostic_allowed ? '诊断运行可提交' : '运行被阻断'}</strong>{preflight.blockers.length ? <ul>{preflight.blockers.map((item, index) => <li key={`${item.code}-${index}`}><code>{item.code}</code>：{item.message}</li>)}</ul> : <p>当前请求通过能力检查。</p>}</div>}
    <h3>最近作业</h3>
    {runs.length === 0 ? <div className="empty-state">暂无提交作业。</div> : <div className="result-list">{runs.map((run) => <div className="result-row" key={run.run_id}><b>{run.status === 'queued' ? '排队中' : run.status === 'running' ? '运行中' : run.status === 'succeeded' ? '已完成' : '失败'} · {run.stage}</b><small>{run.created_at} · {run.diagnostic ? '诊断实验：不代表可信历史绩效；详细执行模式与价格口径见结果' : '可信历史运行'} · Universe {run.version_id.slice(0, 8)} · snapshot {run.snapshot_id.slice(0, 8)}</small>{run.error && <small className="job-error">{run.error.code}：{run.error.message}</small>}{run.experiment_id && <a href={`/strategies/${encodeURIComponent(run.experiment_id)}`}>查看实验结果 →</a>}<details><summary>作业身份</summary><small>run_id {run.run_id} · request {run.config_sha256} · Universe hash {run.universe_content_hash} · snapshot SHA-256 {run.snapshot_sha256}</small></details></div>)}</div>}
  </section>;
}
