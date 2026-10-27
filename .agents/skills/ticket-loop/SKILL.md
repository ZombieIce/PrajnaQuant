---
name: ticket-loop
description: "Coordinate one eligible ticket through implementation and independent review for owner merge."
disable-model-invocation: true
argument-hint: "<Issue number or request> [implementer-model=<id>] [reviewer-model=<id>]"
---

Coordinate one ticket through implementation, verification, independent review and fixes. The owner merges. Invocation authorizes delegation to an implementer and fresh independent reviewers; the coordinator does not implement, review or reinterpret findings. Read [implement-ticket](../implement-ticket/SKILL.md), [review-pr](../review-pr/SKILL.md), [AGENTS](../../../AGENTS.md), [tracker](../../../docs/agents/issue-tracker.md) and [triage labels](../../../docs/agents/triage-labels.md) before dispatch.

## Start conditions and models

1. Resolve exactly one open ticket from its number/URL or a unique matching GitHub search result. Confirm exactly one triage label `ready-for-agent`, no assignee and every referenced blocker closed as completed. If no unique eligible match, report candidates/states and stop; do not create/retitle Issues, change triage or guess.
2. Resolve each role's model selection: explicit invocation parameter first; otherwise reuse a choice already confirmed in this conversation and still supported; otherwise use the current tool's inheritance/default behavior. Inspect available tool schema/options rather than hardcoding model IDs. Ask only when an explicit choice is unavailable or configuration is genuinely ambiguous. Do not ask every ticket for an already-confirmed preference or silently downgrade a high-risk ticket. Preserve selections for fixes/reviews; if support changes, resolve it explicitly rather than substituting a weaker model.
3. Confirm delegation tools (`collaboration.spawn_agent`, `wait_agent`, `followup_task`) support the isolation below. If unavailable, stop before claiming/launching. Record model arguments as explicit, confirmed reuse, or inheritance/default; when actual resolved model is unreadable, report that honestly.

Done: one current eligible Issue, supported model configuration/default path and delegation/isolation capability are established before implementer claim/launch.

## Implementation handoff

Spawn one implementer, uniquely named, with `fork_turns: "none"`. Pass a `model` override only for a supported selected model; otherwise omit it and use the tool default (do not sacrifice context isolation to force inheritance). Give it the Issue pointer and instruction to follow implement-ticket through ready-for-review handoff in **loop mode**, retaining its exclusive worktree/cache until loop completion/stop. It claims only this ticket, starts from latest main, verifies/self-checks, opens one PR with `Closes #<N>`, posts current ready SHA and stops without review.

Wait for completion/help. Confirm PR URL, Issue link and ready SHA against GitHub. Route branch-caused required-check failures to the same implementer; external causes/owner decisions stop with evidence. Stop for triage, unresolved requirements, size/independent-contract gates or other blockers. Do not start review from self-check alone.

Done: one branch/PR and implementer handoff for current head, with retained worktree path/disposition recorded.

## Independent review

For initial review and every re-review spawn a **new** uniquely named reviewer with `fork_turns: "none"`, selected model override or omitted/default as above. Pass only repository/Issue/spec pointers, PR URL/current head, and for re-review prior review comment links and previous/current head SHAs. Instruct it to follow review-pr in its own isolated worktree, derive original acceptance itself, publish its own comment, never push to implementation branch, then clean its workspace. Do not pass implementation notes/reasoning or coordinator interpretation; never reuse a reviewer agent.

Wait for its PR comment and reviewed SHA. Treat that comment and the reviewer's conclusion as the record; relay links/SHAs/state without adding, removing or reinterpreting findings. Ordinary tickets use one reviewer for both axes; review-pr requires parallel Spec/Standards for high risk and can upgrade discovered risk.

Done: an independent comment records both axes, acceptance coverage, limits and conclusion for the named head.

## Fix and re-review

Send all findings' comment links and reviewed/current SHAs to the **same implementer** via `followup_task`, keeping its model and worktree/cache. It responds to each finding under implement-ticket; any pushed fix (including docs) needs a new reviewer session. No-push responses may retain the reviewed head only when every finding has a reasoned disposition and review still covers current head. The coordinator cannot waive blockers or convert unverified requirements to passes.

Count post-initial fix/re-review cycles, at most three. Each new pushed head gets a fresh reviewer; head changes during review require a fresh pass on current head and count toward the limit. Preserve incremental re-review from review-pr. After the third cycle, stop for owner decision if blockers remain or any finding lacks a response. Also stop for external blockers, unresolved scope/acceptance ambiguity, exceeded size/independent-contract gates or requests expanding the Issue; do not change acceptance to keep looping.

For success confirm from current GitHub that final head has independent review with no unresolved blockers, every finding has an implementer reply, required ticket verification is complete and repository-required checks are green. Wait for pending checks; route branch-caused failures back through fixes/re-review within the limit, and stop for external causes. Do not merge/close PR or Issue.

Done: open PR ready for owner merge at independently reviewed final head with required evidence/checks complete, or concrete stop report with no readiness claim.

## Workspace and final handoff

On success or any stop, coordinate disposition of **this loop's** worktrees under the [tracker lifecycle](../../../docs/agents/issue-tracker.md). The implementer may retain its worktree/cache during review and fixes; reviewers always use separate workspaces. Clean safe owned worktrees at loop end; preserve uncommitted/unpushed work and required ignored files, report path/reason/next action. If the implementer is unavailable, inspect ownership and preservation conditions before any coordinator cleanup. Never sweep other tasks.

Return Issue/PR links, current head, reviewer comment or blocker/check links, required-verification/check status, model selections or inheritance/default (actual model unknown if unreadable), worktree disposition and one next action. At success state that owner merge is pending; at stop say what requires owner action. Reviewer conclusions remain the reviewer's own.
