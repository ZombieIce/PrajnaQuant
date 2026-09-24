import { useEffect, useState } from 'react';
import { Chart } from '../Chart';
import { api, pct, Header, ErrorBox, Panel, type Signal, type Payload } from '../shared';

const numberOrDash = (value: number | null | undefined, digits = 3) =>
  value == null || !Number.isFinite(value) ? '—' : value.toFixed(digits);

const percentageOrDash = (value: number | null | undefined) =>
  value == null || !Number.isFinite(value) ? '—' : pct(value);

const chartBase = {
  tooltip: { trigger: 'axis' as const },
  dataZoom: [{ type: 'inside' as const }, { type: 'slider' as const }],
};

export default function FactorDetail() {
  const [signals, setSignals] = useState<Signal[]>([]);
  const [key, setKey] = useState(decodeURIComponent(location.pathname.split('/').pop() || 'momentum_60'));
  const [data, setData] = useState<Payload | null>(null);
  const [error, setError] = useState('');

  useEffect(() => {
    api<Signal[]>('/api/v1/signals').then(setSignals).catch((e) => setError(e.message));
  }, []);

  useEffect(() => {
    setData(null);
    setError('');
    api<Payload>(`/api/research/signals/${encodeURIComponent(key)}/report`)
      .then(setData)
      .catch((e) => setError(e.message));
  }, [key]);

  const report = data?.report?.reports?.[0];
  const daily = report?.daily ?? [];
  const dates = daily.map((point) => point.date);
  const rolling = daily.map((_, index) => {
    const values = daily
      .slice(Math.max(0, index - 59), index + 1)
      .map((point) => point.rank_ic)
      .filter((value): value is number => value != null && Number.isFinite(value));
    return values.length ? values.reduce((sum, value) => sum + value, 0) / values.length : null;
  });
  const quantileCount = Math.max(0, ...daily.map((point) => point.quantile_returns.length));
  const evaluationLabel = report?.evaluation_status === 'evaluated'
    ? '有效'
    : report?.evaluation_status === 'insufficient_cross_section'
      ? '截面样本不足'
      : '无有效 IC';

  const metricRows: [string, string][] = report ? [
    ['评价状态', evaluationLabel],
    ['Pearson IC', numberOrDash(report.mean_pearson_ic, 4)],
    ['Mean Rank IC', numberOrDash(report.mean_rank_ic, 4)],
    ['Rank ICIR', numberOrDash(report.rank_ic_ir)],
    ['IC 正率', percentageOrDash(report.rank_ic_positive_rate)],
    ['有效标签覆盖率', percentageOrDash(report.coverage)],
    ['前瞻收益 Q 高－Q 低', percentageOrDash(report.mean_long_short_return)],
    ['因子自相关 Lag 1', numberOrDash(report.mean_factor_autocorrelation_lag1)],
    ['Top 分位换手', percentageOrDash(report.mean_top_quantile_turnover)],
    ['有效截面日', `${report.evaluated_dates} / ${report.daily.length}`],
    ['有效标签数', `${report.labeled_observation_count.toLocaleString()} / ${report.expected_observation_count.toLocaleString()}`],
    ['缺失分数 / 标签', `${report.missing_factor_count.toLocaleString()} / ${report.missing_label_count.toLocaleString()}`],
  ] : [];

  return <>
    <Header />
    <main>
      <h1>{data?.report.definition.display_name ?? '因子研究详情'}</h1>
      <p>{data?.report.definition.description ?? '展示已保存的因子研究报告。'}</p>
      {signals.length > 0 && <label>因子
        <select value={key} onChange={(event) => {
          history.pushState({}, '', `/factors/${encodeURIComponent(event.target.value)}`);
          setKey(event.target.value);
        }}>
          {signals.map((signal) => <option key={signal.key} value={signal.key}>{signal.display_name}</option>)}
        </select>
      </label>}
      {error ? <ErrorBox>{error}。请先生成并保存该因子的研究报告。</ErrorBox>
        : !report ? <div className="notice">加载已保存报告中…</div>
          : <>
            <p className="notice">
              标签：{report.label_method === 'next_open_to_forward_open' ? '下一行情日开盘入场，H 个交易日后开盘退出' : 'T 日收盘至第 H 个证券有效 bar 收盘'}；
              日历：{data.report.calendar_basis}；分组收益是逐期前瞻标签，不是可交易净值。
            </p>
            <section className="metrics">
              {metricRows.map(([name, value]) => <div className="metric" key={name}>
                <small>{name}</small><strong>{value}</strong>
              </div>)}
            </section>
            <section className="grid">
              <Panel title="Pearson IC 时间序列">
                <Chart option={{ ...chartBase, xAxis: { type: 'category', data: dates }, yAxis: { type: 'value' }, series: [{ type: 'line', showSymbol: false, data: daily.map((point) => point.pearson_ic) }] }} />
              </Panel>
              <Panel title="Rank IC 时间序列">
                <Chart option={{ ...chartBase, xAxis: { type: 'category', data: dates }, yAxis: { type: 'value' }, series: [{ type: 'line', showSymbol: false, data: daily.map((point) => point.rank_ic) }] }} />
              </Panel>
              <Panel title="Rolling Rank IC（近 60 个报告日）">
                <Chart option={{ ...chartBase, xAxis: { type: 'category', data: dates }, yAxis: { type: 'value' }, series: [{ type: 'line', showSymbol: false, data: rolling, color: '#0f766e' }] }} />
              </Panel>
              <Panel title="分组前瞻收益（不复利）">
                <Chart option={{ ...chartBase, legend: {}, xAxis: { type: 'category', data: dates }, yAxis: { type: 'value' }, series: Array.from({ length: quantileCount }, (_, index) => ({ name: `Q${index + 1}`, type: 'line' as const, showSymbol: false, data: daily.map((point) => point.quantile_returns[index] ?? null) })) }} />
              </Panel>
              <Panel title="Q 高－Q 低逐期前瞻收益（不复利）">
                <Chart option={{ ...chartBase, xAxis: { type: 'category', data: dates }, yAxis: { type: 'value' }, series: [{ type: 'line', showSymbol: false, data: daily.map((point) => point.long_short_return), color: '#dc2626' }] }} />
              </Panel>
              <Panel title="IC 衰减（不同前瞻期）">
                <Chart option={{ tooltip: { trigger: 'axis' }, xAxis: { type: 'category', data: data.report.reports.map((item) => `${item.forward_days}D`) }, yAxis: { type: 'value' }, series: [{ type: 'bar', data: data.report.reports.map((item) => item.mean_rank_ic) }] }} />
              </Panel>
              <Panel title="有效标签数 / Universe 数">
                <Chart option={{ ...chartBase, xAxis: { type: 'category', data: dates }, yAxis: { type: 'value' }, series: [
                  { name: '有效标签数', type: 'line', showSymbol: false, data: daily.map((point) => point.count), color: '#7c3aed' },
                  { name: '评价 Universe', type: 'line', showSymbol: false, data: daily.map((point) => point.expected_count), color: '#0891b2' },
                ] }} />
              </Panel>
            </section>
          </>}
    </main>
  </>;
}
