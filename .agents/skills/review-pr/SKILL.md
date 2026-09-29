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

1. **Establish scope.** Read the PR, originating issue and spec, applicable `AGENTS.md`, and relevant repository standards. Record the latest PR head SHA. Completion criterion: acceptance criteria and changed-file scope are explicit.
2. **Inspect independently.** Create a separate worktree, check out the PR branch, and use the merge-base with `origin/main` as the fixed point. Run `/code-review` so Spec and Standards are reviewed as separate axes. Completion criterion: both axes have findings or an explicit zero-finding result.
3. **Verify findings.** Recheck each finding against the actual diff, code, tests, and documents; remove false positives and note relevant test results. Run applicable commands from `AGENTS.md` Testing Rules. Completion criterion: every published finding is supported by repository evidence and classified as blocking or non-blocking.
4. **Publish review.** Add one PR comment beginning `**Independent reviewer:**`, with separate `Spec` and `Standards` sections, blocking/non-blocking labels for each finding, and an overall review conclusion. Do not push changes to the PR branch. Completion criterion: the review comment identifies the reviewed head SHA and records both axes and the conclusion.
5. **Re-review fixes.** After the project owner starts a fresh re-review session for a new pushed head, inspect the new diff and verify each addressed finding. Add a follow-up comment beginning `**Independent reviewer:**`. Completion criterion: each prior finding is marked resolved or remains open with evidence. The project owner merges.
