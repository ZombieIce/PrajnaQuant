import { useEffect, useState } from 'react';
import { api, pct, useSort, Header, ErrorBox, SavedResultTools, type StrategySummary } from '../shared';
import RunJobsPanel from '../components/RunJobsPanel';

export default function StrategyDirectory() {
  const [rows, setRows] = useState<StrategySummary[]>([]); const [error, setError] = useState('');
  useEffect(() => { api<StrategySummary[]>('/api/experiments').then(setRows).catch((e) => setError(e.message)); }, []);
  const { sorted, by, sort } = useSort(rows, 'created_at'); const th = (title: string, key: keyof StrategySummary) => <th onClick={() => by(key)}>{title}{sort.key === key ? (sort.desc ? ' ↓' : ' ↑') : ''}</th>;
  return <><Header /><main><h1>策略绩效</h1><p>已保存策略实验与 Rust 回测作业。诊断实验始终带限制标记，不能解释为可信历史业绩。</p><RunJobsPanel /><SavedResultTools kind="strategy" />{error && <ErrorBox>{error}</ErrorBox>}{!error && rows.length === 0 && <div className="empty-state">暂无已保存策略实验。</div>}<div className="table-wrap"><table><thead><tr>{th('策略名称', 'name')}{th('累计收益', 'total_return')}{th('年化收益', 'annualized_return')}{th('夏普', 'sharpe_ratio')}{th('最大回撤', 'max_drawdown')}{th('卡玛', 'calmar_ratio')}<th>Universe</th><th>规则</th></tr></thead><tbody>{sorted.map((s) => <tr key={s.experiment_id}><td><a className="row-link" href={`/strategies/${s.experiment_id}`}>{s.name}</a><small>{s.created_at.slice(0, 10)}</small></td><td>{pct(s.total_return)}</td><td>{pct(s.annualized_return)}</td><td>{s.sharpe_ratio.toFixed(2)}</td><td>{pct(s.max_drawdown)}</td><td>{s.calmar_ratio.toFixed(2)}</td><td>{s.universe_status === 'legacy_universe_unknown' || !s.universe_id ? 'Universe 未知' : `${s.universe_name ?? s.universe_id} · ${s.universe_pit_status ?? 'unknown'} / ${s.universe_coverage ?? 'unverified'}`}</td><td>{s.score_mode === 'rotation_composite' ? '轮动综合评分' : s.score_mode === 'single_momentum' ? `${s.lookback_days}日动量` : '评分口径未知'} · Top{s.top_n} · {s.rebalance_every}日调仓</td></tr>)}</tbody></table></div></main></>;
}
