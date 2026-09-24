import { useEffect, useMemo, useState } from 'react';
import { Chart } from '../Chart';
import { api, pct, Header, ErrorBox, Panel, type Experiment } from '../shared';

export default function StrategyDetail() {
  const id = decodeURIComponent(location.pathname.split('/').pop() || '');
  const [exp, setExp] = useState<Experiment | null>(null);
  const [error, setError] = useState('');
  const [rangeStart, setRangeStart] = useState('');
  const [rangeEnd, setRangeEnd] = useState('');

  useEffect(() => {
    api<Experiment>(`/api/experiments/${encodeURIComponent(id)}`).then(setExp).catch((e) => setError(e.message));
  }, [id]);

  const curve = exp?.backtest.equity_curve ?? [];
  const firstDate = curve[0]?.date ?? '';
  const lastDate = curve.at(-1)?.date ?? '';
  const selectedStart = rangeStart && rangeStart >= firstDate && rangeStart <= lastDate ? rangeStart : firstDate;
  const selectedEnd = rangeEnd && rangeEnd >= selectedStart && rangeEnd <= lastDate ? rangeEnd : lastDate;
  const visibleCurve = useMemo(
    () => curve.filter((point) => point.date >= selectedStart && point.date <= selectedEnd),
    [curve, selectedStart, selectedEnd],
  );
  const visiblePositionCurve = useMemo(
    () => (exp?.backtest.position_curve ?? []).filter((point) => point.date >= selectedStart && point.date <= selectedEnd),
    [exp, selectedStart, selectedEnd],
  );
  const normalizedNav = useMemo(() => {
    const base = visibleCurve[0]?.equity ?? 1;
    return visibleCurve.map((point) => point.equity / base);
  }, [visibleCurve]);
  const selectedDrawdown = useMemo(() => {
    let peak = Number.NEGATIVE_INFINITY;
    return visibleCurve.map((point) => {
      peak = Math.max(peak, point.equity);
      return peak > 0 ? point.equity / peak - 1 : 0;
    });
  }, [visibleCurve]);

  if (!exp) return <><Header /><main>{error ? <ErrorBox>{error}</ErrorBox> : <p>加载策略详情…</p>}</main></>;

  const metrics = exp.backtest.metrics;
  const dates = visibleCurve.map((point) => point.date);
  const positionSymbols = [...new Set(visiblePositionCurve.flatMap((point) => point.holdings.map((holding) => holding.symbol)))].sort();
  const finalPositions = Object.entries(exp.backtest.final_positions ?? {}).filter(([, quantity]) => quantity > 0);
  const deferrals = exp.backtest.rebalance_deferrals;
  const unexecuted = exp.backtest.unexecuted_orders;
  const instrumentPerformance = [...(exp.backtest.instrument_performance ?? [])].sort((a, b) => b.total_pnl - a.total_pnl);
  const onStartChange = (value: string) => {
    setRangeStart(value);
    if (value > selectedEnd) setRangeEnd(value);
  };
  const onEndChange = (value: string) => {
    setRangeEnd(value);
    if (value < selectedStart) setRangeStart(value);
  };

  return <>
    <Header />
    <main>
      <a className="back" href="/">← 策略目录</a>
      <h1>{exp.name}</h1>
      <p>{exp.config.strategy.momentum_short_days && exp.config.strategy.momentum_long_days ? `轮动综合评分（短动量 ${exp.config.strategy.momentum_short_days} 日 / 长动量 ${exp.config.strategy.momentum_long_days} 日）` : `${exp.config.strategy.lookback_days}日动量`} · Top{exp.config.strategy.top_n} · 每{exp.config.strategy.rebalance_every}个交易日调仓</p>
      <p>{exp.universe ? `${exp.universe.name} · ${exp.universe.pit_status} · ${exp.universe.coverage}` : 'Universe 未知（旧实验）'}</p>
      <p>执行状态：{exp.backtest.execution_status_mode === 'status_gated' ? '冻结快照状态门槛（历史可知性未验证）' : exp.backtest.execution_status_mode === 'legacy_bar_only' ? '旧快照仅按行情价判断，未验证可交易状态' : '旧报告未记录执行状态模式'}</p>
      <details><summary>实验身份与假设</summary><p>Universe ID：{exp.universe?.universe_id ?? '旧结果缺失'} · 版本 ID：{exp.universe?.version_id ?? '旧结果缺失'} · 成员内容 hash：{exp.universe?.content_hash ?? '旧结果缺失'}</p><p>快照 ID：{exp.snapshot?.snapshot_id ?? '缺失'} · SHA-256：{exp.snapshot?.sha256 ?? '缺失'}</p>{exp.assumptions?.length ? <ul>{exp.assumptions.map((item, index) => <li key={index}>{item}</li>)}</ul> : <p>旧结果未保存假设，不能据此推断历史 PIT、分红或收益口径。</p>}</details>
      <section className="metrics">{[
        ['累计收益', pct(metrics.total_return)],
        ['年化收益', pct(metrics.annualized_return)],
        ['夏普比率', metrics.sharpe_ratio.toFixed(2)],
        ['卡玛比率', metrics.calmar_ratio.toFixed(2)],
        ['最大回撤', pct(metrics.max_drawdown)],
      ].map(([key, value]) => <div className="metric" key={key}><small>{key}</small><strong>{value}</strong></div>)}</section>

      <section className="strategy-curves" aria-label="策略净值和回撤曲线">
        <div className="curve-range" aria-label="统一图表时间区间">
          <strong>图表区间</strong>
          <label>开始日期<input type="date" min={firstDate} max={selectedEnd} value={selectedStart} onChange={(event) => onStartChange(event.target.value)} /></label>
          <span>至</span>
          <label>结束日期<input type="date" min={selectedStart} max={lastDate} value={selectedEnd} onChange={(event) => onEndChange(event.target.value)} /></label>
          <small>净值按区间首个有效点归一至 1.0；回撤从所选区间重新计算，顶部指标仍按完整回测期统计。</small>
        </div>
        {visibleCurve.length === 0 ? <div className="empty-state">所选区间没有权益数据。</div> : <>
          <Panel title="策略净值">
            <Chart option={{
              tooltip: { trigger: 'axis', valueFormatter: (value) => Number(value).toFixed(4) },
              xAxis: { type: 'category', data: dates, boundaryGap: false },
              yAxis: { type: 'value', scale: true, name: '净值' },
              series: [{ type: 'line', showSymbol: false, data: normalizedNav, name: '策略净值', color: '#2563eb' }],
            }} />
          </Panel>
          <Panel title="策略回撤">
            <Chart option={{
              tooltip: { trigger: 'axis', valueFormatter: (value) => pct(Number(value)) },
              xAxis: { type: 'category', data: dates, boundaryGap: false },
              yAxis: { type: 'value', scale: true, axisLabel: { formatter: (value) => `${(Number(value) * 100).toFixed(0)}%` } },
              series: [{ type: 'line', showSymbol: false, data: selectedDrawdown, areaStyle: {}, name: '回撤', color: '#dc2626' }],
            }} />
          </Panel>
          {visiblePositionCurve.length === 0 ? <>
            <Panel title="持仓资金占用率"><div className="empty-state">该报告没有逐日持仓快照，请重新运行策略生成新版报告。</div></Panel>
            <Panel title="各标的持股数量"><div className="empty-state">该报告没有逐日持仓快照，请重新运行策略生成新版报告。</div></Panel>
          </> : <>
            <Panel title="持仓资金占用率">
              <Chart option={{
                tooltip: { trigger: 'axis', valueFormatter: (value) => pct(Number(value)) },
                xAxis: { type: 'category', data: dates, boundaryGap: false },
                yAxis: { type: 'value', min: 0, max: 1, axisLabel: { formatter: (value) => `${(Number(value) * 100).toFixed(0)}%` } },
                series: [{ type: 'line', showSymbol: false, data: visiblePositionCurve.map((point) => point.capital_utilization), name: '资金占用率', areaStyle: { opacity: 0.12 }, color: '#0f766e' }],
              }} />
            </Panel>
            <Panel title="各标的持股数量">
              <Chart option={{
                tooltip: { trigger: 'axis' },
                legend: { type: 'scroll', top: 0 },
                xAxis: { type: 'category', data: dates, boundaryGap: false },
                yAxis: { type: 'value', min: 0, name: '份' },
                series: positionSymbols.map((symbol) => ({
                  type: 'line' as const,
                  showSymbol: false,
                  name: symbol,
                  data: visiblePositionCurve.map((point) => point.holdings.find((holding) => holding.symbol === symbol)?.quantity ?? 0),
                })),
              }} />
            </Panel>
          </>}
          {visiblePositionCurve.some((point) => point.holdings.some((holding) => (holding.stale_calendar_days ?? 0) > 0)) && <Panel title="陈旧估值记录"><div className="result-list">{visiblePositionCurve.flatMap((point) => point.holdings.filter((holding) => (holding.stale_calendar_days ?? 0) > 0).map((holding) => <div className="result-row" key={`${point.date}-${holding.symbol}`}><b>{point.date} · {holding.symbol}</b><small>最近收盘 {holding.mark_date ?? '日期未知'} · 距估值日 {holding.stale_calendar_days} 自然日 · 估值价 {holding.mark_price}</small></div>))}</div></Panel>}
        </>}
      </section>
      <Panel title="各标的盈亏">
        {instrumentPerformance.length === 0 ? exp.backtest.metrics.trade_count > 0
          ? <div className="empty-state">旧报告没有逐标的盈亏明细，请重新运行策略生成新版报告。</div>
          : <div className="empty-state">该回测没有交易，暂无标的盈亏记录。</div>
          : <>
            <div className="pnl-note">盈亏采用加权平均成本；买卖滑点已计入成交价，佣金与税计入对应成本/卖出净收入。浮动盈亏按期末最近收盘价估值。</div>
            <div className="table-wrap pnl-table"><table><thead><tr>
              <th>标的</th><th>买入数量</th><th>卖出数量</th><th>期末持有</th><th>期末市值</th><th>已实现盈亏</th><th>浮动盈亏</th><th>总盈亏</th>
            </tr></thead><tbody>{instrumentPerformance.map((item) => <tr key={item.symbol}>
              <td>{item.symbol}<small>{item.trade_count} 笔成交</small></td>
              <td>{item.buy_quantity.toLocaleString()}</td><td>{item.sell_quantity.toLocaleString()}</td><td>{item.final_quantity.toLocaleString()}</td>
              <td>{currency(item.market_value)}</td><td className={pnlClass(item.realized_pnl)}>{currency(item.realized_pnl)}</td>
              <td className={pnlClass(item.unrealized_pnl)}>{currency(item.unrealized_pnl)}</td><td className={pnlClass(item.total_pnl)}><b>{currency(item.total_pnl)}</b></td>
            </tr>)}</tbody><tfoot><tr><th colSpan={5}>合计</th><th>{currency(instrumentPerformance.reduce((sum, item) => sum + item.realized_pnl, 0))}</th>
              <th>{currency(instrumentPerformance.reduce((sum, item) => sum + item.unrealized_pnl, 0))}</th>
              <th>{currency(instrumentPerformance.reduce((sum, item) => sum + item.total_pnl, 0))}</th>
            </tr></tfoot></table></div>
          </>}
      </Panel>
      <section className="grid">
        <Panel title={`期末实际持仓（${finalPositions.length} / Top-${exp.config.strategy.top_n}）`}>
          <div className="result-list">
            {finalPositions.length === 0 ? <div className="empty-state">期末无正持仓。</div> : finalPositions.map(([symbol, quantity]) =>
              <div className="result-row" key={symbol}><b>{symbol}</b><small>{quantity.toLocaleString()} 份</small></div>) }
          </div>
          <small>这是回测最后记录的实际正持仓；末日收盘产生的待执行信号不会出现在这里。</small>
        </Panel>
        <Panel title="调仓延期（完整回测期）">
          {!exp.backtest.execution_status_mode || deferrals === undefined ? <div className="empty-state">旧报告未记录执行模式或延期审计，不能据此确认没有发生延期。</div>
            : deferrals.length === 0 ? <div className="empty-state">没有记录到旧仓卖出受阻的调仓延期。</div>
              : <div className="result-list">{deferrals.map((item, index) => <div className="result-row" key={`${item.attempt_date}-${index}`}>
                <b>{item.attempt_date} · {item.reason === 'held_position_missing_open' ? '旧持仓缺开盘价' : item.reason}</b>
                <small>决策 {item.decision_date} · 目标 {item.target_symbols.join('、') || '空目标'} · 阻塞 {item.blocked_symbols.join('、')}</small>
              </div>)}</div>}
        </Panel>
        <Panel title="未执行目标与订单（完整回测期）">
          {!exp.backtest.execution_status_mode || unexecuted === undefined ? <div className="empty-state">旧报告未记录执行模式或逐腿审计，不能据此确认所有订单均已执行。</div>
            : unexecuted.length === 0 ? <div className="empty-state">没有记录到未执行目标或订单。</div>
              : <div className="result-list">{unexecuted.map((item, index) => <div className="result-row" key={`${item.attempt_date}-${item.symbol}-${index}`}><b>{item.attempt_date} · {item.symbol} · {item.side}</b><small>决策 {item.decision_date} · 原因 {item.reason} · 目标数量 {item.desired_quantity ?? '未知'} · 状态 {item.trade_status ?? '未知'} · 来源 {item.status_sources ?? '无'}</small></div>)}</div>}
        </Panel>
      </section>
    </main>
  </>;
}

function currency(value: number) {
  return new Intl.NumberFormat('zh-CN', { style: 'currency', currency: 'CNY', maximumFractionDigits: 2 }).format(value);
}

function pnlClass(value: number) {
  return value > 0 ? 'pnl-positive' : value < 0 ? 'pnl-negative' : '';
}
