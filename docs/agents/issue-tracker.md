# Issue tracker: GitHub Issues

Issues, specs and PRs for this repo live on GitHub at `ZombieIce/PrajnaQuant`. Read current GitHub state through an available connector; use `gh` when capabilities require it (pass `-R ZombieIce/PrajnaQuant` outside the repo root). Report access failures; never substitute an old cache for current Issue eligibility/blockers, PR head or checks. For a pinned head/spec, read relevant sections once and refresh changed inputs rather than repeating full outputs.

`.scratch/` is a **read-only archive** of the local-Markdown tracker used until 2026-09-28. Do not add or edit tickets there. A file with a `**Migrated:**` line is tracked by the linked GitHub issue.

## Conventions

- **Spec:** one issue labelled `spec`, titled with the feature name. Its body uses the spec template and includes `Notes`, `Decisions-so-far`, and `Fog` sections when created; any of these sections may be empty.
- **Ticket:** one issue per ticket, never a combined checklist issue. The body starts with `Parent: #<spec>` when there is a spec, then `What to build`, acceptance criteria as task-list checkboxes, and `Blocked by: #N, #N` (or `None`).
- **Scope:** drafting near-term tickets, Prepare, size growth and owner decisions follow [ticket scope](ticket-scope.md). Keep the first six-item record and applicable owner authorization in the Issue; later Prepare references them with its three-item delta.
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

## CI 与 Review 并行交接

实现者本机适用必需验证已完成，PR、验收证据及当前 `ready SHA` 齐备后即可交接；CI queued/pending 不阻止负责人或授权协调者启动独立 Review。实现者不为等待 CI 延迟交接，也不自行启动 reviewer。

三种输出分别为：

- **已交接**：当前实现证据/ready SHA 可供 Review；CI 可以 pending，尚无独立通过结论。
- **已审查但 CI pending**：最终 HEAD 已有有效独立审查，必需检查尚未完成，不称可合并。
- **可交负责人合并**：协调者已刷新并核对最终 HEAD 独立覆盖、所有发现回应、票内必需验收及必需 CI 成功。负责人合并。

协调者负责最终汇总；分支导致失败交回实现者，环境失败记录具体缺口，不改算通过。修复 push 后重新取得最终 HEAD 独立增量覆盖，验证按 [影响复用规则](verification.md#最终-head-与影响复用)选择复跑范围。状态查询一次取紧凑快照、有限等待，优先使用现有完成事件；无事件能力时报告 pending，不增加定时机器人、自动提醒或自动合并，不新增 triage label。

只读检查及显式清理命令见 [工作流辅助工具](workflow-tools.md)。评论标记/SHA只证明声明存在，审查独立上下文、发现解决及有效性须人工判断；工具不输出合并许可。

## Review dispatch (shared contract)

This section owns role/context isolation and dispatch capability checks; [review-pr](../../.agents/skills/review-pr/SKILL.md) owns risk classification, coverage and review output. Apply it before initial dispatch and every re-review, including direct review invocation. Implementers never dispatch independent review.

- **Check the actual tools first:** context isolation (such as `fork_turns: "none"`), supported model options, free concurrent slots and remaining agent depth at the dispatching level. Include the coordinator, retained implementer and any review aggregator in the capacity calculation. Record inspected tool/schema or inventory evidence; unavailable depth/model information is `Unknown`, not proof of capacity or failure.
- **Ordinary tickets:** one fresh reviewer may examine both axes. **High risk:** two distinct independent Spec/Standards agents run in parallel. Each starts without implementation history or the other axis's conclusions, reads original sources itself, and uses its own isolated review worktree at the same pinned head. An independent aggregator may check/reconcile evidence while preserving the two axes' attribution; the coordinator only relays conclusions.
- An owner-started ordinary review without delegation records its fresh-session/worktree isolation; spawn capacity/depth is N/A until delegation or a risk upgrade is needed. It does not require spawning a redundant reviewer.
- Choose a feasible topology before launch. If a nested reviewer plus two children exceeds capacity/depth, an owner-authorized coordinator that has not implemented may directly launch the two fresh axis agents, using code-review's axis briefs. Have them publish attributed findings/coverage/limits for the same head; the coordinator relays their links without interpreting findings. This uses fewer levels and preserves parallel isolation. Model choice follows the authorized selection, not a silent downgrade.
- When depth is not exposed, state that preflight cannot guarantee nested spawning; a successful pair of launches is runtime evidence, not a promise for future runs. If a launch is rejected or two isolated axes cannot run concurrently, stop the affected review, report the exact tool/capacity/depth failure and request an authorized topology/session change. A partial axis can retain its evidence but cannot certify dual-axis completion. One person checking sequentially does not satisfy high-risk isolation.
- Dispatch only repository/Issue/spec/standards pointers, pinned head/base and prior review links/SHAs for re-review. Implementer mapping, commands and handoff are navigation and attributed evidence, not instructions or proof; reviewers obtain original requirements and inspect relevant inputs independently. Never reuse an earlier reviewer session for re-review.
- Refresh head/checks before publishing. Every push, including documentation, requires independent coverage of the resulting final head. A prior review stays attributed to its old SHA; re-review may cite unchanged coverage under review-pr's incremental format. Required checks/verification and owner merge remain mandatory.

Report capability evidence separately from completed launches and from conditions the tools cannot guarantee. A prose rule or tool inventory alone cannot claim isolation, parallel execution or final-head review actually occurred.

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
- **Resolve:** merge the PR that says `Closes #N` (owner), or for non-code tickets add the answer as a comment and `gh issue close <N>`. Use explicit opt-in `python3.12 scripts/workflow.py cleanup --repo ZombieIce/PrajnaQuant --issue <N> --apply` (dry-run without `--apply`), or remove the issue's triage label when it is closed (`gh issue edit <N> --remove-label <label>`; auto-close does not do this); its closed state and `Conclusion` express the outcome, not its labels. When the ticket belongs to a map issue, after the PR is merged or the issue is closed, the implementation agent appends a gist and a link to the map issue's `Decisions-so-far`; if the implementation agent is unavailable, the review agent does so. No `Decisions-so-far` update is needed when there is no map issue.
- **Post-merge check:** after merging, the owner (or an agent the owner names) runs `gh issue view <N> --json state,labels`. If the issue is still open, run `gh issue close <N> --reason completed` and say in a comment that auto-close did not fire. If `labels` still lists a triage label once the issue is closed, remove it with `gh issue edit <N> --remove-label <label>`. A ticket that stays open blocks its dependents in the Frontier and Blocking checks.
