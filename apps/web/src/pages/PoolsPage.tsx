import { useEffect, useState } from 'react';
import { api, ErrorBox, Header, Panel } from '../shared';

type Version = { version: number; universe: string; members: string[] };
type Pool = { id: string; name: string; draft: string[]; versions: Version[] };
type Registry = { schema_version: 1; data: { pools: Pool[] } };
type Field = { state: string; value: string | null; reason: string | null };
type Instrument = { instrument_id: string; kind: string; lot_size: string | null; price_increment: string | null; size_increment: string | null;
  lifecycle: { established_date: Field; listing_date: Field }; coverage: { first_bar: string | null; last_bar: string | null; bars: number } };
const field = (value: Field) => value.state === 'known' ? value.value : `${value.state} · ${value.reason ?? '未知原因'}`;
const same = (a: string[], b: string[]) => JSON.stringify(a) === JSON.stringify(b);

export default function PoolsPage() {
  const [pools, setPools] = useState<Pool[]>([]);
  const [id, setId] = useState('cross-asset');
  const [version, setVersion] = useState(1);
  const [text, setText] = useState('');
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [busy, setBusy] = useState(false);
  const [catalog, setCatalog] = useState<{ dsv: string; instruments: number; venues: string[] }[]>([]);
  const [dsv, setDsv] = useState('');
  const [instruments, setInstruments] = useState<Instrument[]>([]);
  const [metadataError, setMetadataError] = useState('');
  const [task, setTask] = useState('');
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
    setInstruments([]); setMetadataError('');
    if (dsv) api<{ data: { instruments: Instrument[] } }>(`/api/v1/datasets/${encodeURIComponent(dsv)}`, { signal: controller.signal })
      .then((value) => { if (!controller.signal.aborted) setInstruments(value.data.instruments); })
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
      if (op === 'publish') setVersion(current.versions.at(-1)!.version);
      setNotice(op === 'save' ? '草稿已保存，已发布版本保持不变。' : '已发布固定版本；相同内容不会重复发布。');
    } catch (e) { setError((e as Error).message); }
    finally { setBusy(false); }
  };
  const submit = async () => {
    setBusy(true); setError(''); setTask('');
    try {
      const result = await api<{ task: { task_id: string } }>('/api/v1/tasks/selections', { method: 'POST', body: JSON.stringify({ idempotency_key: crypto.randomUUID(), dsv, pool: { id, version }, strategy: 'buy_and_hold', result_level: 'standard' }) });
      setTask(result.task.task_id);
    } catch (e) { setError((e as Error).message); }
    finally { setBusy(false); }
  };
  return <><Header /><main className="dataset-page pools-page"><h1>研究池版本管理</h1>
    <p>事后选定的 Static Universe，保留幸存者偏差，不代表历史 PIT 名单。三池独立研究；草稿不进入实验。</p>
    {error && <ErrorBox>{error}。可刷新页面读取当前草稿。</ErrorBox>}{notice && <p role="status">{notice}</p>}
    {!pool ? <p>正在读取研究池…</p> : <>
      <Panel title="编辑与发布"><label>研究池<select aria-label="研究池" value={id} disabled={busy} onChange={(e) => { setId(e.target.value); setVersion(1); setNotice(''); setTask(''); }}>
        {pools.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}</select></label>
        <label>草稿成员（完整证券身份，每行一个）<textarea aria-label="草稿成员" rows={7} value={text} disabled={busy} onChange={(e) => setText(e.target.value)} /></label>
        <p>成员只存身份；成立/上市/行情覆盖及交易规则来自所选 Dataset。无数据时仍可管理名单，提交时由 Rust 核验。</p>
        <button className="secondary" disabled={busy || !dirty} onClick={() => void change('save')}>保存草稿</button>{' '}
        <button className="primary" disabled={busy || dirty} onClick={() => void change('publish')}>发布固定版本</button>
        {dirty && <p>请先保存草稿再发布。</p>}
      </Panel>
      <Panel title="读取已发布版本"><label>固定版本<select aria-label="固定版本" value={version} disabled={busy} onChange={(e) => setVersion(Number(e.target.value))}>
        {pool.versions.map((v) => <option key={v.version} value={v.version}>版本 {v.version} · {v.members.length} 只</option>)}</select></label>
        <ul data-testid="pool-members">{published?.members.map((member) => <li key={member}>{member}</li>)}</ul>
        <p>当前显示版本已冻结；实验固定该版本的成员身份和所选 Dataset，未来编辑不改写旧实验。</p>
        <label>元数据与实验 Dataset<select aria-label="研究池 Dataset" value={dsv} disabled={busy} onChange={(e) => setDsv(e.target.value)}><option value="">请选择固定 Dataset</option>
          {catalog.map((d) => <option key={d.dsv} value={d.dsv}>{d.venues.join('/')} · {d.instruments} 只 · {d.dsv.slice(-12)}</option>)}</select></label>
        {metadataError && <ErrorBox>{metadataError}</ErrorBox>}
        <div className="table-wrap"><table><thead><tr><th>成员</th><th>成立/上市</th><th>实际行情覆盖</th><th>手数/价格增量/数量增量</th></tr></thead><tbody>
          {published?.members.map((member) => { const i = instruments.find((item) => item.instrument_id === member); return <tr key={member} data-testid="pool-metadata"><td>{member}</td>
            {i ? <><td>{field(i.lifecycle.established_date)} / {field(i.lifecycle.listing_date)}</td><td>{i.coverage.first_bar ?? '未知'} ～ {i.coverage.last_bar ?? '未知'} · {i.coverage.bars} 条</td><td>{i.lot_size ?? '未知'} / {i.price_increment ?? '未知'} / {i.size_increment ?? '未知'}</td></>
              : <td colSpan={3}>Unknown：{dsv ? '此 Dataset 无该成员' : '未选择 Dataset'}</td>}</tr>; })}
        </tbody></table></div>
        <p>仅提交已发布的单交易所版本，策略为现有期初等权买入持有；沪深混合池可管理，买入持有提交暂不支持。原始价格研究，分红总回报未完整验证。默认模拟成本由 Rust 冻结。</p>
        <button className="primary" disabled={busy || !dsv || !published} onClick={() => void submit()}>提交固定池买入持有</button>
        {task && <p><a href={`/backtests?task=${encodeURIComponent(task)}`}>查看固定池任务</a></p>}
      </Panel>
    </>}
  </main></>;
}
