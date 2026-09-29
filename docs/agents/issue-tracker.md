# Issue tracker: GitHub Issues

Issues, specs and PRs for this repo live on GitHub at `ZombieIce/PrajnaQuant`. Use the `gh` CLI; always pass `-R ZombieIce/PrajnaQuant` when outside the repo root.

`.scratch/` is a **read-only archive** of the local-Markdown tracker used until 2026-09-28. Do not add or edit tickets there. A file with a `**Migrated:**` line is tracked by the linked GitHub issue.

## Conventions

- **Spec:** one issue labelled `spec`, titled with the feature name. Its body uses the spec template and includes `Notes`, `Decisions-so-far`, and `Fog` sections when created; any of these sections may be empty.
- **Ticket:** one issue per ticket, never a combined checklist issue. The body starts with `Parent: #<spec>` when there is a spec, then `What to build`, acceptance criteria as task-list checkboxes, and `Blocked by: #N, #N` (or `None`).
- **Triage state:** exactly one triage label from `triage-labels.md`. A ticket that has been claimed has an assignee.
- **Milestone:** the roadmap stage the work belongs to (`POC-0`, `MVP-0` … `MVP-6`). Repo infrastructure that isn't tied to a stage has no milestone.
- **Conclusion:** verdicts (`adopt / defer / reject / unresolved`), acceptance notes and caveats go on a `**Conclusion:**` line in the issue body or in the closing comment, never in the title or labels.
- **Comments:** discussion, review results and hand-off notes are issue or PR comments.

## Branches and PRs

- One ticket → one branch `<issue>-<slug>` from the latest `main` → one PR whose description contains `Closes #<issue>`.
- Agents may commit and push to their own branch. They must not push to `main`, force-push a shared branch, or merge.
- Independent review happens after the PR is created. An agent that did not implement the change gives the review as a PR comment; implementation self-review before PR creation does not count as independent review and must not be described as one in the PR description. Before the PR is merged, it must have at least one such review comment and an implementation-agent reply addressing each finding.
- The project owner merges. Merging closes the issue.
- One working tree carries one branch. Run parallel tasks in separate `git worktree` checkouts.

## When a skill says "publish to the issue tracker"

Create the issue with `gh issue create --title ... --body-file ... --label <triage> [--milestone <stage>]`. Publish blockers first, so each `Blocked by` line can use real issue numbers.

## When a skill says "fetch the relevant ticket"

`gh issue view <N> --comments`. The user normally passes the issue number or URL.

## Wayfinding operations

- **Map:** an issue labelled `spec` (or the effort's parent issue). Its body holds Notes / Decisions-so-far / Fog.
- **Child ticket:** an issue whose body has `Parent: #<map>`.
- **Blocking:** a `Blocked by: #N, #N` line. A ticket is unblocked when every listed issue is closed as completed.
- **Frontier:** `gh issue list --state open --label ready-for-agent`. Keep issues with no assignee whose blockers are all closed; the lowest number wins.
- **Claim:** `gh issue edit <N> --add-assignee @me` and a comment saying the work has started, before any work.
- **Resolve:** merge the PR that says `Closes #N` (owner), or for non-code tickets add the answer as a comment and `gh issue close <N>`. Remove the issue's triage label when it is closed; its closed state and `Conclusion` express the outcome, not its labels. After the PR is merged or the issue is closed, the implementation agent appends a gist and a link to the map issue's `Decisions-so-far`; if the implementation agent is unavailable, the review agent does so.
