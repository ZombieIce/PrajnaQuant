# Independent Shanghai/Shenzhen session calendar input

This MVP-7 input is independent of market Raw and the frozen MVP-6 SZSE
normalizer. A local operator supplies one JSON document containing **both**
`XSHG` and `XSHE` exactly once, each with its own source, inclusive coverage
and complete list of exchange sessions over that coverage. Missing dates within
coverage mean the supplying source asserts there is no session; an empty list
is allowed for a wholly closed range. This contract does not verify that the
source is authoritative or its coverage complete. It never infers a session,
holiday or suspension from prices, another venue, or weekdays.

```json
{
  "schema_version": 1,
  "venues": [
    {
      "venue_id": "XSHG",
      "source": {"source_id": "synthetic-sh", "source_version": "1", "source_ref": "fixture-sh"},
      "coverage_start": "2026-01-01",
      "coverage_end": "2026-01-03",
      "sessions": [{"session_date": "2026-01-02", "ts_open": "2026-01-02T09:30:00+08:00", "ts_close": "2026-01-02T15:00:00+08:00"}]
    },
    {
      "venue_id": "XSHE",
      "source": {"source_id": "synthetic-sz", "source_version": "1", "source_ref": "fixture-sz"},
      "coverage_start": "2026-01-01",
      "coverage_end": "2026-01-03",
      "sessions": [{"session_date": "2026-01-03", "ts_open": "2026-01-03T09:30:00+08:00", "ts_close": "2026-01-03T15:00:00+08:00"}]
    }
  ]
}
```

The example is synthetic, not an assertion about actual trading dates. Source
fields must be nonempty and identify the independently sourced input and its
version/reference. Each date must be within its venue's coverage, occur at
most once even when duplicated entries agree, and have explicit-offset RFC
3339 timestamps with `open < close`, both on the declared Asia/Shanghai local
date. Coverage start must not exceed end. Unsupported/missing/duplicate venues,
invalid dates or times, unknown fields and conflicting dates are rejected.
Separate venues may legitimately list different sessions or bounds.

Import from a local file into an explicitly chosen local root outside the
MVP-6 archive. The receipt name contains only ASCII letters, digits, `_`, `-`;
it cannot overwrite an existing receipt. The command prints no version hash.

```sh
cargo run -p prajna-data --bin prajna-session-calendar -- \
  import --root /path/to/local/calendar-lake \
  --input /path/to/local/calendar.json --receipt first-import
```

The receipt is stored locally at `session-calendars/receipts/first-import.txt`.
`CalendarStore::read_receipt` reads its version; `get` loads source and
coverage for exactly that version, and `sessions(version, venue, start, end)`
returns that venue's Domain Sessions for an inclusive covered range. A missing
venue, invalid range, or any part outside coverage is an error. No method
selects the latest version. A later publisher must persist the explicitly
selected calendar version and use that same identity on rebuild.

The version covers schema, each venue's source, declared coverage and sessions.
Venue/session order and equivalent timestamp offsets normalize before hashing;
reimporting equivalent content is idempotent, while a revision produces a new
version and leaves the old one readable. Versions live under the configured
root with integrity checked on every read; reimporting the same input into a
fresh root reproduces the same version. The receipt is a local selector, not a
mutable latest pointer. Keep real input, source details and version identities
in the configured local area; do not copy them into public artifacts or tool
output. No real source or full history was validated by this contract. Daily
Normalizer and Dataset publication remain later work.
