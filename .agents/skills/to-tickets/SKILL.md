---
name: to-tickets
description: Break a plan or spec into contract-bounded GitHub tickets with blocking edges and near-term scope checks.
disable-model-invocation: true
---

# To Tickets

Break a plan, spec, or conversation into independently verifiable **tickets**, each declaring the tickets that **block** it.

Follow [AGENTS](../../../AGENTS.md), the [GitHub tracker contract](../../../docs/agents/issue-tracker.md) and the shared [ticket scope rule](../../../docs/agents/ticket-scope.md). `.scratch/` is a read-only archive.

## Process

### 1. Gather context

Work from whatever is already in the conversation context. If the user passes a reference (a spec path, an issue number or URL) as an argument, fetch it and read its full body and comments.

### 2. Check current capabilities

For near-term tickets, inspect relevant production entry points, source and tests, reusing evidence already in context. Identify existing capabilities and missing contracts; record the baseline. Use the project's domain glossary and applicable ADRs. Keep distant tickets at behavior/boundary/dependency level until they approach implementation.

Done: near-term gaps have code/test evidence; distant tickets explicitly defer their scope check.

### 3. Draft vertical slices

Break the work at independent behavior/contract boundaries under the shared scope rule.

<vertical-slice-rules>

- A thin slice covers the layers required for its promised behavior; page/API/storage changes may belong to one ticket.
- Shared infrastructure, such as a browser framework, may be independently ticketed with named consumers and a runnable acceptance path.
- Every ticket is independently demoable or verifiable; required tests and contract documentation belong in its estimate.
- Separate prefactoring only when it has an independently verifiable result and genuinely blocks the behavior.

</vertical-slice-rules>

Give each ticket its **blocking edges**: the other tickets that must complete before it can start. A ticket with no blockers can start immediately.

For every near-term ticket, include the six-item template from the shared scope rule. Resolve a triggered size review through explicit owner budget authorization or an approved contract-boundary split before publication as `ready-for-agent`. Owner approval of a breakdown alone is not an explicit budget exception. Done: each near-term ticket has an evidenced scope check and a continuation/authorization/split decision; deferred checks on distant tickets remain visible.

**Wide refactors are the exception to vertical slicing.** A **wide refactor** is one mechanical change (rename a column, retype a shared symbol) whose **blast radius** fans across the whole codebase, so a single edit breaks thousands of call sites at once and no vertical slice can land green. Don't force it into a tracer bullet; sequence it as **expand–contract**. First expand: add the new form beside the old so nothing breaks. Then migrate the call sites over in batches sized by blast radius (per package, per directory), each batch its own ticket blocked by the expand, keeping CI green batch to batch because the old form still exists. Finally contract: delete the old form once no caller remains, in a ticket blocked by every migrate batch. When even the batches can't stay green alone, keep the sequence but let them share an integration branch that all block a final integrate-and-verify ticket; green is promised only there.

### 4. Quiz the user

Present the proposed breakdown as a numbered list. For each ticket, show:

- **Title**: short descriptive name
- **Blocked by**: which other tickets (if any) must complete first
- **What it delivers**: the end-to-end behaviour this ticket makes work
- **Scope check**: the six-item result for near-term tickets; flag requested budget exceptions with scope and total upper bound, or deferred checks for distant tickets

Ask the user:

- Does the granularity feel right? (too coarse / too fine)
- Are the blocking edges correct: does each ticket only depend on tickets that genuinely gate it?
- Should any tickets be merged or split further?

Iterate until the user approves the breakdown.

### 5. Publish the tickets to the configured tracker

Publish the approved tickets to `ZombieIce/PrajnaQuant`, one Issue per ticket in dependency order (blockers first), using actual Issue numbers in `Blocked by`. Apply exactly one triage label: `ready-for-agent` when the scope check is complete and requirements/budget settled; `needs-triage` for deferred checks or pending decisions. Record explicit owner budget authorization in the corresponding Issue. Blocking edges still control when a ready ticket can be claimed.

Work the **frontier**: any ticket whose blockers are all done. For a purely linear chain that means top to bottom.

Do NOT close or modify any parent issue.

<issue-template>

Parent: #<spec> (omit when there is no parent)

## What to build

The end-to-end behaviour this ticket makes work, from the user's perspective, not layer-by-layer implementation.

## Acceptance criteria

- [ ] Criterion 1
- [ ] Criterion 2

## Blocked by

- A reference to each blocking ticket, or "None (can start immediately)".

## Scope check

Use the six-item template in the shared scope rule, with baseline/evidence and any explicit owner budget authorization; for distant tickets, state that the check is deferred until near-term planning.

</issue-template>

Keep acceptance behavioral. Scope evidence may name entry points/source/tests at the recorded baseline as navigation; it does not prescribe the future file layout. If a prototype snippet captures a decision more precisely than prose (state machine, schema or type shape), include only the decision-rich part and identify its origin.
