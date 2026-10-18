# Shenzhen fund price-limit rule catalogue v1

This catalogue covers `[2022-10-01, 2026-10-01)` and is embedded as
`szse-fund-limit-rules@1` in the Rust research crate. It records verified
exchange rule periods and category definitions; it does **not** claim to contain
point-in-time fund membership.

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

- **深证上〔2020〕515号**, §2.9, published 2020-06-12: [official PDF](https://www.szse.cn/lawrules/rule/repeal/rules/P020231230545310237980.pdf), SHA-256 `060ecac8b7de29e579e58d61d350dcb1dfb08a72efaa5520b6b9f0a06b97eb1f`.
- **深证上〔2021〕337号**, published 2021-03-31, effective 2021-04-06: [official notice](https://www.szse.cn/lawrules/rule/trade/current/t20210331_585336.html) and [official PDF](https://docs.static.szse.cn/www/lawrules/rule/trade/current/W020210331716400647184.pdf), SHA-256 `2540ea6c44b6c624089bbac4ea08d161e88dde929858db68caf7282733521632`. Relevant provisions include §§3.3.11, 3.3.15–3.3.16 and 5.2.3(3).
- **深证上〔2023〕98号**, published 2023-02-17, effective 2023-04-10: [official notice](https://www.szse.cn/lawrules/rule/repeal/rules/t20230217_598773.html), [official PDF](https://docs.static.szse.cn/www/lawrules/rule/repeal/rules/W020230217564423808793.pdf), and [SZSE effective-date event](https://www.szse.cn/aboutus/trends/news/t20230411_599874.html). PDF SHA-256 `7018114a6e11deb239c2a72e71e49defc6e8841b3e2c093b3bbf809282c67222`. Relevant provisions include §§3.3.11, 3.3.13–3.3.15, 3.3.19 and 5.2.3(3).
- **深证上〔2026〕551号**, published 2026-04-24, effective 2026-07-06: [official notice](https://www.szse.cn/lawrules/rule/trade/current/t20260424_620190.html) and [official PDF](https://docs.static.szse.cn/www/lawrules/rule/trade/current/W020260424690713155663.pdf), SHA-256 `9b66f8b0db70f84a25ef1ccb4ee2351001724e408117552d75f6d8993483c586`. Relevant provisions include §§3.3.11, 3.3.13–3.3.15, 3.3.19 and 5.2.3(3).
- SZSE identifies 2020-08-24 as the first registration-era ChiNext listing date in its [official event notice](https://www.szse.cn/aboutus/trends/news/t20200824_580927.html).

## Fund membership and fail-closed behavior

The legal texts describe category criteria and, for some categories, state that
SZSE publishes a corresponding fund list. This implementation did not verify
an official historical per-fund list or point-in-time category assignment
dataset for the covered window. Consequently, the embedded
`category_assignments` array is empty. It contains no inferred memberships,
fund names, or ticker-based classifications.

Callers must supply an explicit lifecycle-specific category assignment with
its half-open effective interval, official document number, and frozen
evidence. The resolver returns no rule for missing, unknown, overlapping,
out-of-coverage, or out-of-interval assignments. V2 execution also requires
all rule and category evidence to be available before the session open;
unsupported or late evidence yields UNKNOWN. Pre-window date-only regulatory
evidence is treated as known immediately before the evaluated open only when
its publication date precedes its documented effective date and that effective
date precedes the evaluated session. This does not relax availability checks
for fund membership evidence.

CI uses only synthetic assignments and synthetic market inputs. Passing those
tests verifies fail-closed resolution, date boundaries, price arithmetic, and
identity wiring; it does not establish real fund membership, source coverage,
actual opening tradability, or investment performance.
