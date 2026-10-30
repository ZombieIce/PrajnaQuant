# ADR 0033：Static Universe 固定公司行动账本

- 状态：Accepted，2026-10-30；#307，扩展 ADR 0023/0032。

## 决定

生产冻结选择与 Experiment 定义可显式带 `corporate_actions`：

```json
{"version":"caf:sha256:HEX","dividend_tax_rate":"0.1"}
```

版本从配置的 lake 根读取并验证，不选 latest；事实导入可显式使用该本机根。
精确税率必填、范围 `[0,1)`；这是固定平税率模拟，不宣称支持历史个人差别税。
版本、规范化税率与 `cn-static-action-ledger@1` 进入 Experiment/Run 身份。
只有单 Venue Static Universe 的 Fast Event `lot + full_target` 支持本绑定；
Vector、parity、Accurate、多 Venue 或同时指定 MVP-6 PIT 均明确拒绝。
冻结选择在显式绑定时设 full_target，移除 entry_budget；省略绑定保留原定义与身份。
Buy-and-hold 仍只有首个正常决策，不因分红发放增加调仓。

账本只处理冻结 Universe 成员，使用原始成交与估值价格；复权序列只供已有研究信号。
同一 CorporateActionProcessor、交易账户与 lot 费用规划处理事件，新增 `fast_event@15`；
未绑定的引擎和 MVP-6 PIT 分支继续原 revision、行为与身份。标准结果仍由现有存储路径
保存公司行动表、账户估值和 Summary；固定行动 Run Spec 随 manifest 保存，原事实版本可追溯来源，不建立第二套绩效或账户计算。

## 事实、缺项与时序

covered/no_events 声明须完整包含评价区间，否则结果覆盖未知；不拼接范围为完整证明。
已知完整事件仍可用于不完整结果。缺必要字段、事件日在运行区间内却不是 Session、
登记与除权同日（既有账本不支持其日内顺序）时，不补造事件，保存逐事件不完整原因。
缺少匹配证券或范围的版本不猜身份，输出未知覆盖与不完整结果。
同证券同日多个开盘事件仍由既有账本拒绝未确定顺序。

版本固定保留事件来源/历史 available_at；observed_at 不作发布时间。
桥接的旧 CorporateActionRow 以事实 available_at 的日期填其必填 announced_date，
announced_at 保持空；它不对外宣称公告日期，不进入信号可知性或 TRI。
原事实版本才是来源及历史可用性权威；账本按实际权益日期结算，不用发布时刻推迟权益。

事件顺序沿用 ADR 0023：除息入应收/拆分→raw open 正常执行→close 估值、日期级
发放与登记→close Decision。登记锁定实际持仓；卖出不取消既有权利，除息新买不补权利。
应收计入 NAV、排除投资预算；发放 close 仅转现金，下一正常调仓才可使用。
拆分按实际整数份向下取整、记录 residual 与 corporate_action_rounding，事件不产生交易费。

## 验证与限制

独立合成手算覆盖登记、应收、分红税、close 发放、正常再投资、拆分和费用；
生产 freeze→run Standard→result/read_run_table 验证持久结果与版本隔离。
已有 MVP-6 公司行动/Vector-Fast 对拍和身份回归继续运行。
这是结构化声明范围的模拟验证，不证明真实公告全集、历史 PIT Universe 或真实总回报。
本票无页面；浏览器明细产品仍由 #310 承接。
