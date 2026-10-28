---
name: implement-ticket
description: "Implement one GitHub ticket and hand off its PR for independent review."
disable-model-invocation: true
---

Implement one ticket when started directly by the owner or delegated by an owner-authorized ticket-loop. Follow [AGENTS](../../../AGENTS.md) and the shared [tracker contract](../../../docs/agents/issue-tracker.md) for current GitHub reads, roles, final-head review and worktree lifecycle. The implementer never launches or acts as the independent reviewer.

## Workflow

1. **Claim.** Read the current Issue/comments and referenced blockers through an available GitHub connector or CLI. Confirm open, unblocked, exactly one triage label `ready-for-agent`; assign yourself and post a start comment. Done: current eligibility, assignee and comment are visible on GitHub.
2. **Prepare.** Fetch latest `origin/main`; create a dedicated `<issue>-<slug>` branch/worktree. Read spec, relevant source/tests and applicable standards. Follow the shared [scope rule and six-item check](../../../docs/agents/ticket-scope.md): reuse the ticket's scope evidence, refresh only changed inputs, and fill missing checks for legacy tickets. Build the acceptance mapping below. Publish the six-item conclusion in the Issue; continue within scope/budget, or request one budget exception or a split along explicit contract boundaries under that rule. Reuse existing owner authorization when still applicable. Done: base pinned, mapping planned and scope/budget established, or pending decision/triage/preservation report complete.
3. **Implement.** Use TDD at agreed seams where it fits; preserve user changes and stay within the ticket. Maintain the mapping; apply the shared scope rule if new facts change contracts, acceptance coverage or the approved budget. Done: each requirement has implementation or an explicit unresolved reason; partial work is labelled partial.
4. **Verify and self-check.** Run the applicable [verification rules](../../../docs/agents/verification.md) and mapped scenarios; inspect the final diff against Issue/spec and standards yourself. Ordinary tickets require no parallel self-review agents. Record risk and verification scope using review-pr's [risk rule](../review-pr/SKILL.md); explicit code-review remains available, but its output is implementer self-check. Reconcile every requirement, time/accounting/data semantics when touched, command result and limit. Update STATUS only for actual capability/risk/semantics/verification changes; do not edit HANDOFF. Done: the final diff matches evidence, no acceptance silently omitted, and unresolved gaps remain explicit.
5. **Open the PR.** Commit/push the ticket branch and use [pr](../pr/SKILL.md). Include `Closes #<N>`, standard template sections, acceptance mapping under `Evidence`, `Handoff` (commands/results, failures/environment limits, risk/verification scope, open questions, worktree disposition and one next step), and `Self-check (implementer)` as the only review-related section. Done: one PR points at the pushed branch and distinguishes verified/partial/unverified evidence without claiming independent review.
6. **Hand off and stop.** Post `**Implementer:** Ready for independent review at <head SHA>.` For an authorized loop, retain the exclusive implementation worktree/cache and report its path to the coordinator for fixes. For direct invocation, clean your worktree under the tracker contract; report retained files/reasons when cleanup is unsafe. Stop without starting review. The owner or authorized coordinator starts a fresh reviewer. Done: current ready SHA/comment and worktree disposition are reported.
7. **Respond to review.** When the owner/coordinator resumes you, read PR findings and current head. In a loop return to the same implementation worktree; after direct-invocation cleanup fetch and recreate the branch worktree. Follow Review responses below. After any fix, verify, commit/push, update PR evidence, post the new SHA ready for re-review, retain/clean according to mode, and stop. If no push, identify the unchanged reviewed head and reasoned dispositions. Done: every finding answered; any new head explicitly awaits a fresh independent session. The owner merges only after final-head review and required verification/checks.

## Acceptance mapping

Use `Issue/spec requirement | implementation entry point | relevant scenarios | verification/output evidence | status`. Include required spec behavior even if the checklist abbreviates it; preserve original requirements.

- Choose normal/negative/boundary scenarios from the ticket. Conditional gates may require missing/empty/unknown inputs, ordering or identity isolation.
- Trace the actual entry point to observable output. A helper report alone does not prove required CLI input loading/persistence; cover local workflows with synthetic end-to-end evidence where feasible.
- Separate implementation from verification: cite code/checks actually run, synthetic versus real-data evidence, and verified/partial/unverified reasons. Formatting or a test count alone is not acceptance. Documentation uses diff/link/consistency checks.

## Review responses

Begin each PR-thread reply `**Implementer:**`. For each blocker identify the requirement/root cause, inspect related inputs/branches, verify the counterexample and neighboring failure modes, and add regression tests where appropriate. Report changed files, updated mapping, evidence and limits together. Disclosure or disagreement does not resolve a valid blocker.

For optional suggestions fix or justify deferral; unrelated refactoring is not a completion condition. Keep later changes limited to acceptance or owner requests. Any push, including docs/refactoring, needs independent review of the resulting head. The shared tracker contract controls safe cleanup; never sweep other tasks' worktrees.
