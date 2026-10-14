# MVP-6：深市 ETF PIT 涨跌幅规则与数据源授权/许可审计

- Issue: #172（Parent #171）
- 审计范围：仅限 PIT 涨跌幅规则来源与适用期间（10%/20%、首日例外、除权除息参考价/最小变动单位）+ TDX（tdx.com.cn）、腾讯（qt.gtimg.cn / web.ifzq.gtimg.cn）、cninfo（cninfo.com.cn）、深交所（szse.cn）的转载/再分发授权条款审计。
- **不在本报告范围**：cninfo 公告分页完整性、159000～159999 代码复用交叉核对、TDX/腾讯逐日行情抽样一致性 —— 以上由 #171/#172 主审计（parent）单独核实，避免重复。
- 原始抓取字节仅冻结在仓库之外：`~/.copilot/session-state/311c4d49-bcd8-41ea-93e8-f7dd5d774f8f/files/172-audit/rules-license/`（本仓库不提交任何原始网页/行情字节、PDF 或版权文本片段）。
- 检索窗口：2026-10-14 08:52–09:05 UTC（2026-10-14 16:52–17:05 +08:00，Asia/Shanghai）。本报告所有“检索时间”均落在该窗口内，单独条目不再重复标注日期。
- 网络环境：工具沙箱对部分搜索引擎域名（so.com、DuckDub html 端点、english.szse.cn）存在 DNS/连接失败（curl 退出码 000 / fetch 工具报 transport-level failure），按约定标记为 **Unresolved（网络失败）**，不得视为“来源不存在”。

## 结论总表（adopt / defer / reject / unresolved）

| # | 主题 | 结论 | 依据摘要 |
|---|------|------|----------|
| 1 | SZSE 官网法律声明（存在性与可达性） | **adopt（仅限"声明存在且内容已取得"这一事实）** | 页面确认可达（HTTP 200），条文已完整获取并冻结（见下）。**注意**：此处"adopt"仅表示"该法律声明的存在、URL 与条文内容已核实"，**不等于"adopt 该来源允许我们做任何特定行为"**——具体行为许可见下一行。 |
| 1b | SZSE 官网法律声明对"自动化批量抓取（内部非商业）/再分发"的许可范围 | **unresolved（无确定法律结论）** | 条文第三条结构是："非商业目的可浏览、下载"（明示许可，**不含"抓取"措辞**）+ "不得**以向他人出售牟利为目的**…包括…电子抓取系统…转载…散发"（禁止性清单，**牟利目的是该清单的限定语**，语法上前置于"电子抓取系统"等全部并列项）。因此：(a) 条文**没有**无条件禁止非商业性下载/电子检索——牟利限定语客观存在；(b) 条文也**没有**明确许可"自动化批量抓取"这一行为模式本身（许可措辞仅覆盖"浏览、下载"）；(c) "批量抓取是否构成牟利目的之外的另一类受限行为"、"内部非商业审计性批量抓取是否被允许"、"公开再分发是否被允许"均**无法从条文文本得出确定性法律结论**。按保守原则统一标记为 unresolved/restricted，不作为已获许可对待，也不作为已被禁止的确定结论对待。 |
| 2 | SZSE ETF 涨跌幅比例（10% vs 20%）原始规则条文 | **unresolved** | 深交所官网“本所业务规则”页面（`/lawrules/rule/new/index.html`）为前端 SPA，规则列表通过客户端渲染后请求内部 API 获取，纯 HTTP 抓取未能定位且未执行 JS，无法取得条文原文；多次尝试搜索引擎（Baidu/Bing/搜狗/DuckDuckGo/360）定位官方 PDF 链接均未成功（见“检索尝试记录”）。 |
| 3 | ETF 首日是否适用涨跌幅限制（不可套用个股新股上市首日无涨跌幅限制的惯例） | **unresolved**（方向性线索仅来自三级来源，未核实至主源） | 仅检索到证券从业资格考试题库类网页的摘要片段（非深交所/证监会官方文本），其表述为“ETF上市首日实行10%的涨跌幅限制，并非无涨跌幅限制”。该片段与 issue 提示的“不得假设个股 IPO 豁免适用于 ETF”方向一致，但来源层级不足（三级转述），**不采纳为结论依据**，仅记录作为后续定向检索提示。 |
| 4 | 除权除息参考价、最小变动单位等细则 | **unresolved** | 同 #2，原始条文页面不可经纯 HTTP 抓取获得；本轮未发现可验证的一手条文。 |
| 5 | TDX（通达信）官方行情授权声明 | **adopt（有限）** | `tdx.com.cn/article/license.html`（静态页，可直接抓取）列出通达信（深圳市财富趋势科技股份有限公司）自称已取得“深圳证券交易所 Level1/Level2 行情授权”等多家交易所/指数公司授权，用于**说明 TDX 对上游数据的使用是经授权**；但该页面未写明下游用户（第三方抓取/复用者）的可用范围，**不能据此推出我们可以再分发或商用** —— 许可缺位 ≠ 允许。 |
| 6 | TDX 用户协议/EULA 的具体使用范围条款 | **unresolved** | `tdx.com.cn/about/yhxy/index.html` 为 Vue SPA 壳（`<div id="app">`+外链 JS），纯 HTTP 抓取仅得到空壳，未渲染出协议正文；无法确认是否允许自动化批量下载全市场行情包。 |
| 7 | 腾讯 qt.gtimg.cn 可达性与条款 | **adopt（事实性，非许可）** | `https://qt.gtimg.cn/q=sz159001` 返回 200，为未公开文档化的内部接口（无 API Key、无公开说明书），仅证明“公开可达”，**不构成公开转载/再分发许可**。 |
| 8 | 腾讯 web.ifzq.gtimg.cn 可达性 | **unresolved（观察到保护性响应，未证实具体触发原因）** | 本次会话中两次请求均收到 HTTP 501 并跳转至 `waf.tencent.com/501page.html`（即便附加 `Referer: https://gu.qq.com/`）。**这仅是对"观察到的保护性/WAF 响应"的客观记录**；是否由 IP 信誉、UA 指纹、请求频率或其它因素触发，本次未做任何对照实验（如更换 UA/IP/频率重试），**不构成已证实的限流/风控触发归因**。按规则标记 unresolved，既不得当作"接口不存在"，也不得反推具体触发机制。 |
| 9 | 腾讯通用服务条款（覆盖行情数据复用） | **unresolved** | 尝试访问 `tencent.com/zh-cn/articles/userpolicy.html`（404）、`tencent.com/zh-cn/privacypolicy.html`（404）、`qq.com/questions/pact.shtml`（302 重定向至无关页面）均未取得条款正文；`gu.qq.com` 前台页为 Vite/Vue SPA 壳，法律声明/版权链接未在静态 HTML 中出现。 |
| 10 | cninfo（巨潮资讯网）免责声明 | **adopt（仅限"声明存在且内容已取得"这一事实；不等于已获抓取/再分发许可）** | `http://www.cninfo.com.cn/new/index`（HTTP 200；HTTPS 同路径返回 403，与 parent 报告的“HTTPS 间歇性失败”现象一致）首页内嵌“免责声明”对话框原文：仅保证信息准确可靠但不担保完整/准确性，未使用方不得据此索赔；**未见任何明示的公开转载/批量分发授权条款**，运营方为“深圳证券信息有限公司”。条款缺位按本任务约定**不解读为默示许可**，抓取/再分发许可本身仍为 unresolved。 |
| 11 | cninfo 公告查询 API（`new/hisAnnouncement/query`）单次探测 | **adopt（事实性）** | 本次仅做 1 次定界探测（非覆盖/分页审计，避免与 parent 重复）：POST 请求返回 HTTP 200，但 `totalRecordNum=0`（空结果，可能是参数/列值不匹配，而非接口不可用）。未就此扩大测试，覆盖完整性结论留给 parent 报告。 |
| 12 | SZSE 官网整体可达性 | **adopt（事实性）** | `www.szse.cn` 主域、`/lawrules/rule/index.html`、`/application/laws/index.html`、`/application/publicindex/footer|header/index.html` 均 HTTP 200 可达；`english.szse.cn` 域名解析失败（DNS lookup failed，非 404），标记 unresolved。 |

## 一手来源与证据明细

### A. SZSE（深圳证券交易所）

1. **法律声明（已确认获取，静态页面，非 SPA）**
   - URL：`http://www.szse.cn/application/laws/index.html`
   - 检索方式/状态：`curl -A Mozilla/5.0 -m 20` → HTTP 200
   - 冻结文件：`szse/szse_legal_notice.html`（10,063 字节，SHA256 `5398c463263421a98cbf90918dcaa16d6d70ef80f2052a9d0c19c1606ab27936`）
   - 关键条款（原文转述，不逐字复制全文到仓库）：
     - 第一条：网站内容及相关软件程序知识产权归深交所所有，受中国知识产权法保护。
     - 第三条（结构需精确区分两个分句）：
       1. **许可分句**：“在遵守中国有关法律与本声明的前提下，任何机构或者个人可基于非商业目的浏览、下载本网站的内容。”——**这是一句明确的肯定性许可，覆盖"非商业目的的浏览、下载"，且未使用"禁止""不得"等否定措辞**。
       2. **禁止分句**：“未经深圳证券交易所书面许可，任何机构或者个人**不得以向他人出售牟利为目的**，使用本网站的任何内容，此种使用包括但不限于拷贝、下载、存贮、通过硬拷贝或电子抓取系统、发送、转换、出租、演示、转载、复制、修改、销售、传播、出版或任何其它形式的散发。”——**"以向他人出售牟利为目的"是该分句的限定语**，在语法上前置并修饰后面的整个行为清单（含"电子抓取系统"）。
     - 第五条：深交所对内容准确性/完整性不作保证，不承担因使用内容产生的损失责任。
   - **修正后的结论（替代本报告早前版本中的不精确表述）**：
     - 本声明**并未无条件禁止非商业性的下载/电子检索**——许可分句明确允许"非商业目的"的浏览、下载，禁止分句的限定语是"牟利目的"，而非"任何自动化手段"。此前报告草稿中将"电子抓取系统"脱离牟利限定语单独引用、暗示"即便非商业自动化抓取也一律受限"的表述**不准确，现予以更正**。
     - 但这**不等于**声明已明确许可"自动化批量抓取"这一具体行为模式：许可分句的措辞仅为"浏览、下载"，未出现"抓取""批量获取""自动化采集"等用语；因此"非商业内部审计用途的自动化批量抓取是否被允许"**仍无法从条文文本得出确定性法律结论**。
     - "公开再分发/商业使用"则明确落入禁止分句的"牟利目的"典型场景（出售、传播、出版等），风险更高，但具体是否构成"牟利"取决于使用场景，本报告不做法律定性。
     - **最终结论**：内部非商业自动化批量抓取 = **unresolved**（条文未明确许可亦未明确禁止）；公开再分发/商业化使用 = **unresolved/restricted**（更接近禁止分句的典型场景，但非终局法律意见）。两者均不得在拿到深交所书面确认前默认为"已获许可"。

2. **业务规则列表页（SPA，内容未获取）**
   - URL：`https://www.szse.cn/lawrules/rule/new/index.html`
   - 检索方式/状态：HTTP 200，但返回内容为前端框架壳（Vue/类似 SPA 路由），规则列表通过客户端脚本异步加载；纯文本 HTTP 抓取未命中任何规则标题/链接（`grep "ETF\|涨跌幅"` 零命中）。
   - 冻结文件：`szse/szse_rule_new.html`（36,843 字节，SHA256 `64f8904a112f19783f6b75e3eb0407a392f5add90b344aa0d3d46b6ec4b49ed9`）
   - 结论：**unresolved** — 需要可执行 JS 的浏览器环境或已知内部 API 端点才能继续；本会话工具不支持渲染 JS，未能定位具体 API。

3. **首页/Footer/Header 片段（辅助定位导航，均确认可达）**
   - `https://www.szse.cn/index/index.html` → 200，冻结 `szse/szse_home.html`（90,574 字节，SHA256 `f7288b3ccc3ba9dcc5a528b57dd4de86ecc93d30fc858b6da7262e1c35b20ace`）
   - `https://www.szse.cn/application/publicindex/footer/index.html` → 200，冻结 `szse/szse_footer.html`（15,078 字节，SHA256 `ff3dbca2dad85e612c8532c1d2c4dcf72bfdd6aea0c627071a72896f0276cc36`）——用于定位“法律声明”“本所业务规则”等导航链接的真实路径。
   - `https://www.szse.cn/application/publicindex/header/index.html` → 200，冻结 `szse/szse_header.html`（107,955 字节，SHA256 `db4419e0ba3e36f2a4d3e74ae6437fe259f62d323b231d40e2f21e2205178dbd`）

4. **ETF 涨跌幅条文（10%/20%、首日例外、除权除息参考价）——未能取得一手条文**
   - 结论：**unresolved（技术性阻断，非来源不存在）**
   - 已尝试且失败的路径（记录命令以便复核，不构成伪造 URL）：
     - 猜测路径 `lawrules/rule/listedrules/zqgs/t20150706_571762.html` 等 → 404（人工猜测路径，已放弃继续猜测）
     - `szse.cn/api/search/full*`、`szse.cn/sitemap.xml` → 404
     - `english.szse.cn/*` → DNS 解析失败（`nodename nor servname provided`）
     - 搜索引擎探测：
       - Baidu（`baidu.com/s?wd=...`）：能取得摘要片段（证实存在《深圳证券交易所交易所交易型开放式指数基金业务实施细则》，生效日期 2015-07-06），但**搜索结果页为渲染后内容，未能提取出对应 szse.cn 一手 URL**（结果区块无可解析的 `href`），故该片段**不作为已核实的一手证据引用**，仅记录作为后续定向检索线索。
       - Bing/Sogou：返回与查询无关的缓存内容（可能受地域/语言影响），未获得有效结果。
       - DuckDuckGo（`html.duckduckgo.com`、`lite.duckduckgo.com`）：返回反爬验证码页（"Please email the following code to error-lite@..."），**unresolved（反爬拦截）**。
       - 360 搜索（so.com）：`curl`/`web_fetch` 均报连接失败（DNS/transport failure），**unresolved（网络失败）**。
   - 由此，本报告**不对 10% vs 20%、首日涨跌幅例外、除权除息参考价/最小变动单位给出可引用的一手条文结论**，仅记录为 unresolved，并建议后续改用具备 JS 渲染能力的抓取方式（如无头浏览器）重新定向审计，而非凭记忆或三级来源代入 Spec。

### B. TDX（通达信，tdx.com.cn）

1. **行情授权声明页（静态，已确认获取）**
   - URL：`https://www.tdx.com.cn/article/license.html`
   - 状态：HTTP 200（`curl -A Mozilla/5.0 -m 20`）
   - 冻结文件：`tdx/tdx_license.html`（28,383 字节，SHA256 `b8f6004fba3ea135e0a5d8807b9d74507fca3ab9c26adc60b2dcb942a1c45cd8`）
   - 内容摘要：页面列出“国内行情授权”“国际行情授权”两组清单，声明已获得上交所、深交所、北交所、港交所、中金所、郑商所、大商所、上期所、上能所、广期所、全国股转系统、中证指数、恒生指数、中国外汇交易中心等 Level1/Level2 行情授权，并附各交易所/指数公司官网链接作为“出处佐证”（而非直接给出授权合同文本）。
   - 结论：该页面**只证明 TDX 自身对上游数据源的使用具备授权声明**，**未就下游（第三方通过 TDX 软件/数据包再抓取、再分发）给出任何许可条款**；不能从中推导出“我们可以合法批量获取/再分发 TDX 全市场数据包”。许可缺位 ≠ 允许，需单独核实 TDX 用户协议（见下）。

2. **用户协议/隐私政策入口（Vue SPA 壳，正文未获取）**
   - 首页隐私政策入口：`https://www.tdx.com.cn/about/yhxy/index.html?tabindex=1`（curl 返回传输失败，退出码对应 transport error）
   - 重试：`https://www.tdx.com.cn/about/yhxy/index.html`（去参数）→ HTTP 200，但响应体仅为 `<div id="app"></div>` + 外链 JS bundle，协议正文由客户端渲染，纯 HTTP 抓取无法得到文本。
   - 冻结文件：`tdx/tdx_yhxy0.html` / `tdx/tdx_yhxy_main.html`（均 1,750 字节，SHA256 `0fd44a6966a80ebefc86aeb0a3720087eb6f903d3ce07214af95cda5c0f205be`，二者内容一致）
   - 结论：**unresolved** —— 无法确认 TDX 是否允许第三方自动化批量下载全市场行情包用于内部研究或再分发；在拿到正文前不得假设允许。

3. **主域可达性**
   - `https://www.tdx.com.cn/` → 200，冻结 `tdx/tdx_home.html`（31,981 字节，SHA256 `9e98fa0174357ba363a6380f1c7e146bc718300f0742fec0b6527e9671595583`）
   - `http://www.tdx.com.cn/` → 301（到 https），冻结 `tdx/tdx_home2.html`（162 字节，SHA256 `9e17cb15dd75bbbd5dbb984eda674863c3b10ab72613cf8a39a00c3e11a8492a`）

### C. 腾讯（qt.gtimg.cn / web.ifzq.gtimg.cn）

1. **qt.gtimg.cn 实时行情接口**
   - 请求：`https://qt.gtimg.cn/q=sz159001`
   - 状态：HTTP 200，返回类 CSV 文本（含代码、名称、现价、五档盘口等字段），**无需任何鉴权头/API Key**
   - 冻结文件：`tencent/qt_sample.txt`（493 字节，SHA256 `2ab0e96423cc626dbc409fb1689ccadd567a343a30466dffc1a874aad4692a1a`）
   - 结论：**仅证明“公开可达”（技术事实）**，未发现任何公开接口文档或使用条款；接口形态（`q=` 查询参数、无版本号）符合“非公开文档化的内部/逆向接口”特征。**公开可达不等于被许可批量抓取或再分发**，尤其在缺乏官方 API 文档的情况下，默认应按“未授权”处理。
   - `https://qt.gtimg.cn/` 根路径 → HTTP 200，但响应体仅 12 字节（空白/占位），冻结 `tencent/qt_root.html`（SHA256 `fd02fcbdda556a69aa36ccbce50d7d8b5f4bcfdda71ccc0afc739bdc9ee34b35`），未发现任何服务说明文本。

2. **web.ifzq.gtimg.cn 历史 K 线接口**
   - 请求 1：`https://web.ifzq.gtimg.cn/appstock/app/fqkline/get?param=sz159001,day,,,10,qfq`（无 Referer）→ HTTP 501，响应体为腾讯 WAF 跳转脚本（`waf.tencent.com/501page.html`），冻结 `tencent/ifzq_sample.txt`（323 字节，SHA256 `573085d3faa59abb4f07080ba247b030ec87bd101aa383ac98f828d1d6fb0f62`）
   - 请求 2：同 URL，附加 `Referer: https://gu.qq.com/` → 仍 HTTP 501，WAF 拦截，冻结 `tencent/ifzq_sample2.txt`（323 字节，SHA256 `ed72f3212c4f52aebb578d531ec41789aaa88b776044b65f8538f0e949a5a559`）
   - 结论：**unresolved（仅记录观察到的保护性响应，不构成已证实的触发原因归因）**。两次请求均收到 HTTP 501 + WAF 跳转页面，这是**客观观察到的服务端保护性响应**；本次未做 UA/IP/频率的对照实验，**无法证实**具体是由请求频率、IP 信誉、UA 指纹或其它因素触发，故不得断言"触发了限流/风控策略"这一具体因果机制。是否对所有客户端/所有时段均如此拦截，需要后续专项验证（含对照组）才能确认，本报告不下绝对结论。

3. **腾讯通用服务条款定位尝试（均未获取到条款正文）**
   - `https://www.tencent.com/zh-cn/articles/userpolicy.html` → 404
   - `https://www.tencent.com/zh-cn/privacypolicy.html` → 404
   - `https://www.qq.com/questions/pact.shtml` → 302 重定向至 `https://h5.news.qq.com/static/babyhome.htm`（与条款无关的育儿活动页），冻结 `tencent/qq_pact.html`（4,183 字节，SHA256 `dd3ca8f2a7ee2f282645a07d06786a1fdb6f0caa1e090063805dc51f525c2464`）
   - `https://gu.qq.com/`（腾讯证券行情中心前台）→ 跟随重定向后 HTTP 200，但响应体为 Vite/Vue SPA 壳（`<div id="app"></div>` + 外链 bundle.js），法律声明/版权链接未出现在静态 HTML 中；冻结 `tencent/gu_qq_home.html`（2,727 字节，SHA256 `7f86741059fd5ab365b8157a120f0570836e99f3fe3bbed88a4999a5e39929ba`）
   - 结论：**unresolved** —— 未能定位腾讯对 `gtimg.cn` 系列行情接口的公开许可条款；在未取得明确许可前，应默认按“未授权的内部研究探测”对待，不得用于对外重分发。

### D. cninfo（巨潮资讯网）

> 注：cninfo 的公告分页完整性、覆盖范围统计属于 parent（#171/#172 主审计）范围，本节仅做许可/条款核实，不重复其分页/覆盖结论。

1. **首页内嵌免责声明（已确认获取）**
   - 请求：`http://www.cninfo.com.cn/new/index`（**注意**：HTTP 明文协议成功，HTTPS 同路径在本次会话返回 403 — 与 parent 报告中“HTTPS 间歇性失败”现象一致，提示该域名对 HTTPS 请求存在额外的风控/证书或 WAF 策略，而非域名不可达）
   - 状态：HTTP（非 S）200
   - 冻结文件：`cninfo/cninfo_home.html`（109,518 字节，SHA256 `4f073f7b0348581897ecba1aff4ff6d4295a7eb54f532b2503437df54cde62c4`）
   - 免责声明原文位置：页面内联 Vue `el-dialog`（`id="statement"`），运营方落款为“深圳证券信息有限公司”（粤B2-20050313号）
   - 条款摘要：
     - 声明网站竭力保证证券市场信息准确可靠，但**不担保（无论明示、默示、法定或其他形式）其准确性和完整性**；使用方不得就使用其信息引起的损失向运营方索赔。
     - 不保证外部链接内容的准确性/完整性，相关风险责任自负。
     - **未见任何关于允许/禁止自动化抓取、批量下载、再分发或商用的明示条款** —— 这是“条款缺位”而非“默示许可”，按任务要求不得将此解读为允许公开再分发。

2. **公告查询接口单次探测（非覆盖审计，仅许可/可达性定界）**
   - 请求：`POST http://www.cninfo.com.cn/new/hisAnnouncement/query`（`pageNum=1&pageSize=5&column=szse&tabName=fulltext&searchkey=ETF上市交易公告书&sortName=pubdate&sortType=desc&isHLtitle=true`，附加 `Referer: http://www.cninfo.com.cn/...`）
   - 状态：HTTP 200，响应体 `{"totalRecordNum":0,...}`（空结果；可能是 `column`/`plate` 参数取值不匹配，未进一步调参排查，避免与 parent 的分页审计重复）
   - 结论：**事实性 adopt**（证明该 POST 端点在本次请求下可达且不触发 403），具体覆盖率/分页边界结论留给 parent 报告，本报告不作延伸。

## 原始数据冻结清单（仅本地，未提交仓库）

冻结目录：`~/.copilot/session-state/311c4d49-bcd8-41ea-93e8-f7dd5d774f8f/files/172-audit/rules-license/`

| 文件 | 字节数 | SHA256 |
|------|--------|--------|
| `cninfo/cninfo_home.html` | 109,518 | `4f073f7b0348581897ecba1aff4ff6d4295a7eb54f532b2503437df54cde62c4` |
| `szse/szse_footer.html` | 15,078 | `ff3dbca2dad85e612c8532c1d2c4dcf72bfdd6aea0c627071a72896f0276cc36` |
| `szse/szse_header.html` | 107,955 | `db4419e0ba3e36f2a4d3e74ae6437fe259f62d323b231d40e2f21e2205178dbd` |
| `szse/szse_home.html` | 90,574 | `f7288b3ccc3ba9dcc5a528b57dd4de86ecc93d30fc858b6da7262e1c35b20ace` |
| `szse/szse_legal_notice.html` | 10,063 | `5398c463263421a98cbf90918dcaa16d6d70ef80f2052a9d0c19c1606ab27936` |
| `szse/szse_rule_new.html` | 36,843 | `64f8904a112f19783f6b75e3eb0407a392f5add90b344aa0d3d46b6ec4b49ed9` |
| `tdx/tdx_home.html` | 31,981 | `9e98fa0174357ba363a6380f1c7e146bc718300f0742fec0b6527e9671595583` |
| `tdx/tdx_home2.html` | 162 | `9e17cb15dd75bbbd5dbb984eda674863c3b10ab72613cf8a39a00c3e11a8492a` |
| `tdx/tdx_license.html` | 28,383 | `b8f6004fba3ea135e0a5d8807b9d74507fca3ab9c26adc60b2dcb942a1c45cd8` |
| `tdx/tdx_yhxy0.html` | 1,750 | `0fd44a6966a80ebefc86aeb0a3720087eb6f903d3ce07214af95cda5c0f205be` |
| `tdx/tdx_yhxy_main.html` | 1,750 | `0fd44a6966a80ebefc86aeb0a3720087eb6f903d3ce07214af95cda5c0f205be` |
| `tencent/gu_qq_home.html` | 2,727 | `7f86741059fd5ab365b8157a120f0570836e99f3fe3bbed88a4999a5e39929ba` |
| `tencent/ifzq_sample.txt` | 323 | `573085d3faa59abb4f07080ba247b030ec87bd101aa383ac98f828d1d6fb0f62` |
| `tencent/ifzq_sample2.txt` | 323 | `ed72f3212c4f52aebb578d531ec41789aaa88b776044b65f8538f0e949a5a559` |
| `tencent/qq_pact.html` | 4,183 | `dd3ca8f2a7ee2f282645a07d06786a1fdb6f0caa1e090063805dc51f525c2464` |
| `tencent/qt_root.html` | 12 | `fd02fcbdda556a69aa36ccbce50d7d8b5f4bcfdda71ccc0afc739bdc9ee34b35` |
| `tencent/qt_sample.txt` | 493 | `2ab0e96423cc626dbc409fb1689ccadd567a343a30466dffc1a874aad4692a1a` |
| `cninfo/cninfo_query_probe1.json`（补冻结） | 165 | `c2a890bbf3a6a53ab02ddc6c1794bf1c72ba45799fe9f59dcc5ba2cc18467114` |
| `cninfo/cninfo_query_probe2.json`（补冻结） | 165 | `c2a890bbf3a6a53ab02ddc6c1794bf1c72ba45799fe9f59dcc5ba2cc18467114` |
| `szse/szse_404_probe_aboutus_trade_rules.html`（补冻结） | 8,675 | `d5086c31d9a9d3da1e5a28e3c5f516c997b4508b20dee99ccda33b1b75d3976b` |
| `fetch_log.txt`（探测命令/失败路径流水账，供复核） | 567 | `1238da732af5daadb596e66f23c442e3ff52f273fd93c0f753b50f4057f77034` |

所有原始响应字节均未复制进本仓库；本报告不包含任何逐字复制的版权正文、行情原始数据或 PDF 内容，仅为条款要点转述与技术性元数据（状态码/字节数/哈希）。

## 已知缺口：未冻结的原始响应体（诚实披露，非最终判断）

以下探测在会话中**观察到了状态码/响应内容**，但**未按规范落盘到冻结目录并计算哈希**，属于本轮审计的记录缺口，列出以便复核或后续补全，不得将其误读为“已完整冻结”：

| 探测对象 | URL / 方式 | 观察到的状态 | 缺口说明 |
|---|---|---|---|
| SZSE 猜测路径（多数仍未冻结） | `lawrules/rule/listedrules/zqgs/t20150706_571762.html` 等（人工猜测，已放弃） | 404 | 响应体被 `-o /dev/null` 丢弃，未落盘/未哈希（仍 unresolved） |
| ~~SZSE 猜测路径（单例，已补冻结）~~ | `aboutus/trade/rules/index.html` | 404 | **已补冻结**：`szse/szse_404_probe_aboutus_trade_rules.html`（8,675 字节，SHA256 `d5086c31d9a9d3da1e5a28e3c5f516c997b4508b20dee99ccda33b1b75d3976b`）—— 内容为深交所标准 404 页面模板，**不含规则条文**，仅证明该路径不存在，不改变 unresolved 结论 |
| SZSE 搜索 API 猜测 | `szse.cn/api/search/full*` | 404（首 500 字节曾打印到终端） | 仅终端可见，未写入冻结目录文件，无哈希 |
| SZSE sitemap/robots | `szse.cn/sitemap.xml`、`szse.cn/robots.txt` | sitemap 404（含 body）；robots 空 | sitemap 响应体曾打印到终端但未落盘；robots 为空响应 |
| SZSE 英文站 | `english.szse.cn/*` | DNS 解析失败（无响应体，预期内） | 无响应体可冻结，非缺口，仅记录以防混淆 |
| TDX 隐私政策入口（带参数） | `tdx.com.cn/about/yhxy/index.html?tabindex=1` | 传输失败（无响应体，预期内） | 无响应体可冻结 |
| 腾讯通用条款 | `tencent.com/zh-cn/articles/userpolicy.html`、`tencent.com/zh-cn/privacypolicy.html` | 404 | 响应体被 `-o /dev/null` 丢弃，未落盘/未哈希 |
| cninfo 备用声明路径 | `new/information/disclaimer`（404）、`new/commonUrl?url=disclosure/statement`（500） | 404 / 500 | 响应体被 `-o /dev/null` 丢弃，未落盘/未哈希 |
| cninfo HTTPS 同路径 | `https://www.cninfo.com.cn/new/index` | 403 | 本轮本地核查：响应体当时即以 `-o /dev/null` 丢弃，无任何本地临时文件残留可供补冻结，**保持 unresolved（字节不可恢复）**（仅 HTTP 明文版本 `cninfo_home.html` 已冻结并哈希） |
| ~~cninfo 查询探测~~（已补冻结） | `new/hisAnnouncement/query`（两次参数变体） | 200，空结果 JSON | **已补冻结**：`cninfo/cninfo_query_probe1.json`（165 字节，SHA256 `c2a890bbf3a6a53ab02ddc6c1794bf1c72ba45799fe9f59dcc5ba2cc18467114`）、`cninfo/cninfo_query_probe2.json`（165 字节，同一 SHA256，两次探测响应体完全一致） |
| 搜索引擎结果页 | Baidu/Bing/Sogou/DuckDuckGo/360 对多个查询词的检索结果 | 多为 200（含反爬验证码页、无关缓存内容等） | 由 `web_fetch` 工具保存至系统临时目录（`/var/folders/...`），**未复制进本次指定的冻结目录，未计算哈希**；其中引用的"2015年业务细则生效日期"等内容仅为三级来源摘要，本报告已声明不采纳为结论依据，但原始检索结果页本身也未被规范冻结 |
| 腾讯 so.com 根路径测试 | `https://gtimg.cn/`、`https://www.so.com/s?...` | 000（transport failure，预期内） | 无响应体可冻结 |

**披露的影响**：上述缺口均为"状态码已知但响应体未规范冻结"，不改变本报告任何 adopt/unresolved 结论的方向（均已在结论中标记为 unresolved 或明确排除为非一手证据），但为保证可复核性，后续如需二次核实应优先针对此清单重新抓取并补齐冻结文件与哈希，而非仅凭本报告的文字转述。

## 主要阻断与后续建议

1. **SZSE/TDX/腾讯三方的核心正文（规则条文、用户协议）均托管在客户端渲染（SPA）页面或受 WAF/风控保护的端点之后**，当前工具链（纯 HTTP `curl`/`web_fetch`，无 JS 执行能力）无法取得。建议后续使用具备无头浏览器能力的抓取方式重新定向审计，而不是依赖记忆或题库类三级来源代入 Spec。
2. **ETF 涨跌幅 10% vs 20%、首日例外、除权除息参考价/最小变动单位** —— 本轮审计**未能取得任何一手条文**，按任务要求保持 **unresolved**，不得缩短窗口或替换来源；在该问题解决前，不应在正式 ingestion 中硬编码这些数值。
3. **licensing 结论（已按本轮更正）**：SZSE 官网法律声明**并未无条件禁止非商业性下载/电子检索**（许可分句明确允许"非商业目的浏览、下载"；禁止分句的限定语是"以牟利为目的"），但**也未明确许可"自动化批量抓取"这一行为模式本身**——"内部非商业自动化抓取"与"公开再分发/商业使用"均标记为 **unresolved/restricted**，不作为已获许可对待，亦不作为已被禁止的确定法律结论对待。TDX 仅证明自身获得上游交易所授权，未明确下游再分发权限，用户协议正文未取得（unresolved）。腾讯 `gtimg.cn` 系列为无文档化内部接口；`web.ifzq.gtimg.cn` 观察到 WAF 保护性响应，具体触发原因未证实（unresolved，非确定性风控归因）。cninfo 仅有通用免责声明，无明示的抓取/再分发许可（条款缺位，非默示许可）。**综合结论：四个来源均未发现明确允许"自动化批量抓取"或"公开再分发"的条款；"内部研究探测的合规性"与"公开发布/再分发的合规性"均应作为 unresolved/restricted 对待，不得仅凭"公开可访问"或"未被条文明确禁止"推定已获许可。**
4. 网络环境本身存在不稳定性（HTTPS 间歇性 403/失败、搜索引擎域名解析失败），与 parent 报告观察一致；本报告涉及的所有"unresolved"均已标注具体失败形式（DNS/连接失败 vs HTTP 403 vs WAF 拦截 vs SPA 空壳），避免与"来源不存在"混淆。
