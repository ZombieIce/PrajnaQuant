# Tencent unadjusted daily transport

`tencent-transport` is the shared request seam for the existing warehouse and the new Raw collector. It owns the bounded Tencent HTTP request and returns the source URL plus the response's original bytes. It does not parse or rewrite the payload, create Raw objects, publish Datasets, or infer market coverage.

## Request contract

`DailyRequest` takes an explicit `sh` or `sz` prefix followed by six digits and an inclusive date range. The start must not follow the end, and the date difference must be less than 800 calendar days. The request parameter remains `{symbol},day,{start},{end},640,`.

The transport sends a GET to `/appstock/app/fqkline/get` with the `param` query, `Referer: https://gu.qq.com/`, and the existing browser user agent. It uses a 10-second connection timeout, a 180-second request timeout, HTTP/1 only, and reqwest's existing default redirect policy. Candidate hosts and order are fixed:

1. `https://web.ifzq.gtimg.cn`
2. `https://proxy.finance.qq.com/ifzqgtimg`
3. `https://ifzq.gtimg.cn`

With the legacy/default entry point, request errors and HTTP status errors advance to the next listed candidate. An empty body or a body rejected by the caller's acceptance predicate also advances. A body-read or client-build error returns immediately, matching the former warehouse call. If no candidate is accepted, `FetchError` includes the legacy error message and per-candidate kind, host, status code when available, and message.

Callers that enforce a source-stop rule can use fetch_unadjusted_daily_with_policy. Its callback sees request/status failures and rejected payloads before the loop can try another candidate. Readable HTTP error bodies are passed to the callback too, so it can detect CAPTCHA pages regardless of status; request failures and unreadable error bodies pass None. Return CandidateAction::Stop to receive FetchErrorKind::PolicyStopped. Keep state in the callback across logical cells and reset it after a successful return. The legacy fetch_unadjusted_daily entry point always continues fallback to preserve the warehouse contract.

The caller supplies the payload acceptance predicate. The legacy warehouse uses its existing `data`-object check; a Raw collector can apply its own JSON shape check without making this crate depend on `serde_json`. A successful `RawDailyResponse` contains the final URL after redirects and the exact body bytes; it does not contain a vendor publication timestamp or local `observed_at`.

This extraction adds no host, proxy, header variation, or access-control workaround. Higher-level collection must preserve the repository's source-stop policy and must not treat an access-control response as permission to invent another route.

## Synthetic verification

Tests inject a deterministic `HttpTransport` and synthetic response bytes, and route the production reqwest adapter to an ephemeral loopback server. They cover stock and ETF request inputs, the fixed request contract, exact byte/source preservation, empty and invalid payload fallback, network and HTTP failures (including CAPTCHA bodies on 429/503 responses), and date/symbol validation. They do not make real external network requests or establish Tencent availability or data coverage.
