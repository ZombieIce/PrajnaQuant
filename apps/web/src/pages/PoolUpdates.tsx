import { useEffect, useState } from 'react';
import { api, ErrorBox, Panel } from '../shared';

type Coverage = { first_bar: string | null; last_bar: string | null; bars: number };
type Member = { collection: string; publication: string; error: string | null; coverage: Coverage | null;
  succeeded_at: string | null; reused_from: string | null; adjusted: string | null; adjustment_error: string | null };
type Attempt = { id: string; pool: string; pool_version: number; start: string; end: string; created_at: string;
  finished_at: string | null; status: string; price_ready: boolean; dataset: { dsv: string } | null; dataset_error: string | null;
  members: Record<string, Member>; actions: { complete?: boolean; total_return?: string; reason?: string; members?: Record<string, { state: string }> } };
type Data = { configs: { name: string; pool: { id: string; version: number }; start: string; end: string }[]; attempts: Attempt[];
  tasks: { attempt: string; task: { status: string; stage: string; error?: { message: string } | null } }[];
  last_price_ready: Record<string, { attempt: string; finished_at: string; dataset: { dsv: string } }>;
  last_member_success: Record<string, { succeeded_at: string; coverage: Coverage }> };
type Props = { poolId: string; version: number; onSelect: (dsv: string, adjusted: Record<string, string>) => Promise<void> };
const coverage = (c: Coverage | null) => c ? `${c.first_bar ?? 'Unknown'} ～ ${c.last_bar ?? 'Unknown'} · ${c.bars} 条` : '未发布';

export default function PoolUpdates({ poolId, version, onSelect }: Props) {
  const [data, setData] = useState<Data | null>(null);
  const [config, setConfig] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [refresh, setRefresh] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    api<{ data: Data }>('/api/v1/pool-updates', { signal: controller.signal }).then(({ data }) => {
      if (controller.signal.aborted) return;
      setData(data); setError('');
      if (data.tasks.some(({ task }) => ['queued', 'running'].includes(task.status))) {
        timer = setTimeout(() => setRefresh(n => n + 1), 1000);
      }
    }).catch((e: Error) => { if (!controller.signal.aborted) setError(e.message); });
    return () => { controller.abort(); clearTimeout(timer); };
  }, [refresh]);
  const configurations = data?.configs.filter(c => c.pool.id === poolId && c.pool.version === version) ?? [];
  const selected = configurations.find(c => c.name === config);
  const attempts = data?.attempts.filter(a => a.pool === poolId && a.pool_version === version) ?? [];
  const last = data?.last_price_ready[`${poolId}:${version}`];
  const submit = async (input: { op: 'prepare'; config: string } | { op: 'retry'; attempt: string }) => {
    setBusy(true); setError('');
    try {
      await api('/api/v1/pool-updates', { method: 'POST', body: JSON.stringify(input) });
      setRefresh(n => n + 1);
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  };
  const select = async (attempt: Attempt) => {
    if (!attempt.dataset) return;
    setBusy(true); setError('');
    try {
      const adjusted: Record<string, string> = {};
      for (const [id, member] of Object.entries(attempt.members)) if (member.adjusted) adjusted[id] = member.adjusted;
      await onSelect(attempt.dataset.dsv, adjusted);
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  };
  return <Panel title="手动更新固定研究池">
    <p>行情使用现有采集或显式已有采集报告；复权与行动需本机供应文件。未配置不猜来源，不生成复权价格或现金事实。</p>
    <p>每次固定本池版本、成员和日期。补跑只复用本计划成功成员；等待输入不占用任务队列。</p>
    <label>本机更新配置<select aria-label="本机更新配置" value={selected?.name ?? ''} disabled={busy} onChange={e => setConfig(e.target.value)}>
      <option value="">选择显式配置</option>{configurations.map(c => <option key={c.name} value={c.name}>{c.name} · {c.start} ～ {c.end}</option>)}
    </select></label>
    {!configurations.length && <p>此池版本尚无本机输入映射；请配置 pool-updates/configs 后刷新。页面不自动使用其他版本。</p>}
    <button className="primary" disabled={busy || !selected} onClick={() => selected && void submit({ op: 'prepare', config: selected.name })}>更新选定池</button>
    <button className="secondary" disabled={busy} onClick={() => setRefresh(n => n + 1)}>刷新更新记录</button>
    {error && <ErrorBox>{error}</ErrorBox>}
    <p data-testid="update-last-success">上次价格输入就绪：{last ? `${last.finished_at} · ${last.dataset.dsv}` : '尚无成功记录'}。它与本次尝试分开，失败不会清空。</p>
    {attempts.map(attempt => {
      const task = data?.tasks.find(t => t.attempt === attempt.id)?.task;
      const active = task && ['queued', 'running'].includes(task.status);
      return <section key={attempt.id} data-testid="pool-update-attempt" data-attempt={attempt.id}>
        <h3>更新尝试 {attempt.created_at}</h3>
        <p>目标范围 {attempt.start} ～ {attempt.end} · 最近完成 {attempt.finished_at ?? '未完成'}</p>
        <p>队列执行：{task?.status ?? '未入队'} · {task?.stage ?? '等待提交'}；执行完成不等于研究全部就绪。</p>
        {task?.error && <ErrorBox>{task.error.message}</ErrorBox>}
        <p data-testid="update-readiness">原始池行情：{attempt.dataset ? '已发布' : '未就绪'}；价格研究输入：{attempt.price_ready ? '已就绪' : '待补充'}；行动覆盖：{attempt.actions.complete ? '范围已覆盖' : '不完整/Unknown'}</p>
        <p>行动覆盖不证明所有引擎均可运行；沪深混合池行动绑定等既有限制保持。行动缺口不阻断无关价格因子。</p>
        <p>总回报适用性：{attempt.actions.total_return ?? '尚未核对'}</p>
        {attempt.dataset_error && <ErrorBox>{attempt.dataset_error}</ErrorBox>}
        {attempt.actions.reason && <p>行动输入：{attempt.actions.reason}</p>}
        <div className="table-wrap"><table><thead><tr><th>成员</th><th>采集 / 发布</th><th>本次实际覆盖 / 最近成功</th><th>研究输入</th></tr></thead><tbody>
          {Object.entries(attempt.members).map(([id, member]) => <tr key={id}>
            <td>{id}</td><td>{member.collection} / {member.publication}{member.reused_from && <p>显式复用尝试 {member.reused_from}</p>}{member.error && <p>{member.error}</p>}</td>
            <td>{coverage(member.coverage)}<p>最近发布成功 {data?.last_member_success[`${poolId}:${version}:${id}`]?.succeeded_at ?? '无'}</p></td>
            <td>复权：{member.adjusted ?? member.adjustment_error ?? '尚未消费'}<p>行动：{attempt.actions.members?.[id]?.state ?? 'unknown'}</p></td>
          </tr>)}
        </tbody></table></div>
        <button className="secondary" disabled={busy || !!active} onClick={() => void submit({ op: 'retry', attempt: attempt.id })}>补跑本计划</button>
        <button className="primary" disabled={busy || !attempt.price_ready} onClick={() => void select(attempt)}>使用此固定版本研究</button>
      </section>;
    })}
  </Panel>;
}
