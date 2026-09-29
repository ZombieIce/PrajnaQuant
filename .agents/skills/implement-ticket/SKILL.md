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
5. **Self-check.** Use `/code-review` against the branch base as an implementer self-check, then resolve or record its findings. Update `docs/STATUS.md` and `HANDOFF.md`. Completion criterion: the diff and handoff match the verified state, and the PR's only section is titled `Self-check (implementer)`.
6. **Open the PR.** Commit and push the ticket branch, then use `/pr` to open one PR whose description includes `Closes #<N>` and the self-check section. Completion criterion: the PR points to this ticket branch and records the self-check without claiming independent review.
7. **Request independent review.** Ask a different agent session to use `review-pr` with no implementation-session context. Then stop implementation work and wait for the PR review comment. Completion criterion: the PR contains an independent review comment posted after the latest push.
8. **Respond to review.** Address every finding or explain why it does not require a change. Start each PR-thread reply with `**Implementer:**`; after fixes, commit and push, then ask the reviewer to re-review. Completion criterion: every finding has an implementation response and any requested re-review is recorded. The project owner merges.
