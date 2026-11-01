# ADR 0038: Fixed-pool production updates and explicit supplier inputs

Status: Accepted scope and implementation contract under the
[#313 owner decision](https://github.com/ZombieIce/PrajnaQuant/issues/313#issuecomment-6553232202).
This decision is not a real-data acceptance claim.

A pool update must produce one all-member Dataset usable by existing research,
not merely a list of independent securities. The minimum production assembler is
therefore included in the same ticket as update tasks and the page. A new
`cn-stock-etf-daily@3` rule composes the existing pinned production v1/v2
Normalizers over Raw inputs and publishes through the ordinary atomic manifest
boundary. Old single-security rules/identities/receipts are unchanged. Rejected
alternatives are copying Parquet, test-only publisher wrappers, dropping failed
members and implicitly mixing previously published versions.

The plan fixes pool/version/members, dates and calendar/security/status evidence.
Attempts preserve their consumed file snapshots and results separately. An explicit
same-plan retry may reuse successful, scope-checked member receipts; failure must
not erase the last usable pool or rewrite a frozen experiment.

Supplier adjustment and corporate-action acquisition are **not** authorized by
this update workflow. Explicit local files/fact versions remain an operational
prerequisite. A versioned source file with no project DSV may be adapted into a
new validated import after raw publication, retaining source bytes and provenance;
an existing bound adjusted object may not be rebound. No adjustment mathematics
or account behavior changes. Separate execution completion, raw publication,
price readiness, action coverage and total-return applicability avoid conflating
successful orchestration with usable research.

The workbench uses the existing shared queue and subprocess/process-group
contract, not a second scheduler or Rust core in the old service. Missing files
finish the attempt with explicit needs-input results instead of holding the
worker. The [operational contract](../contracts/pool-updates.md) defines commands,
storage, failure boundaries and synthetic verification scope.
