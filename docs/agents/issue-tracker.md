# Issue tracker: GitHub Issues

Issues, specs and PRs for this repo live on GitHub at `ZombieIce/PrajnaQuant`. Read current GitHub state through an available connector; use `gh` when capabilities require it (pass `-R ZombieIce/PrajnaQuant` outside the repo root). Report access failures; never substitute an old cache for current Issue eligibility/blockers, PR head or checks. For a pinned head/spec, read relevant sections once and refresh changed inputs rather than repeating full outputs.

`.scratch/` is a **read-only archive** of the local-Markdown tracker used until 2026-09-28. Do not add or edit tickets there. A file with a `**Migrated:**` line is tracked by the linked GitHub issue.

## Conventions

- **Spec:** one issue labelled `spec`, titled with the feature name. Its body uses the spec template and includes `Notes`, `Decisions-so-far`, and `Fog` sections when created; any of these sections may be empty.
- **Ticket:** one issue per ticket, never a combined checklist issue. The body starts with `Parent: #<spec>` when there is a spec, then `What to build`, acceptance criteria as task-list checkboxes, and `Blocked by: #N, #N` (or `None`).
- **Triage state:** exactly one triage label from `triage-labels.md`. A ticket that has been claimed has an assignee.
- **Milestone:** the roadmap stage the work belongs to (`POC-0`, `MVP-0` …; current product stage: `MVP-7`). Repo infrastructure that isn't tied to a stage has no milestone.
- **Conclusion:** verdicts (`adopt / defer / reject / unresolved`), acceptance notes and caveats go on a `**Conclusion:**` line in the issue body or in the closing comment, never in the title or labels.
- **Comments:** discussion, review results and hand-off notes are issue or PR comments.

## Branches and PRs

- One ticket → one branch `<issue>-<slug>` from the latest `main` → one PR whose description contains `Closes #<issue>`.
- Agents may commit and push to their own branch. They must not push to `main`, force-push a shared branch, or merge.
- The owner may directly start implement-ticket/review-pr or authorize ticket-loop to delegate those workflows. After the PR and implementer ready-SHA comment exist, the owner or authorized coordinator starts a fresh independent reviewer session, including every re-review. That session has not implemented the branch and does not inherit implementation context. The implementer stops at handoff, never launches/acts as reviewer. Self-check is labelled `Self-check (implementer)` and is not independent review. Review-pr defines ordinary versus high-risk review depth. Before merge, independent review must cover the latest pushed head (including any later documentation push), every finding must have an implementer response, and all required verification/checks must be complete; missing required evidence cannot be called merge-ready.
- All agents use one GitHub account; identify the speaker in each review-thread comment with the exact prefix `**Implementer:**` or `**Independent reviewer:**`.
- Before merging, the project owner confirms that an independent-review comment was posted after the latest push to the PR branch.
- PR comments are the source of truth for review findings and conclusions. Do not copy reviewer conclusions into branch documents. Ticket hand-off (verified commands, limits, open questions, next step) lives in the PR description; `HANDOFF.md` is an entry point that PRs do not edit. Update `STATUS.md` only when capability, risk, quantitative semantics, or verification status changes, as a single entry under its topic, and never append per-ticket log entries.
- The project owner merges. Merging normally closes the issue, but GitHub's auto-close is not guaranteed; see **Post-merge check** under Wayfinding operations.
- One working tree carries one branch. Run parallel tasks in separate `git worktree` checkouts; lifecycle and preservation are below.

## Worktree lifecycle (shared contract)

- **Ownership/isolation:** create task-owned worktrees from the required base. Record path/owner/mode at handoff. A loop retains its implementer's exclusive worktree and build cache through review/fixes; fixes return there. Reviewers use their own worktrees at pinned PR heads, with no access to implementation context through borrowed workspaces.
- **Direct implementation:** after posting ready SHA, clean the task-owned worktree when safe; later fixes fetch/recreate the branch worktree. **Loop:** defer implementation cleanup until success or stop. **Reviewer:** clean its own workspace after publishing each pass. On every completion or abnormal stop report removed paths or retained paths, reasons and next action; if cleanup cannot be completed, report it without deleting work.
- **Preservation gate:** inspect `git status --short` and untracked/ignored files, including content hidden by ignore rules. A clean status alone does not authorize deletion of user data. Preserve uncommitted/unpushed work and any ignored files needed by the user/task; record retained location or a verified safe copy before removal. If ownership or disposability is unknown, leave the worktree and ask the owner about that data. Never sweep worktrees belonging to other tasks, even when their PRs are closed/merged.
- **Removal:** for implementation, confirm no uncommitted/untracked work and local HEAD equals the current pushed remote branch head; refresh the remote before this check. For review, confirm it remains at the pinned head with no local work to preserve. Remove only the verified owned path from another checkout with `git worktree remove <path>`. Use `--force` only after the preservation gate passes and the remaining ignored content is known disposable build output created by this task (such as `target/`), never to discard unknown data. Keep ticket branches for owner deletion after merge; report `git worktree list` in handoff. Do not run repository-wide cleanup as part of a ticket.

## When a skill says "publish to the issue tracker"

Create the issue with `gh issue create --title ... --body-file ... --label <triage> [--milestone <stage>]`. Publish blockers first, so each `Blocked by` line can use real issue numbers.

## When a skill says "fetch the relevant ticket"

Fetch the current Issue and comments using the connector, or `gh issue view <N> --comments`. The user normally passes the number/URL. Follow referenced blockers/spec as needed and verify current state before claiming or dispatching.

## Wayfinding operations

- **Map:** an issue labelled `spec` (or the effort's parent issue). Its body holds Notes / Decisions-so-far / Fog.
- **Child ticket:** an issue whose body has `Parent: #<map>`.
- **Blocking:** a `Blocked by: #N, #N` line. A ticket is unblocked when every listed issue is closed as completed.
- **Frontier:** `gh issue list --state open --label ready-for-agent`. Keep issues with no assignee whose blockers are all closed; the lowest number wins.
- **Claim:** `gh issue edit <N> --add-assignee @me` and a comment saying the work has started, before any work.
- **Resolve:** merge the PR that says `Closes #N` (owner), or for non-code tickets add the answer as a comment and `gh issue close <N>`. Remove the issue's triage label when it is closed (`gh issue edit <N> --remove-label <label>`; auto-close does not do this); its closed state and `Conclusion` express the outcome, not its labels. When the ticket belongs to a map issue, after the PR is merged or the issue is closed, the implementation agent appends a gist and a link to the map issue's `Decisions-so-far`; if the implementation agent is unavailable, the review agent does so. No `Decisions-so-far` update is needed when there is no map issue.
- **Post-merge check:** after merging, the owner (or an agent the owner names) runs `gh issue view <N> --json state,labels`. If the issue is still open, run `gh issue close <N> --reason completed` and say in a comment that auto-close did not fire. If `labels` still lists a triage label once the issue is closed, remove it with `gh issue edit <N> --remove-label <label>`. A ticket that stays open blocks its dependents in the Frontier and Blocking checks.
