---
name: review-pr
description: "Independently review a ticket PR against its issue, spec, and repository standards."
disable-model-invocation: true
---

Review when started directly by the owner or by an owner-authorized ticket-loop after an implementer ready comment. Each initial review/re-review is a new session that has not implemented/committed to this branch and does not inherit implementation context. Follow [AGENTS](../../../AGENTS.md) and the shared [tracker contract](../../../docs/agents/issue-tracker.md); findings come from current GitHub/repository sources, not implementer summaries.

## Workflow

1. **Pin scope.** Read the current PR/head, Issue/comments, spec and applicable standards; pin head and merge-base with latest `origin/main`. Create your own review worktree at that head, never borrow the implementation worktree. Derive acceptance coverage below independently. Fixed-head/spec inputs may be read by relevant sections; refresh changed inputs, blockers, head and checks before conclusions. Done: all requirements and changed-file scope identified at a known head.
2. **Inspect both axes.** Apply the risk rule below. For ordinary tickets one independent reviewer examines both Spec and Standards; no mandatory code-review delegation. For high risk, run [code-review](../code-review/SKILL.md) against the pinned merge-base with separate Spec/Standards agents, then reconcile their results yourself. Inspect every mapped requirement before concluding. Done: both axes have findings or explicit zero findings, and every requirement has evidence or a gap.
3. **Verify.** Recheck findings against diff, code, tests and documents; remove false positives. Use [verification rules](../../../docs/agents/verification.md) and classify findings below. Done: each finding has requirement/standard, evidence, consequence and classification; required unverified behavior remains a gap.
4. **Publish and stop.** Post one comment beginning `**Independent reviewer:**` with reviewed head SHA, risk/verification scope, separate `Spec` and `Standards` sections, coverage under Spec, classified findings, verification limits and overall conclusion. Publish all verified findings together. Do not push to the PR branch. Refresh PR head/checks; if changed, mark this SHA stale and require review of the new head. Clean your review worktree under the tracker contract, report any retention and stop. Done: GitHub has both axes/coverage/conclusion for the named SHA; zero findings do not turn unverified criteria into verified ones.
5. **Re-review incrementally.** A new owner/coordinator-started session reads prior review comments and compares prior reviewed head to current head. Verify every prior blocker and each suggestion's disposition; inspect changed behavior, neighboring failure modes and affected outputs/acceptance. Revisit broader scope for omissions or newly discovered risk, upgrading to parallel axes when warranted. Publish as above. Identify new blockers as prior omission, fix regression, interpretation change, or uncertain origin; interpretation changes cite original requirements/owner clarification. Done: prior findings have evidenced dispositions, affected coverage is current, and the conclusion applies to the named latest head. The owner merges; the loop's three-cycle coordination limit does not waive legitimate blockers.

## Risk rule

High risk includes changes to execution timing, price/return semantics, accounts/costs, identity compatibility or contracts across modules. Expand independent review into parallel Spec/Standards axes for these tickets; upgrade if inspection discovers such risk. Ordinary changes, including a thin vertical slice without these contract risks, can use one reviewer. State the judgment and verification scope in handoff/review; uncertainty with concrete risk warrants escalation, not silent model downgrades. This is separate from the implementer's own acceptance self-check.

## Acceptance coverage

Publish `Issue/spec requirement | implementation evidence | verification/output evidence | status`. Derive from original requirements; the implementer mapping is navigation, not proof. Distinguish implemented, partial/missing and unverified, citing actual checks and synthetic/real-data limits.

Trace actual entry point to observable output, including input loading/invocation/writing for workflows and persisted reports. Probe relevant missing/empty values, ordering, identity/lifecycle isolation or unsupported evidence. Read existing checks before proposing automation; prefer regression tests for deterministic failures over another prose rule.

## Finding classification

- **Blocking:** evidenced failure of Issue/spec or mandatory repository standard with a concrete consequence; cite requirement and counterexample/path. Missing required verification remains an acceptance gap even when disclosed. Environment limits alone are not code defects: specify what remains unverified and whether acceptance requires it.
- **Non-blocking:** optional design/style suggestions or follow-up outside acceptance. Label judgment calls; justified deferral does not prevent completion. Do not require unrelated refactors.

Propose scope/acceptance changes for owner clarification; retain valid blockers without enlarging or weakening requirements. Reviewer conclusions remain PR comments; the coordinator relays them without rewriting them.
