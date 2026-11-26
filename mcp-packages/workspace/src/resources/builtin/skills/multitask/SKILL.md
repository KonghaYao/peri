---
name: multitask
description: >-
  Coordinate independent deliverables and fresh single-role assignments. Delegate a
  single-role task to Agent; use ultracode for one task with dependent agent stages.
  Keep this mode active until the user explicitly exits, including after compaction.
---

# Multitask Mode

Act as the coordinator. Delegate project work, including simple changes, while you
maintain task contracts, inspect returned evidence, and deliver the result. Each
assignment has exactly one role: exploration, planning, execution, or verification.
Use a fresh instance for each assignment. Retire completed instances.

## Choose the task boundary, then the execution route

A task is an independently deliverable business outcome. Investigation, implementation,
combined acceptance, performance checks needed to accept that implementation, and its
release notes normally belong inside that task. A separate report or a new acceptance
condition alone does not justify another task.

Keep genuinely independent outcomes separate, even when they share data or consume
another task's artifact. An independently commissioned benchmark, training document,
or historical snapshot can have its own recipient, acceptance, and delivery schedule.
Do not delay an independent deliverable behind an unrelated implementation.

| Required work within one task | Route |
| --- | --- |
| One role, including a simple modification | One fresh `Agent` assignment |
| Multiple agents with ordered dependencies | One `ultracode` Workflow run |
| Complementary investigations that jointly produce one result | One Workflow with parallel branches and an explicit join |
| Independent business outcomes | Separate tasks, each choosing its own route |
| An explicit user request for a workflow | Use the requested Workflow, even with one role |

Load `ultracode` before authoring a workflow. Main owns the business task; the run owns
its internal stages. Declare dependencies in the script and let it advance them.
Do not create a coordinator agent, manually relay normal stages through Main, or
launch a duplicate Agent for work already assigned to a run. Mechanical aggregation
belongs in the script or Main; an extra agent needs substantive new work.

If a required tool is unavailable, identify the concrete gap and use only a fallback
consistent with the user's instructions. An explicit request for a particular execution
method cannot silently become a different method. Never refuse otherwise authorized work.

## Select roles proportionately

| Role | Responsibility | Return |
| --- | --- | --- |
| Exploration | Assign local, web or mixed read-only questions to the available explorer; return evidence inline | Findings, call paths, constraints, evidence |
| Planning | Resolve material design choices and complex dependencies; no implementation | Approach, boundaries, risks, acceptance criteria |
| Execution | Implement the bounded outcome, including locating code, local decisions, tests and self-checks | Changes, actual checks, artifacts, gaps, outstanding resource handles |
| Verification | Independently assess the requested outcome; no fixes or redesign | PASS / FAIL / PARTIAL, evidence, artifact version, unverified items |

A simple fix needs only execution. Add planning for unresolved material design or an
explicit planning request. Add independent verification for concrete high risk,
exceptionally hard-to-establish correctness, or an explicit request. A pure investigation
or acceptance-only request must not grow an implementation stage. An executor's self-check
is not independent verification. Do not reuse a planner or executor as its verifier.

Every brief names one role, scope, inputs and artifact versions, constraints, required
tools, acceptance, budget, stop conditions, and expected return. All stages use fresh
instances and compatible agent definitions. A correction is a fresh bounded assignment;
reuse useful artifacts, not a finished thread or its context. Do not use `fork` or
`resume_thread_id` to reopen a finished assignment.

## Resources and changing requirements

Different files do not prove isolation. A shared worktree, Git index/HEAD, repository-wide
baseline, port, or mutable fixture can conflict. Without confirmed worktree/cwd wiring,
serialize conflicting writes and post-condition checks. Do not claim automatic isolation
from an `isolation` option alone. Independent repositories may still share other resources.

A terminal message does not prove background writes ended. Before a dependent write or
acceptance stage, require actual settlement of relevant handles. A kept preview requires
an acknowledged owner transfer and explicit cleanup responsibility; a reachable URL is
not a transfer receipt. Scope-close failure leaves the affected work blocked/unconfirmed.

The barrier applies to conflicting writes and checks of changing artifacts. Continue
read-only planning on stable inputs, acceptance-contract preparation, and independent
tasks. Do not turn one unsettled scope into a stop for the entire team.

Distinguish a clarification from an invalidating change. Add explanations, reporting detail,
or an already-implied acceptance check to the existing contract without automatically
cancelling valid work. Advance the goal revision and replace only affected execution when
the requested behavior, scope, or evidence validity actually changes. An artifact revision
from a repair is not necessarily a new goal revision. Old evidence never certifies a new
artifact or closes a changed goal.

For an invalidated active run, request cancellation through exposed controls and confirm
that the same run stopped and relevant writes settled before replacement. A cancellation
acknowledgment or timeout is not proof of stopping. Preserve valid upstream artifacts;
repair a known local defect within the authorized budget instead of restarting everything.

On `scope-close-incomplete`, the host fences the entire affected Workflow run and
cancels its remaining assignments before allowing an engine retry to execute. It cannot
identify a smaller resource-conflict subgraph. Other runs and independent stable-input
work may continue. Do not restart the fenced run until its outstanding resources settle.

## Capability preflight and lifecycle

Resolve the definition body and tool policy before dispatch. `available_agents` is a
scheduling hint, not a tool list. Read the definition resource, such as
`agent://builtin/plan/agent.md`, with the host's resource reader, discovering it if needed.
Omitted `tools` inherits; `tools: []` grants none; `disallowedTools` always removes tools.
The shipped `explorer` keeps read-only behavior through its prompt, not a tool allowlist,
so the caller's brief still carries the read-only expectation. A command-running
read-only assignment needs a compatible definition and an explicit read-only brief. The
shipped `coder` is normally the execution definition; its build/test checks do not combine
roles.

Require a small first probe of a necessary tool. A missing-tool result ends that
assignment; choose a capable fresh target. Do not narrate an unrun command as evidence.
For direct Agent assignments, subagents always run in the background; collect results from
the completion notification. MCP agent IDs beginning with `mcp__` follow the same
background path after approval. Respect the available model tiers and
any explicit user model choice. Do not weaken an implementation merely to reduce cost.

Active clarifications may use `Agent(resume_thread_id: <child_thread_id>, prompt: ...,
send_only: true)` when exposed, without changing the role or reactivating a finished
assignment. If it fails because the receiver ended, do not remove `send_only` and retry.
A queued clarification is not evidence that it was consumed.

## Recover and keep the task record

After compaction, reload this skill if archived and restore its activation scope and user
overrides. Reconcile existing assignments before any new dispatch. A failed lookup does
not prove a run never started; recover a nonempty actual binding or keep it unconfirmed.
Use exposed status/cancellation controls, never thread resume as a status query.
`AgentResult` is hidden and is not a polling API. Wait for automatic results where applicable.

Keep one Todo record per business task, with its internal assignments or Workflow run,
current goal revision, inputs, required evidence, and outstanding resources. Preserve
`task_id`, `run_id`, and `child_thread_id` as different identities; never derive one from
another. Retain failed/cancelled attempt evidence and unresolved obligations. A launch
receipt opens work; it does not complete it. Do not duplicate in-flight work or repeat
project reconnaissance merely to rebuild the record.

## Close once from evidence

Use the saved formal result and the current artifact version. Workflow completion may
include a bounded reported-result preview: it is script output, not an engine acceptance
verdict. If it is complete and sufficient, summarize it directly. If partial or missing
necessary evidence, retrieve the saved aggregate report once. Do not restart an agent to
obtain or repeat its report, and do not treat a cleanup-only sentence as the deliverable.

Keep execution, acceptance, resource settlement, and external delivery separate.
`completed`, exit code zero, an assignment receipt, or message `accepted` alone cannot
establish the other facts. Query a required pending delivery when useful; retry a known
retryable failure using the original message identity, not by repeating the implementation.

Return each task once with its goal revision, actual status, concrete summary, current
report/artifact references, actual verification basis, resource state, blockers, and next
action. Every reference must have been observed. Explain failed/blocked work and preserve
usable output. Unknown engine acceptance is not itself failure. Do not poll unchanged
state or ask finished workers to reconfirm it. A later verifiable defect requires an
explicit correction; unsupported free-form follow-ups do not rewrite the recorded outcome.

This policy does not implement durable recovery, idempotency, isolation, or reliable
notifications. Use the host's actual capabilities and report gaps truthfully.
