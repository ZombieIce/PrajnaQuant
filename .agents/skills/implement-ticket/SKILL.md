---
name: implement-ticket
description: "Implement one GitHub ticket through an independently reviewed pull request."
disable-model-invocation: true
---

Implement one GitHub ticket using the repository's one-ticket/one-branch/one-PR workflow. The upstream `implement` skill is for implementer self-check only.

## Workflow

1. **Claim.** Read the ticket with `gh issue view <N> --comments`, confirm it is unblocked and labelled `ready-for-agent` (a `needs-triage` ticket waits for the owner), assign it to yourself, and comment that work has started. Completion criterion: the issue shows your assignee and a start comment.
2. **Prepare.** Fetch the latest `origin/main`; create a dedicated worktree and branch named `<issue>-<slug>` from it. Read the ticket's spec pointer, applicable `AGENTS.md`, and relevant source/tests. Build the acceptance mapping below before implementing, then estimate the diff against the size gate in `AGENTS.md` Development Rules (about 800 changed lines including tests, one interface per ticket). Over the gate, post a split proposal on the issue and stop this session until the owner confirms. Completion criterion: the worktree is clean at the latest main base, every acceptance criterion has a planned implementation and verification path or an explicit unresolved question, and the estimate is within the gate or a split proposal is posted.
3. **Implement.** Use `/tdd` at agreed seams where it fits. Preserve existing user changes and keep the diff within this ticket. Maintain the acceptance mapping as the implementation evolves. Completion criterion: every acceptance criterion has an implementation or a clearly documented unresolved reason; partial work is labelled partial.
4. **Verify.** Run the applicable commands from `AGENTS.md` Testing Rules and the checks identified by the acceptance mapping. Review time, accounting, and data semantics when the change touches them. Completion criterion: each mapped scenario has evidence or an explicit verification limit; report each command and its result, including failures and environmental limits.
5. **Self-check.** Use `/code-review` against the branch base as an implementer self-check, then resolve or record its findings. Reconcile the complete acceptance mapping with the final diff and verified outputs. Update `docs/STATUS.md` only if capability, risk, quantitative semantics, or verification status actually changed; do not edit `HANDOFF.md`. Completion criterion: the diff matches the verified state, no criterion is silently omitted, and review conclusions remain in PR comments.
6. **Open the PR.** Commit and push the ticket branch, then use `/pr` to open one PR whose description includes `Closes #<N>`, the standard `/pr` sections, the acceptance mapping under `Evidence`, a `Handoff` section (verified commands and results, failures and environmental limits, open questions, and the single recommended next step), and a section titled `Self-check (implementer)` as its only review-related section. Apply explicit owner instructions to the PR body. Completion criterion: the PR points to this ticket branch, distinguishes verified/partial/unverified acceptance evidence, and records the self-check without claiming independent review.
7. **Hand off for independent review.** Post `**Implementer:** Ready for independent review at <head SHA>.` on the PR, then clean up the worktree (see Worktree cleanup) and stop this session. The project owner starts a fresh reviewer session using `review-pr`; the implementer does not launch or assign a reviewer. Completion criterion: the owner has the ready comment, the worktree is removed, and the implementation session has ended.
8. **Respond to review.** After a reviewer comments, recreate the worktree from the remote branch (`git fetch origin <branch> && git worktree add <path> <branch>`). Address every finding using Review responses below. Start each PR-thread reply with `**Implementer:**`; after fixes, commit and push, update the PR evidence, post the new head SHA as ready for re-review, clean up the worktree, and stop this session. The project owner starts the fresh reviewer session. If no push is needed, retain the reviewed head and record the responses. Completion criterion: every finding has a response and supporting evidence or a documented deferral, and the worktree is removed; if pushed, the new head explicitly awaits independent re-review, otherwise responses identify the unchanged reviewed head. The project owner merges only after independent review covers the latest pushed head.

## Acceptance mapping

Use a compact table: `Issue/spec requirement | implementation entry point | relevant scenarios | verification/output evidence | status`. Include behavior required by the spec even when the acceptance checklist abbreviates it; preserve the original requirements.

- Plan normal, negative and boundary cases where they affect acceptance. For conditional gates, include missing/empty/unknown inputs and ordering or identity isolation when relevant; choose cases from the ticket rather than copying a universal checklist.
- Follow required behavior through its actual entry point to its observable output. A helper returning a report does not prove that the requested CLI loads inputs and persists that report. When the ticket requires a local workflow, cover it with a synthetic end-to-end case where feasible.
- Separate implementation from verification: cite code and the tests/checks actually run, identify synthetic versus real-data evidence, and mark each criterion verified, partial or unverified with reasons. Passing formatter/lint or a test count alone does not establish acceptance. For documentation-only work, use diff and consistency checks rather than application tests.

## Review responses

For each blocking finding, identify the violated requirement and root cause; inspect related branches and inputs in the affected behavior. Verify the reported counterexample and relevant neighboring failure modes, using regression tests for code changes where appropriate. Update the acceptance mapping and report the changed files, evidence and remaining limits together. A disagreement or unresolved blocker remains explicit; disclosure alone does not satisfy the requirement.

Non-blocking suggestions are optional. Respond with a justified fix or deferral; defer unrelated refactoring rather than making it a condition of completion. After a head has no blockers, keep further changes limited to acceptance needs or explicit owner requests. Any later push, including a refactor or documentation change, requires independent review of the resulting head before merge.

## Worktree cleanup

Each worktree carries its own `target/` (multiple GB), so never leave one behind after handoff.

1. From the main checkout, confirm the branch is fully pushed: `git -C <worktree> status --short` is empty and `git -C <worktree> rev-parse HEAD` equals `git rev-parse origin/<branch>`. If not, commit and push first; never delete unpushed work.
2. Remove it: `git worktree remove --force <worktree>` (`--force` is needed only to discard ignored build output such as `target/`, and only after step 1 passes), then `git worktree prune`.
3. Keep the local and remote branch; the project owner deletes them after merge. Report `git worktree list` in the handoff.
4. Also clear leftover worktrees of merged or closed tickets: list with `git worktree list`, and remove only those whose PR is merged/closed and whose step 1 check passes. Ask the owner before removing anything else.
