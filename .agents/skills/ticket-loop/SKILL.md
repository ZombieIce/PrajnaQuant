---
name: ticket-loop
description: "Coordinate one eligible GitHub ticket from implementation through independent review to a merge-ready PR."
disable-model-invocation: true
argument-hint: "<Issue number or request> [implementer-model=<id>] [reviewer-model=<id>]"
---

# Ticket loop

Run one repository ticket through implementation, verification, independent review, any required fixes and re-review, then stop with a merge-ready PR for the owner. The owner merges. This skill coordinates agents; it does not implement or review the change itself.

Use `.agents/skills/implement-ticket/SKILL.md` as the implementation workflow and `.agents/skills/review-pr/SKILL.md` as the independent review workflow. Read both, plus `AGENTS.md`, `docs/agents/issue-tracker.md`, and `docs/agents/triage-labels.md`, before dispatching work. Those files define ticket claiming, scope limits, validation, comments, review evidence and worktree cleanup.

## Start conditions

1. Resolve exactly one ticket. An Issue number or URL identifies the ticket directly. For natural-language input, search the repository's GitHub Issues and require exactly one matching open ticket with exactly one triage label, `ready-for-agent`, and no unresolved blocker. If there is no unique eligible match, show the candidate Issue numbers, titles and relevant states, then stop. Do not create or retitle an Issue, change its triage state, or select a ticket by guesswork.
2. Resolve both agent models before any Issue claim or agent launch. Parse optional `implementer-model=<id>` and `reviewer-model=<id>` values from the invocation. Read the current model choices in the `collaboration.spawn_agent` tool schema and validate each supplied ID against that list. Ask the user separately for any missing or unsupported choice before continuing. The two choices may be the same or different. Reuse the selected reviewer model for each review pass.
3. Confirm the exact Issue is open, has exactly one triage label and that label is `ready-for-agent`, and is unblocked. For dependency tickets, inspect the referenced blockers rather than inferring their state from the ticket text. If a precondition fails, report the Issue state and stop.

The loop requires `collaboration.spawn_agent`, `collaboration.wait_agent`, and `collaboration.followup_task`. If any are unavailable, report that the loop cannot preserve its delegation and isolation requirements; stop before claiming the Issue.

Completion criterion: one eligible Issue and two valid model choices are recorded; no ticket has been claimed and no agent has been launched before model selection is complete.

## Implementation handoff

Start one implementer with `collaboration.spawn_agent`, using a unique task name such as `ticket_<N>_implementer`, `fork_turns: "none"`, and `model` set to the selected implementer model. Give it only the Issue URL/number and the instruction to read and follow `implement-ticket` through its PR-ready handoff. It must use a dedicated worktree from the latest `main`, claim and implement only this ticket, run the applicable verification, complete its self-check, push its ticket branch, create one PR containing `Closes #<N>`, and post its ready-for-review head SHA. It must stop there without starting review.

Wait for the implementer to finish or ask for help. Continue only when the PR URL, Issue link and ready-for-review SHA are available in GitHub. If a required PR check fails, send the failure link to the implementer to diagnose and fix when it is caused by the branch; stop with evidence when the cause is external or needs owner judgment. If the implementer stops for triage, an unresolved requirement, the size/interface gate, or another blocker, report the evidence and stop without starting review.

Completion criterion: the Issue has one implementation branch and PR, and the PR has an implementer handoff comment for its current head.

## Independent review

For the initial review and every re-review, start a new reviewer with `collaboration.spawn_agent`, using unique task names such as `ticket_<N>_review_initial` or `ticket_<N>_rereview_1`, `fork_turns: "none"`, and the selected reviewer model. Give it only these source pointers and state:

- repository and Issue URL/number;
- the Issue's spec pointer;
- PR URL and current head SHA;
- for re-review, links to prior reviewer comments and the new head SHA.

Tell it to read and follow `review-pr` in its own worktree, derive acceptance coverage from GitHub and the repository, publish its own PR comment, and never push to the implementation branch. Do not give it implementation-agent notes, summaries, reasoning, or the coordinator's interpretation. It must determine every finding from the Issue, spec, repository and PR. Never reuse a reviewer agent for a later pass.

Wait for the reviewer to publish its comment. Treat the GitHub comment and its reviewed SHA as the review record. Use the reviewer's stated conclusion to choose the next handoff. The coordinator relays comment links, head SHAs and workflow state; it does not add, remove, reinterpret or publish reviewer findings.

Completion criterion: an independent reviewer comment covers the PR head under review, and the reviewer's own conclusion and findings are recorded on the PR.

## Fix and re-review loop

If the review comment identifies findings, send the implementer the PR/comment links and the relevant reviewed and current head SHAs using `collaboration.followup_task`. Keep the same implementer agent and model for fixes. Ask it to follow the review-response steps in `implement-ticket`, respond to every finding, and push any required code fix. A new pushed head always requires a fresh reviewer session. If no code change is needed, accept the implementer's response only when every finding has a reasoned disposition and the existing review still covers the latest head; an unresolved blocking finding cannot be waived by the coordinator.

Count post-initial fix-and-review cycles. Allow at most three. Each cycle that pushes code gets one new reviewer with the selected reviewer model and a fresh context/worktree. If the latest head changed during review, have a new reviewer inspect the current head and count that pass toward the limit. After the third cycle, if the latest reviewer reports an unresolved finding or any finding lacks an implementer response, stop and give the user the PR comment links and head SHAs for a decision. Also stop for external blockers, unresolved scope/acceptance ambiguity, a ticket that exceeds the repository's size or one-interface gate, or a requested change that would expand the Issue.

Never mark the PR ready based on the implementer's self-check. For success, verify from GitHub that the latest pushed head has an independent review with no unresolved blockers, every review finding has an implementer reply, the ticket's required verification is complete, and any repository-required PR checks are green. If a check is pending, wait for its result; route branch-caused failures to the implementer and send every pushed fix through a fresh review. Stop when the cause is external or needs owner judgment. Do not merge, close the PR, or close the Issue.

Completion criterion: the PR is open and ready for the owner to merge, with the latest head independently reviewed, every finding answered, and all applicable verification and required checks complete; otherwise the workflow stops with a concrete blocker report.

## Return to the user

At a blocker, return the exact Issue/PR links, current head SHA, relevant comments or failed checks, what is waiting on the owner, and the next action needed. At completion, return the Issue and PR links, current head SHA, independent reviewer comment link, the selected implementer/reviewer models, verification and required-check status, and state that the PR is waiting for the owner to merge. The coordinator reports status and links only; reviewer conclusions remain the reviewer's own.
