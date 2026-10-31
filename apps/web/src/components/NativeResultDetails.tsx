import { useState, type ReactNode } from 'react';

type Row = Record<string, unknown>;
export type NativeDetails = {
  details?: Record<string, Row[] | undefined>;
  allocations?: Row[] | null;
  units?: { currency?: string | null };
  tables: { saved: string[]; not_saved: string[]; not_produced_by_engine: string[] };
};
const labels: Record<string, string> = {
  session_date: '执行/估值日期', decision_session: '决策日期', valuation_point: '估值点',
  instrument_id: '证券', portfolio_id: '组合', quantity: '数量（份）', price: '成交/估值价',
  market_value: '持仓市值', weight: '权重（比例）', cash: '账户现金（含未分配资本）', equity: '总权益',
  cash_weight: '现金权重（比例）', dividend_receivable: '分红应收（不可花费）',
  unallocated_capital: '未分配资本（账户级，不能由组合直接支配）',
  liquidation_receivable: '清算应收（不可花费）', dividend_receivable_weight: '分红应收权重（比例）',
  liquidation_receivable_weight: '清算应收权重（比例）', positions: '持仓',
  kind: '状态', record_type: '记录类型', order: '订单', fill: '成交', costs: '费用',
  EmbeddedSlippage: '滑点已计入成交价', SeparateSlippage: '滑点单独扣费',
  side: '方向', id: '订单号', submitted_at: '提交时间（Unix ns）', executed_at: '成交时间（Unix ns）',
  raw_price: '原始价', raw_notional: '原始名义金额', trade_notional: '成交名义金额',
  pricing: '滑点计价模式', execution_price: '含滑点成交价', commission: '比例佣金',
  minimum_commission_top_up: '最低佣金补足', tax: '税', slippage: '滑点金额',
  total_fees: '扣费合计', cash_delta: '现金变动', fee_basis: '计费基数', turnover_basis: '换手基数',
  fee_side: '计费方向', blocked: '受阻证券', skipped_buys: '跳过买入', reason: '原因',
  applied: '已执行目标（比例）', targets: '待执行目标（比例）', is_retry: '是否重试',
  blocked_entries: '受阻买入', blocked_entry_reason: '买入受阻原因', ts_open: '开盘时间（Unix ns）',
  attempted_at: '尝试时间（Unix ns）', requested_quantity: '请求数量（份）', actual_quantity: '实际数量（份）',
  quantity_kind: '数量类型', attempt: '尝试', pit_sizing: '数量/费用审计',
  weight_unavailable_reason: '权重不可用原因', available_cash_weight: '可用现金权重（比例）',
  portfolio_components: '开盘资金构成', portfolio_components_close: '收盘资金构成',
  cost: '成本率（相对前一 NAV）', turnover: '换手率', net_return: '净收益率', nav: '净值',
};
const states: Record<string, string> = {
  deferred: '延期', retry_deferred: '重试延期', executed: '已执行', entries_retried: '买入重试',
  retryable_execution: '可重试执行', pending_at_end: '期末待执行', risk_exit_pending_at_end: '期末风险退出待执行',
  attempt: '执行尝试', sizing_adjustment: '数量调整（非成交）', risk_exit: '风险退出',
  order: '订单', Buy: '买入', Sell: '卖出', lot_rounding: '整手舍入', insufficient_cash: '现金不足',
  held_instrument_not_executable: '已持有证券不可执行', instrument_not_executable: '证券不可执行',
  missing_or_nonpositive_equity: '总权益缺失或不为正', open_after: '开盘执行后', close: '收盘',
};
function value(input: unknown): ReactNode {
  if (input === null || input === undefined) return '不可用（未记录）';
  if (typeof input === 'boolean') return input ? '是' : '否';
  if (typeof input !== 'object') return states[String(input)] ?? String(input);
  if (Array.isArray(input)) return input.length ? input.map((item, i) => <span key={i}>{i > 0 ? '；' : ''}{value(item)}</span>) : '无';
  const entries = Object.entries(input as Row);
  return entries.length ? <dl>{entries.map(([key, item]) => <div key={key}><dt>{labels[key] ?? key}</dt><dd>{value(item)}</dd></div>)}</dl> : '无';
}
function unavailable(run: NativeDetails, name: string) {
  if (run.tables.not_saved.includes(name)) return '未保存：当前保存级别没有该明细，不能推断为零交易。';
  if (run.tables.not_produced_by_engine.includes(name)) return '引擎不产生：该引擎没有此类原生明细，不能推断为零交易。';
  return '不可用：未取得已保存明细，不能推断为零交易。';
}
function SavedTable({ run, name, title, empty, rows, testId }: { run: NativeDetails; name: string; title: string; empty: string; rows?: Row[]; testId?: string }) {
  const source = run.details?.[name];
  const available = run.tables.saved.includes(name) && Array.isArray(source);
  const shown = rows ?? source;
  const [offset, setOffset] = useState(0);
  return <section data-testid={testId ?? `native-${name}`}><h3>{title}</h3>
    {!available ? <p role="status">{unavailable(run, name)}</p>
      : !shown?.length ? <p>{empty}</p>
        : <div className="table-wrap"><table><thead><tr><th>序号</th><th>已保存明细</th></tr></thead>
          <tbody>{shown.slice(offset, offset + 20).map((row, i) => <tr key={i}><td>{offset + i + 1}</td><td>{value(row)}</td></tr>)}</tbody></table>
          <p>记录 {offset + 1}–{Math.min(offset + 20, shown.length)} / {shown.length}
            <button type="button" disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - 20))}>上一页</button>
            <button type="button" disabled={offset + 20 >= shown.length} onClick={() => setOffset(offset + 20)}>下一页</button></p></div>}
  </section>;
}

export default function NativeResultDetails({ run, sessions }: { run: NativeDetails; sessions?: Row[] }) {
  const isEventEngine = run.tables.saved.includes('account_valuations') || run.tables.not_saved.includes('account_valuations');
  const orders = run.details?.orders;
  const dates = [...new Set(run.allocations?.map(row => String(row.session_date)) ?? [])].sort();
  const [date, setDate] = useState(dates.at(-1) ?? '');
  return <section className="native-result-details" aria-label="持仓现金与执行明细">
    <h2>持仓、现金和执行明细</h2>
    <p>金额单位：{run.units?.currency ?? '未记录（金额币种不可用）'}；数量单位：份；权重为 0–1 比例，总权益包含应收，应收不可花费。目标权重不代表实际持仓。Summary 执行事件计数包含全现金目标，不能当作实际 Fill 数量。</p>
    <p>成交价已经包含滑点时，滑点金额仅作解释，不能再加扣一次；最低佣金补足与比例佣金分别列示。成本率与金额不可相加。</p>
    {isEventEngine ? <section data-testid="native-allocations"><h3>实际持仓与现金（Rust 估值投影）</h3>
      {!run.tables.saved.includes('account_valuations') || !Array.isArray(run.allocations)
        ? <p>{unavailable(run, 'account_valuations')}</p>
        : !run.allocations.length ? <p>已保存估值表为空，实际持仓不可用。</p>
          : <><label>持仓估值日期<select aria-label="持仓估值日期" value={date} onChange={change => setDate(change.target.value)}>{dates.map(day => <option key={day}>{day}</option>)}</select></label><div className="table-wrap"><table><thead><tr><th>日期</th><th>估值点</th><th>现金 / 应收 / 实际权重</th></tr></thead>
            <tbody>{run.allocations.filter(row => row.session_date === date).map((row, i) => <tr key={i}><td>{String(row.session_date)}</td><td>{value(row.valuation_point)}</td>
              <td>{value(Object.fromEntries(Object.entries(row).filter(([key]) => !['session_date', 'valuation_point'].includes(key) && (key !== 'weight_unavailable_reason' || row[key] !== null))))}</td></tr>)}</tbody></table></div></>}
    </section> : <>
      <SavedTable run={run} name="weights_after_execution" title="实际开盘执行后权重（Vector）" empty="已保存实际权重表为空；不代表明细未保存。" />
      <p>Vector 不产生现金金额账本；以下仅为保存的资金比例。非账本 Vector 或旧 Session 未保存资金构成时不可用。</p>
      <SavedTable run={{ ...run, details: { ...run.details, sessions: sessions } }} name="sessions" title="Vector 现金/应收比例及逐日成本率" empty="已保存 Session 表为空：资金比例不可用。" rows={sessions?.map(row => ({ session_date: row.session_date, portfolio_components: row.portfolio_components, portfolio_components_close: row.portfolio_components_close, cost: row.cost }))} />
    </>}
    <SavedTable run={run} name="fills" title="实际成交与成交成本" empty="已保存成交表为空：该表确认无成交；仍需核对独立费用结算及延期/跳过记录。" />
    <SavedTable run={run} name="fee_settlements" title="独立费用结算（无成交数量）" empty="已保存费用结算表为空：无独立费用结算。" />
    <SavedTable run={run} name={isEventEngine ? 'orders' : 'executions'} title="订单、延期、跳过与数量调整" empty="已保存执行审计表为空：无审计记录。" />
    {isEventEngine ? <SavedTable run={run} name="orders" testId="native-pending" title="期末 pending 明细（目标 / 风险退出）" rows={orders?.filter(row => row.kind === 'pending_at_end' || row.record_type === 'risk_exit_pending_at_end')} empty="已保存订单审计中没有期末 pending 记录。" />
      : <SavedTable run={run} name="pending_at_end" title="期末 pending 目标" empty="已保存期末目标表为空：无期末目标；风险退出仍须核对执行审计。" />}
  </section>;
}
