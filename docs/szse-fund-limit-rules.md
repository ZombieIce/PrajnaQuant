# Shenzhen fund price-limit rule catalogue v1

This catalogue covers `[2022-10-01, 2026-10-01)` and is published as
[`configs/mvp-6/limit_rules_v1.json`](../configs/mvp-6/limit_rules_v1.json),
versioned `szse-fund-limit-rules@1`. It records verified exchange rule periods,
the source citation and evidence level for each row, plus fixed-list membership
coverage. Official raw files and their hashes remain local.

## Rule periods

| Applicable period | Verified categories | Limit | Source |
| --- | --- | ---: | --- |
| 2022-10-01–2023-04-10 | Default fund category | 10% | SZSE Trading Rules, 深证上〔2021〕337号 |
| 2022-10-01–2023-04-10 | ChiNext index ETF/LOF and class-B structured-fund category; qualifying ChiNext/20%-limit-stock LOFs | 20% | SZSE ChiNext special rules, 深证上〔2020〕515号, with tick/calculation support from 深证上〔2021〕337号 |
| 2023-04-10–2026-07-06 | Default fund category | 10% | SZSE Trading Rules, 深证上〔2023〕98号 |
| 2023-04-10–2026-07-06 | Qualifying ChiNext/20%-limit-stock index ETFs and LOFs; qualifying LOFs with at least 80% of non-cash assets in those securities | 20% | SZSE Trading Rules, 深证上〔2023〕98号 |
| 2026-07-06–2026-10-01 | The same default and special categories | 10% / 20% | SZSE Trading Rules, 深证上〔2026〕551号 |

The 2020 special rules took effect for the registration-based ChiNext listing
regime on 2020-08-24, the first listing day for that regime. The 2023 rules
took effect on 2023-04-10 with the first registration-based Main Board listings.
The 2026 rules take effect on 2026-07-06 and supersede the 2023 rules. The
catalogue keeps the original legal effective dates while limiting resolution to
the stated v1 coverage interval.

All listed rows use a 0.001 CNY tick, nearest-tick half-up rounding, and the
exchange minimum one-tick difference between reference and limit price. When
the rounded difference is less than one tick, the limit is moved to one tick
from reference; a computed lower limit below one tick is raised to one tick.
The first trading session is subject to the stated fund limit and uses the
previous net asset value as its reference price; no stock IPO first-day
exemption is inferred.

## Primary sources

- **深证上〔2020〕515号**, §2.9, published 2020-06-12: [official PDF](https://www.szse.cn/lawrules/rule/repeal/rules/P020231230545310237980.pdf). Primary evidence.
- **深证上〔2021〕337号**, published 2021-03-31, effective 2021-04-06: [official notice](https://www.szse.cn/lawrules/rule/trade/current/t20210331_585336.html) and [official PDF](https://docs.static.szse.cn/www/lawrules/rule/trade/current/W020210331716400647184.pdf). Relevant provisions include §§3.3.11, 3.3.15–3.3.16 and 5.2.3(3). Primary evidence.
- **深证上〔2023〕98号**, published 2023-02-17, effective 2023-04-10: [official notice](https://www.szse.cn/lawrules/rule/repeal/rules/t20230217_598773.html), [official PDF](https://docs.static.szse.cn/www/lawrules/rule/repeal/rules/W020230217564423808793.pdf), and [SZSE effective-date event](https://www.szse.cn/aboutus/trends/news/t20230411_599874.html). Relevant provisions include §§3.3.11, 3.3.13–3.3.15, 3.3.19 and 5.2.3(3). Primary evidence.
- **深证上〔2026〕551号**, published 2026-04-24, effective 2026-07-06: [official notice](https://www.szse.cn/lawrules/rule/trade/current/t20260424_620190.html) and [official PDF](https://docs.static.szse.cn/www/lawrules/rule/trade/current/W020260424690713155663.pdf). Relevant provisions include §§3.3.11, 3.3.13–3.3.15, 3.3.19 and 5.2.3(3). Primary evidence.
- SZSE identifies 2020-08-24 as the first registration-era ChiNext listing date in its [official event notice](https://www.szse.cn/aboutus/trends/news/t20200824_580927.html).

## Fund membership and fail-closed behavior

The legal texts describe category criteria and, for some categories, state that
SZSE publishes a corresponding fund list. This implementation did not verify
an official historical per-fund list or point-in-time category assignment
dataset for the covered window. The published `category_assignments` array is
empty. No class is inferred from a code or exchange-traded name. Each fixed-list
code is explicitly UNKNOWN throughout the catalogue coverage window; this is a
coverage-window statement, not a claimed listing/termination interval.

| ETF code | Covered interval | Status | Reason |
| --- | --- | --- | --- |
| 159919 | 2022-10-01–2026-10-01 | UNKNOWN | No source-backed PIT category assignment; no limit percentage inferred |
| 159560 | 2022-10-01–2026-10-01 | UNKNOWN | No source-backed PIT category assignment; no limit percentage inferred |
| 159522 | 2022-10-01–2026-10-01 | UNKNOWN | No source-backed PIT category assignment; no limit percentage inferred |
| 159612 | 2022-10-01–2026-10-01 | UNKNOWN | No source-backed PIT category assignment; no limit percentage inferred |
| 159952 | 2022-10-01–2026-10-01 | UNKNOWN | No source-backed PIT category assignment; no limit percentage inferred |

The unresolved instrument-level assignments are explicit for each regime:

| Period | Category/period coverage still unknown |
| --- | --- |
| 2022-10-01–2023-04-10 | Which funds are default 10%; which index ETFs, LOFs, class-B structured-fund shares, or qualifying 80%-asset LOFs meet either 20% criterion |
| 2023-04-10–2026-07-06 | Which funds are default 10%; which qualifying ChiNext/20%-limit-stock index ETFs or LOFs, and which qualifying 80%-asset LOFs, receive 20% |
| 2026-07-06–2026-10-01 | The same instrument-level assignments under the 2026 rules |
| Any date in scope | Any category not enumerated in the applicable SZSE rule row, or any interval for which the official rule cannot be established |

Callers must supply an explicit lifecycle-specific category assignment with
its half-open effective interval, official document number, evidence level,
and local frozen membership evidence. Primary assignments require a SZSE text
or list that directly names the fund. Secondary assignments require the fund
contract/prospectus tracking-index statement together with the SZSE index-class
rule. The public rule table stores regulation citations without Raw hashes; the
rule version and citations are bound into the resolved `LimitRule`. The resolver
returns no rule for missing, unknown, overlapping,
out-of-coverage, or out-of-interval assignments. V2 execution also requires
all rule and category evidence to be available before the session open;
unsupported or late evidence yields UNKNOWN. Pre-window date-only regulatory
evidence is treated as known immediately before the evaluated open only when
its publication date precedes its documented effective date and that effective
date precedes the evaluated session. This does not relax availability checks
for fund membership evidence. The assignment identity hashes the instrument,
category, half-open interval, document number, and evidence together; altering
one component without rebuilding the assignment identity is rejected. This
integrity binding does not replace source-text adjudication.

CI uses only synthetic assignments and synthetic market inputs. Passing those
tests verifies fail-closed resolution, date boundaries, price arithmetic, and
both evidence levels using synthetic assignments; it does not establish real fund membership, source coverage,
actual opening tradability, or investment performance.
