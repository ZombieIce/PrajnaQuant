export const dailyPriceMode = 'daily_price_simulation_v1';
export const dailyPriceLabel = '日线价格模拟';

export default function ExecutionMode({ daily, onChange, disabled, strictLabel = '既有执行模式' }: {
  daily: boolean; onChange: (daily: boolean) => void; disabled?: boolean; strictLabel?: string;
}) {
  return <>
    <label>执行模式<select aria-label="执行模式" disabled={disabled} value={daily ? dailyPriceMode : ''} onChange={(event) => onChange(event.target.value === dailyPriceMode)}>
      <option value="">{strictLabel}</option>
      <option value={dailyPriceMode}>{dailyPriceLabel}</option>
    </select></label>
    {daily && <p className="hint">假设历史收盘可用，T 日复权收盘形成信号，下一交易 Session 以未复权开盘价为模拟成交基础。报告扣费后价格收益，跨除权日受价格跳变影响。</p>}
  </>;
}
