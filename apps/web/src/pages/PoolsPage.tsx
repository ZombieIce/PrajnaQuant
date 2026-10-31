import { useEffect, useState } from 'react';
import { api, ErrorBox, Header, Panel } from '../shared';

type Version = { version: number; universe: string; members: string[] };
type Pool = { id: string; name: string; draft: string[]; versions: Version[] };
type Registry = { schema_version: 1; data: { pools: Pool[] } };
type Field = { state: string; value: string | null; reason: string | null };
type Instrument = { instrument_id: string; kind: string; lot_size: string | null; price_increment: string | null; size_increment: string | null;
  lifecycle: { established_date: Field; listing_date: Field }; coverage: { first_bar: string | null; last_bar: string | null; bars: number } };
type Adjusted = { version: string; instrument_id: string; method_version: string };
type Window = { common_start: string | null; common_end: string | null; start: string | null; end: string | null;
  warmup_sessions: number; warmup_sessions_used: number; warnings: string[];
  parameters: { label: string; warmup_sessions: number; runnable: boolean; reason?: string }[] };
type Plan = { runnable: boolean; universe: string; venues: string[]; adjusted: Record<string, string>; window: Window | null;
  blocking: { code: string; message: string }[] };
type RotationParameters = { params_version: number; lookback: number; top_k: number; rebalance_every: number };
type FrozenDefinition = { dsv: string; universe: string; venues?: string[]; strategy?: string;
  parameter_space?: { list?: RotationParameters[]; grid?: Record<string, unknown> };
  costs?: { commission_rate?: number; buy_slippage_bps?: number; sell_slippage_bps?: number; buy_tax_rate?: number; sell_tax_rate?: number };
  fast_event?: { initial_cash?: string; minimum_commission?: string };
  evaluation?: { start?: string; end?: string; warmup_sessions?: number };
};
type Frozen = {
  definition: FrozenDefinition;
  engine: { kind: string; semantic_version: string };
  venues: string[];
  window: Window | null;
  pool: { id: string; version: number };
};
const rotationDefaults = { lookback: '60', topK: '1', frequency: '5' };
const defaultSettings = { initial_cash: '1000000', commission_rate: '0.0003', minimum_commission: '5', buy_slippage_bps: '2', sell_slippage_bps: '2', buy_tax_rate: '0', sell_tax_rate: '0' };
type CallError = Error & { code?: string; details?: unknown };
async function call<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(url, { ...init, headers: { 'Content-Type': 'application/json', ...init?.headers } });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) {
    const failure = (body?.error ?? {}) as { code?: string; message?: string; details?: unknown };
    const error = new Error(failure.message ?? `请求失败（${response.status}）`) as CallError;
    error.code = failure.code ?? 'http_error';
    error.details = failure.details;
    throw error;
  }
  return body as T;
}
const structuralHint = (code?: string) => code === 'inconsistent_venue_calendar'
  ? '结构性共享日历错误：所选 Dataset 的 XSHG/XSHE 交易日集合或开收盘时刻不一致；Rust 不补齐、不压缩日历，也不选第一个 Venue。'
  : code === 'invalid_venue_selection'
    ? '结构性 Venue 选择错误：所选集合中有 Venue 缺少 Instrument 或 Session；不生成伪 Venue，也不静默改用其他 Venue。'
    : null;
const saved = () => { try { return JSON.parse(localStorage.getItem('pool-selection@1') ?? '{}'); } catch { return {}; } };
const field = (value: Field) => value.state === 'known' ? value.value : `${value.state} · ${value.reason ?? '未知原因'}`;
const same = (a: string[], b: string[]) => JSON.stringify(a) === JSON.stringify(b);

export default function PoolsPage() {
  const [pools, setPools] = useState<Pool[]>([]);
  const [id, setId] = useState<string>(() => saved().id ?? 'cross-asset');
  const [version, setVersion] = useState<number>(() => saved().version ?? 1);
  const [text, setText] = useState('');
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [busy, setBusy] = useState(false);
  const [catalog, setCatalog] = useState<{ dsv: string; instruments: number; venues: string[] }[]>([]);
  const [dsv, setDsv] = useState<string>(() => saved().dsv ?? '');
  const [instruments, setInstruments] = useState<Instrument[]>([]);
  const [metadataError, setMetadataError] = useState('');
  const [task, setTask] = useState('');
  const [adjusted, setAdjusted] = useState<Record<string, string>>(() => saved().adjusted ?? {});
  const [versions, setVersions] = useState<Adjusted[]>([]);
  const [strategy, setStrategy] = useState<string>(() => saved().strategy ?? 'buy_and_hold');
  const [parameterText, setParameterText] = useState<string>(() => saved().parameterText ?? '{"list":[{"params_version":1}]}');
  const [rotation, setRotation] = useState(() => ({ lookback: saved().lookback ?? rotationDefaults.lookback, topK: saved().topK ?? rotationDefaults.topK, frequency: saved().frequency ?? rotationDefaults.frequency }));
  const [settings, setSettings] = useState(() => ({ ...defaultSettings, ...(saved().settings ?? {}) }));
  const isRotation = strategy === 'pool_momentum_topk';
  const rotationParameters: RotationParameters = { params_version: 1, lookback: Number(rotation.lookback), top_k: Number(rotation.topK), rebalance_every: Number(rotation.frequency) };
  const [plan, setPlan] = useState<{ key: string; data: Plan } | null>(null);
  const [frozen, setFrozen] = useState<Frozen | null>(null);
  const [submitError, setSubmitError] = useState<CallError | null>(null);
  let parameters: unknown;
  try { parameters = isRotation ? { list: [rotationParameters] } : JSON.parse(parameterText); } catch { parameters = null; }
  const selection = { dsv, pool: { id, version }, strategy, adjusted, parameter_space: parameters,
    ...(isRotation ? { settings: { ...settings, commission_rate: Number(settings.commission_rate), buy_slippage_bps: Number(settings.buy_slippage_bps), sell_slippage_bps: Number(settings.sell_slippage_bps), buy_tax_rate: Number(settings.buy_tax_rate), sell_tax_rate: Number(settings.sell_tax_rate) } } : {}) };
  const selectionKey = JSON.stringify(selection);
  const currentPlan = plan?.key === selectionKey ? plan.data : null;
  useEffect(() => { setTask(''); setFrozen(null); setSubmitError(null); }, [selectionKey]);
  useEffect(() => {
    localStorage.setItem('pool-selection@1', JSON.stringify({ id, version, dsv, adjusted, strategy, parameterText, ...rotation, settings }));
  }, [id, version, dsv, adjusted, strategy, parameterText, rotation, settings]);
  const pool = pools.find((p) => p.id === id);
  const published = pool?.versions.find((v) => v.version === version);
  const edited = text.split(/[\s,，]+/).filter(Boolean).sort();
  const dirty = pool ? !same(edited, pool.draft) : false;
  useEffect(() => {
    api<Registry>('/api/v1/research-pools').then((value) => setPools(value.data.pools)).catch((e: Error) => setError(e.message));
    api<{ data: { datasets: typeof catalog } }>('/api/v1/datasets').then((value) => setCatalog(value.data.datasets)).catch((e: Error) => setMetadataError(e.message));
  }, []);
  useEffect(() => { setText(pool?.draft.join('\n') ?? ''); }, [pool]);
  useEffect(() => {
    const controller = new AbortController();
    setInstruments([]); setVersions([]); setMetadataError('');
    if (dsv) api<{ data: { instruments: Instrument[]; adjusted_series: { versions: Adjusted[] } } }>(`/api/v1/datasets/${encodeURIComponent(dsv)}`, { signal: controller.signal })
      .then((value) => { if (!controller.signal.aborted) { setInstruments(value.data.instruments); setVersions(value.data.adjusted_series.versions); } })
      .catch((e: Error) => { if (!controller.signal.aborted) setMetadataError(e.message); });
    return () => controller.abort();
  }, [dsv]);
  const change = async (op: 'save' | 'publish') => {
    if (!pool) return;
    setBusy(true); setError(''); setNotice('');
    try {
      const result = await api<Registry>('/api/v1/research-pools', { method: 'POST', body: JSON.stringify({ op, id, expected: pool.draft, ...(op === 'save' ? { members: edited } : {}) }) });
      setPools(result.data.pools);
      const current = result.data.pools.find((p) => p.id === id)!;
      if (op === 'publish') { setVersion(current.versions.at(-1)!.version); setAdjusted({}); }
      setNotice(op === 'save' ? '草稿已保存，已发布版本保持不变。' : '已发布固定版本；相同内容不会重复发布。');
    } catch (e) { setError((e as Error).message); }
    finally { setBusy(false); }
  };
  const preflight = async () => {
    setBusy(true); setError(''); setPlan(null); setFrozen(null); setTask(''); setSubmitError(null);
    const key = selectionKey;
    try {
      const result = await call<{ data: Plan }>('/api/v1/tasks/selections/preflight', { method: 'POST', body: JSON.stringify({ ...selection, idempotency_key: 'preflight' }) });
      setPlan({ key, data: result.data });
    } catch (e) { setSubmitError(e as CallError); }
    finally { setBusy(false); }
  };
  const submit = async () => {
    if (!currentPlan?.runnable) return;
    setBusy(true); setError(''); setTask(''); setFrozen(null); setSubmitError(null);
    try {
      const result = await call<{ task: { task_id: string }; selection: Frozen }>('/api/v1/tasks/selections', { method: 'POST', body: JSON.stringify({ ...selection, idempotency_key: crypto.randomUUID(), expected_window: currentPlan.window, result_level: isRotation ? 'full' : 'standard' }) });
      setTask(result.task.task_id);
      setFrozen(result.selection);
    } catch (e) { setSubmitError(e as CallError); }
    finally { setBusy(false); }
  };
  return <><Header /><main className="dataset-page pools-page"><h1>研究池版本管理</h1>
    <p>事后选定的 Static Universe，保留幸存者偏差，不代表历史 PIT 名单。三池独立研究；草稿不进入实验。</p>
    {error && <ErrorBox>{error}。可刷新页面读取当前草稿。</ErrorBox>}{notice && <p role="status">{notice}</p>}
    {!pool ? <p>正在读取研究池…</p> : <>
      <Panel title="编辑与发布"><label>研究池<select aria-label="研究池" value={id} disabled={busy} onChange={(e) => { setId(e.target.value); setVersion(1); setAdjusted({}); setNotice(''); setTask(''); }}>
        {pools.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}</select></label>
        <label>草稿成员（完整证券身份，每行一个）<textarea aria-label="草稿成员" rows={7} value={text} disabled={busy} onChange={(e) => setText(e.target.value)} /></label>
        <p>成员只存身份；成立/上市/行情覆盖及交易规则来自所选 Dataset。无数据时仍可管理名单，提交时由 Rust 核验。</p>
        <button className="secondary" disabled={busy || !dirty} onClick={() => void change('save')}>保存草稿</button>{' '}
        <button className="primary" disabled={busy || dirty} onClick={() => void change('publish')}>发布固定版本</button>
        {dirty && <p>请先保存草稿再发布。</p>}
      </Panel>
      <Panel title="读取已发布版本"><label>固定版本<select aria-label="固定版本" value={version} disabled={busy} onChange={(e) => { setVersion(Number(e.target.value)); setAdjusted({}); }}>
        {pool.versions.map((v) => <option key={v.version} value={v.version}>版本 {v.version} · {v.members.length} 只</option>)}</select></label>
        <ul data-testid="pool-members">{published?.members.map((member) => <li key={member}>{member}</li>)}</ul>
        <p>当前显示版本已冻结；实验固定该版本的成员身份和所选 Dataset，未来编辑不改写旧实验。</p>
        <label>元数据与实验 Dataset<select aria-label="研究池 Dataset" value={dsv} disabled={busy} onChange={(e) => { setDsv(e.target.value); setAdjusted({}); }}><option value="">请选择固定 Dataset</option>
          {catalog.map((d) => <option key={d.dsv} value={d.dsv}>{d.venues.join('/')} · {d.instruments} 只 · {d.dsv.slice(-12)}</option>)}</select></label>
        {metadataError && <ErrorBox>{metadataError}</ErrorBox>}
        <div className="table-wrap"><table><thead><tr><th>成员</th><th>成立/上市</th><th>实际行情覆盖</th><th>手数/价格增量/数量增量</th></tr></thead><tbody>
          {published?.members.map((member) => { const i = instruments.find((item) => item.instrument_id === member); return <tr key={member} data-testid="pool-metadata"><td>{member}</td>
            {i ? <><td>{field(i.lifecycle.established_date)} / {field(i.lifecycle.listing_date)}</td><td>{i.coverage.first_bar ?? '未知'} ～ {i.coverage.last_bar ?? '未知'} · {i.coverage.bars} 条</td><td>{i.lot_size ?? '未知'} / {i.price_increment ?? '未知'} / {i.size_increment ?? '未知'}</td></>
              : <td colSpan={3}>Unknown：{dsv ? '此 Dataset 无该成员' : '未选择 Dataset'}</td>}</tr>; })}
        </tbody></table></div>
        <p data-testid="pool-identity">固定身份：{id} / {version} · Universe {published?.universe} · DSV {dsv || '未选择'}</p>
        {published?.members.map((member) => <label key={member}>复权版本 {member}<select aria-label={`复权版本 ${member}`} disabled={busy} value={adjusted[member] ?? ''}
          onChange={(e) => { const next = { ...adjusted }; if (e.target.value) next[member] = e.target.value; else delete next[member]; setAdjusted(next); }}>
          <option value="">请选择固定 adjusted version</option>{versions.filter((v) => v.instrument_id === member).map((v) => <option key={v.version} value={v.version}>{v.method_version} · {v.version}</option>)}
        </select><code>{adjusted[member] ?? '未绑定'}</code></label>)}
        <label>预检策略<select aria-label="预检策略" disabled={busy} value={strategy} onChange={(e) => { setStrategy(e.target.value); setParameterText(''); }}>
          <option value="buy_and_hold">买入持有</option><option value="ma_crossover">均线交叉</option><option value="s2_rotation">轮动（旧多因子）</option><option value="pool_momentum_topk">池内动量 Top-K 轮动</option></select></label>
        {isRotation ? <>
          <p className="hint">按绑定复权收盘计算尾随动量，T 日收盘排名，下一可用 Session 开盘尝试成交；并列按 InstrumentId 升序，合格不足 K 按实际数量等权，零候选为全现金。受阻买腿资金留现金、不重分配；池内黄金仅被选中时持有。默认动量窗口 60、Top-K 1、每 5 Session 调仓，仅为可编辑模拟参数。</p>
          <label>动量窗口（Session）<input aria-label="动量窗口" type="number" min="1" step="1" required value={rotation.lookback} onChange={e => setRotation({ ...rotation, lookback: e.target.value })} /></label>
          <label>Top-K<input aria-label="Top-K" type="number" min="1" step="1" required value={rotation.topK} onChange={e => setRotation({ ...rotation, topK: e.target.value })} /></label>
          <label>调仓频率（Session）<input aria-label="调仓频率" type="number" min="1" step="1" required value={rotation.frequency} onChange={e => setRotation({ ...rotation, frequency: e.target.value })} /></label>
          {(Object.entries({ initial_cash: '本金（CNY）', commission_rate: '佣金率', minimum_commission: '最低佣金（CNY）', buy_slippage_bps: '买入滑点（bp）', sell_slippage_bps: '卖出滑点（bp）', buy_tax_rate: '买入税率', sell_tax_rate: '卖出税率' }) as [string, string][]).map(([key, label]) => <label key={key}>{label}<input aria-label={label} type="number" min={key === 'initial_cash' ? '0.00000001' : '0'} step="any" required value={settings[key]} onChange={e => setSettings({ ...settings, [key]: e.target.value })} /></label>)}
          <p className="hint" data-testid="rotation-parameters">当前模板参数：动量窗口 {rotation.lookback} · Top-K {rotation.topK} · 调仓频率 {rotation.frequency} · 本金 {settings.initial_cash} CNY · 佣金率 {settings.commission_rate} · 最低佣金 {settings.minimum_commission} · 买/卖滑点 {settings.buy_slippage_bps}/{settings.sell_slippage_bps}bp · 买/卖税 {settings.buy_tax_rate}/{settings.sell_tax_rate}</p>
        </> : <>
          <label>参数组合 JSON<textarea aria-label="参数组合 JSON" disabled={busy} value={parameterText} onChange={(e) => setParameterText(e.target.value)} /></label>
          {!parameters && <p>请输入有效参数列表或网格 JSON，预热由 Rust 校验。</p>}
        </>}
        <p>支持单交易所与沪深同池（XSHG+XSHE）；多 Venue 基线按 ADR 0027 使用 full_target，Rust 冻结真实 Venue 集合，不补日、不压缩日历，也不选单一 Venue。复权价仅作研究，原始价负责成交/估值，分红总回报未完整验证；模拟成本、参数与手数来源随冻结定义保存。</p>
        <button className="secondary" disabled={busy || !dsv || !published || !parameters} onClick={() => void preflight()}>预检共同窗口</button>
        {currentPlan && <div data-testid="pool-preflight" className={currentPlan.runnable ? 'notice' : 'blocked'}>
          <strong>{currentPlan.runnable ? '可运行' : '不可运行'}</strong>
          <p data-testid="plan-venues">Venue 范围：{currentPlan.venues.join('、')}（各自真实交易所；不合并为单一 Venue）</p>
          <p>Universe {currentPlan.universe}</p>
          {currentPlan.window && <><p>共同数据窗口：{currentPlan.window.common_start ?? 'Unknown'} ～ {currentPlan.window.common_end ?? 'Unknown'}</p>
            <p>最大预热：{currentPlan.window.warmup_sessions} Sessions；实际预热：{currentPlan.window.warmup_sessions_used} Sessions</p>
            <p>评价窗口/可运行范围：{currentPlan.window.start ?? 'Unknown'} ～ {currentPlan.window.end ?? 'Unknown'}</p>
            <ul>{currentPlan.window.parameters.map((p, index) => <li key={index}>{p.label} · 预热 {p.warmup_sessions} · {p.runnable ? '可运行' : `不可运行：${p.reason}`}</li>)}</ul>
            {currentPlan.window.warnings.map((w, index) => <p key={index}>{w}</p>)}</>}
          <ul>{currentPlan.blocking.map((b, index) => <li key={index}><code>{b.code}</code>：{b.message}</li>)}</ul>
        </div>}
        {submitError && <ErrorBox>
          <strong>预检/提交未通过</strong>
          <p><code>{submitError.code ?? 'http_error'}</code>：{submitError.message}</p>
          {structuralHint(submitError.code) && <p>{structuralHint(submitError.code)}</p>}
          <p>页面不展示替代计划、零交易或伪造成功。</p>
        </ErrorBox>}
        <button className="primary" disabled={busy || !currentPlan?.runnable} onClick={() => void submit()}>提交固定池实验</button>
        {frozen && <div data-testid="frozen-selection" className="notice">
          <strong>冻结输入（Rust 选择/任务入口）</strong>
          <p>输入池：{frozen.pool.id} v{frozen.pool.version} · Dataset：<code>{frozen.definition.dsv}</code></p>
          <p>Venue：{frozen.venues.join('、')} · 引擎：<code>{frozen.engine.semantic_version}</code>（{frozen.engine.kind}）</p>
          <p>Universe：<code>{frozen.definition.universe}</code>{frozen.window ? ` · 评价窗口：${frozen.window.start ?? 'Unknown'} ～ ${frozen.window.end ?? 'Unknown'}（预热 ${frozen.window.warmup_sessions} Sessions）` : ''}</p>
          {(() => { const parameters = frozen.definition.parameter_space?.list?.[0]; const costs = frozen.definition.costs; if (frozen.definition.strategy !== 'pool_momentum_topk' || !parameters) return null;
            return <p data-testid="frozen-rotation-parameters">冻结模板：池内动量 Top-K · 动量窗口 {parameters.lookback} · Top-K {parameters.top_k} · 调仓频率 {parameters.rebalance_every} · 本金 {frozen.definition.fast_event?.initial_cash} CNY · 佣金率 {costs?.commission_rate} · 最低佣金 {frozen.definition.fast_event?.minimum_commission} · 买/卖滑点 {costs?.buy_slippage_bps}/{costs?.sell_slippage_bps}bp · 买/卖税 {costs?.buy_tax_rate}/{costs?.sell_tax_rate}</p>; })()}
          <p>浏览器只展示 Rust 冻结与持久结果，不计算费用、净值或绩效。</p>
          {task && <p><a href={`/backtests?task=${encodeURIComponent(task)}`}>查看固定池任务</a></p>}
        </div>}
      </Panel>
    </>}
  </main></>;
}
