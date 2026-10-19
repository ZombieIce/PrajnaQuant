---
name: review-pr
description: "Independently review a ticket pull request against its issue, spec, and repository standards."
disable-model-invocation: true
---

Review a ticket PR as an independent reviewer in a separate agent session.

## Preconditions

- This session has not implemented or committed to the PR branch and does not inherit implementation-session context.
- The project owner starts this session after the implementer posts a ready-for-review comment.
- The inputs are the PR number, originating issue, spec pointer, and repository documents. Read them from GitHub and the repository; do not rely on implementer summaries as evidence.

## Workflow

1. **Establish scope.** Read the PR, originating issue and spec, applicable `AGENTS.md`, and relevant repository standards. Record the latest PR head SHA and build the acceptance coverage below from the original requirements. Completion criterion: every acceptance criterion and any additional required spec behavior is identified, with changed-file scope explicit.
2. **Inspect independently.** Create a separate worktree, check out the PR branch, and use the merge-base with `origin/main` as the fixed point. Run `/code-review` so Spec and Standards are reviewed as separate axes, then reconcile the results against the complete acceptance coverage. Inspect all mapped requirements before concluding, rather than stopping at the first blocker. Completion criterion: both axes have findings or an explicit zero-finding result, and every mapped requirement has evidence or a recorded gap.
3. **Verify findings.** Recheck each finding against the actual diff, code, tests, and documents; remove false positives and note relevant test results. Run applicable commands from `AGENTS.md` Testing Rules. Apply Finding classification below. Completion criterion: every published finding has a requirement/standard, repository evidence, impact and blocking/non-blocking classification; verification limits remain explicit.
4. **Publish review.** Add one PR comment beginning `**Independent reviewer:**`, with separate `Spec` and `Standards` sections, acceptance coverage under `Spec`, blocking/non-blocking labels for each finding, and an overall review conclusion. Publish all verified findings from this pass together. Do not push changes to the PR branch. Completion criterion: the comment identifies the reviewed head SHA, accounts for every mapped requirement and records both axes, remaining verification limits and the conclusion. A zero-finding result does not turn unverified criteria into verified ones. If the PR head changed during review, identify the reviewed SHA as stale and require review of the new head before merge.
   After publishing, remove the review worktree with `git worktree remove --force <path> && git worktree prune` (it holds no work to preserve; `--force` discards `target/`).
5. **Re-review fixes.** After the project owner starts a fresh re-review session for a new pushed head, read the prior review comments and compare the prior reviewed head with the new head. Verify every prior blocking finding and record the disposition of prior non-blocking suggestions. Inspect changed behavior, neighboring failure modes and affected acceptance requirements, including required outputs; revisit broader scope when the change or a discovered omission warrants it. Update acceptance coverage and add a follow-up comment beginning `**Independent reviewer:**` with the reviewed SHA, both axes and an overall conclusion. Identify each new blocker as a prior omission, a regression introduced by the fix, or a requirement-interpretation change, with evidence; if its origin is uncertain, say so. An interpretation change must cite the original requirement or an explicit owner clarification. New valid blockers remain reportable regardless of origin or earlier approval. Completion criterion: prior blockers are resolved or remain open with evidence, optional suggestions have recorded dispositions, affected requirements have updated coverage, and the conclusion applies to the latest pushed head. The project owner merges.

## Acceptance coverage

Publish a compact table: `Issue/spec requirement | implementation evidence | verification/output evidence | status`. Derive it independently; the implementer's mapping is a navigation aid, not proof. For each requirement distinguish implemented behavior, partial/missing implementation and unverified behavior, citing code and checks actually run. State synthetic versus real-data limits where relevant.

Trace required behavior from its actual entry point to its observable output. When a workflow or persisted report is required, verify input loading, invocation and output writing rather than accepting an isolated helper. Probe meaningful negative and boundary cases in the affected contract: missing versus empty values, ordering, identity/lifecycle isolation or unsupported evidence when applicable. Read existing checks before proposing new automation; prefer regression tests for deterministic failures over another prose rule.

## Finding classification

- **Blocking:** an evidenced failure to meet the Issue/spec or a mandatory repository standard, with a concrete consequence. Cite the requirement and counterexample or code path. An unmet required verification remains an acceptance gap even if disclosed. An environmental limit is not by itself evidence of a code defect; state what remains unverified and whether the ticket requires that verification before acceptance.
- **Non-blocking:** optional design/style improvements or follow-up work outside the required acceptance scope. Label judgment calls explicitly; a justified deferral does not prevent review completion. Avoid prescribing unrelated refactors as a prerequisite to approval.

Report findings against the existing requirements. Propose any scope or acceptance change for owner clarification rather than silently enlarging or weakening the ticket. Retain legitimate blockers without imposing a fixed review-round limit.
