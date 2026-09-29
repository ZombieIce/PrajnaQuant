---
name: implement-ticket
description: "Implement one GitHub ticket through an independently reviewed pull request."
disable-model-invocation: true
---

Implement one GitHub ticket using the repository's one-ticket/one-branch/one-PR workflow. The upstream `implement` skill is for implementer self-check only.

## Workflow

1. **Claim.** Read the ticket with `gh issue view <N> --comments`, confirm it is unblocked, assign it to yourself, and comment that work has started. Completion criterion: the issue shows your assignee and a start comment.
2. **Prepare.** Fetch the latest `origin/main`; create a dedicated worktree and branch named `<issue>-<slug>` from it. Read the ticket's spec pointer, applicable `AGENTS.md`, and relevant source/tests. Completion criterion: the worktree is clean at the latest main base and the acceptance criteria are understood.
3. **Implement.** Use `/tdd` at agreed seams where it fits. Preserve existing user changes and keep the diff within this ticket. Completion criterion: every acceptance criterion has an implementation or a clearly documented unresolved reason.
4. **Verify.** Run the applicable commands from `AGENTS.md` Testing Rules. Review time, accounting, and data semantics when the change touches them. Completion criterion: report each command and its result, including failures and environmental limits.
5. **Self-check.** Use `/code-review` against the branch base as an implementer self-check, then resolve or record its findings. Update `docs/STATUS.md` and `HANDOFF.md` with the PR link and `Independent review: see PR #<N>`. Completion criterion: the diff and handoff match the verified state; review conclusions remain in PR comments.
6. **Open the PR.** Commit and push the ticket branch, then use `/pr` to open one PR whose description includes `Closes #<N>`, the standard `/pr` sections, and a section titled `Self-check (implementer)` as its only review-related section. Apply explicit owner instructions to the PR body. Completion criterion: the PR points to this ticket branch and records the self-check without claiming independent review.
7. **Hand off for independent review.** Post `**Implementer:** Ready for independent review at <head SHA>.` on the PR, then stop this session. The project owner starts a fresh reviewer session using `review-pr`; the implementer does not launch or assign a reviewer. Completion criterion: the owner has the ready comment and the implementation session has ended.
8. **Respond to review.** After a reviewer comments, address every finding or explain why it does not require a change. Start each PR-thread reply with `**Implementer:**`; after fixes, commit and push, post the new head SHA as ready for re-review, and stop this session. The project owner starts the fresh reviewer session. Completion criterion: every finding has an implementation response and the latest review follows the latest push. The project owner merges.
