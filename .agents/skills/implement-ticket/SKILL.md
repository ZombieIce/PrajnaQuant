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
7. **Hand off for independent review.** Post `**Implementer:** Ready for independent review at <head SHA>.` on the PR, then clean up the worktree (see Worktree cleanup) and stop this session. The project owner starts a fresh reviewer session using `review-pr`; the implementer does not launch or assign a reviewer. Completion criterion: the owner has the ready comment, the worktree is removed, and the implementation session has ended.
8. **Respond to review.** After a reviewer comments, recreate the worktree from the remote branch (`git fetch origin <branch> && git worktree add <path> <branch>`). Address every finding or explain why it does not require a change. Start each PR-thread reply with `**Implementer:**`; after fixes, commit and push, post the new head SHA as ready for re-review, clean up the worktree, and stop this session. The project owner starts the fresh reviewer session. Completion criterion: every finding has an implementation response, the latest review follows the latest push, and the worktree is removed. The project owner merges.

## Worktree cleanup

Each worktree carries its own `target/` (multiple GB), so never leave one behind after handoff.

1. From the main checkout, confirm the branch is fully pushed: `git -C <worktree> status --short` is empty and `git -C <worktree> rev-parse HEAD` equals `git rev-parse origin/<branch>`. If not, commit and push first; never delete unpushed work.
2. Remove it: `git worktree remove --force <worktree>` (`--force` is needed only to discard ignored build output such as `target/`, and only after step 1 passes), then `git worktree prune`.
3. Keep the local and remote branch; the project owner deletes them after merge. Report `git worktree list` in the handoff.
4. Also clear leftover worktrees of merged or closed tickets: list with `git worktree list`, and remove only those whose PR is merged/closed and whose step 1 check passes. Ask the owner before removing anything else.
