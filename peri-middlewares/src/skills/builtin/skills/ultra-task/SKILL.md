---
name: ultra-task
description: >
  Adaptive task supervision through subagents. Use when the user invokes
  "ultra-task", asks the main agent to act as a task supervisor, or wants
  subagents to perform most of a non-trivial task while the main agent
  coordinates and verifies. Do not use for simple one-step work, scripted
  Workflow pipelines, or Ultra-ADLC-scale audited delivery.
userInvocable: true
argumentHint: "[goal to supervise through subagents]"
---

# Ultra Task

Act as the task supervisor. The Main Agent is the **control plane**: it owns the
user's intent, task graph, authorization boundary, integration, and completion
verdict. Subagents are the **execution plane**: delegate most bounded
investigation, implementation, testing, and review work to them.

An explicit `/ultra-task` invocation always selects this mode. Delegation is not
a quota: keep decisions that change user intent, permissions, scope, or global
architecture in the Main Agent, and perform tiny coordination or integration
actions directly when delegating them would cost more than doing them.

Use the direct `Agent` tool. Do not switch to `Workflow` or Ultra-ADLC unless the
user separately asks for that mode. Subagents cannot launch further subagents.

## 1. Establish control

Before dispatching work:

1. Read the repository instructions and inspect enough primary evidence to
   identify the real task boundary.
2. Translate the request into acceptance requirements and a dependency-aware
   ledger of work packages.
3. Record for each package: objective, dependencies, owner, read/write scope,
   expected artifact, acceptance evidence, and state.
4. Map every acceptance requirement to a package or to an explicitly retained
   Main-Agent responsibility.

Keep the ledger in the current plan unless the user requested a durable artifact.
Use these states: `pending`, `running`, `candidate`, `accepted`, and `blocked`.
A subagent result is only `candidate` until the Main Agent checks its artifacts
and evidence.

Ask the user before fan-out only when a missing product decision or authority
would materially change the result. Do not ask them for facts available in the
workspace.

This step is complete when all acceptance requirements have an owner, observable
evidence, and non-overlapping write ownership.

## 2. Issue work contracts

Delegate packages that are ready according to their dependencies. Each Agent
prompt is a work contract and includes:

```text
Objective: one bounded outcome
Context: only the facts needed to act correctly
Ownership: exact files, modules, or result dimensions this agent owns
Boundaries: what it must not change or decide
Inputs: repository instructions, dependencies, and prior results to consume
Acceptance: commands, observations, or artifacts that prove the work
Budget: search depth, effort or output limit, and an explicit stop condition
Return: concise result, files changed, evidence, blockers, and remaining work
Coordination: other agents may be editing; preserve unrelated work and never revert it
```

Select the narrowest suitable built-in or project agent:

- `explorer` for specific read-only codebase questions; include the required
  quick, medium, or very-thorough search depth.
- `coder` for a decided implementation with exclusive file ownership.
- `verification` for independent, read-only attempts to disprove completion.
- `web-researcher` for current external-source research.
- `general-purpose` when the package legitimately mixes several kinds of work.

Use a new defined-type subagent for an isolated, self-contained contract. Use
`fork: true` only when the package genuinely needs the full conversation context;
do not repeat that context in the fork directive. Set `cwd` when the package is
scoped below the current directory.

Choose a model override only when it changes the expected outcome: `haiku` for
bounded lookup or extraction, `sonnet` for ordinary implementation and review,
and `opus` for high-risk cross-boundary reasoning. Do not spend a stronger model
on mechanical work by default.

Never give two live writers overlapping file ownership. Tell every writer that it
is not alone in the workspace and must accommodate concurrent changes. Never
delegate an action that the user has not authorized; approval of `Agent` transfers
the inherited tools to the subagent without per-tool approval.

This step is complete when every launched subagent can execute without guessing
its scope, authority, dependencies, or completion criterion.

## 3. Run adaptive waves

Dispatch independent ready packages together. Use background Agents only when the
Main Agent has useful, non-overlapping control-plane work to do while they run;
never exceed the Agent tool's limit of three concurrent background tasks. Use
synchronous Agents when their result is needed before the task graph can advance.

While a wave runs, the Main Agent may refine downstream contracts, inspect shared
interfaces, resolve dependencies, and prepare integration. It must not duplicate
the substantive work already assigned to a live subagent.

Stop a package when its acceptance evidence is sufficient. For bounded inspection
tasks, do not exhaustively enumerate adjacent issues or expand into a repository
audit unless the user asked for that scope; record important out-of-scope findings
as residual risks instead.

When a result arrives:

1. Compare it with the package contract.
2. Inspect the referenced files or artifacts rather than trusting the summary.
3. Check that claimed commands reached a terminal result and produced meaningful
   evidence.
4. Mark the package `accepted`, return it for a bounded repair, or replan its
   dependants.
5. Dispatch the next ready wave from the updated ledger.

If an Agent call is interrupted or fails and returns a `child_thread_id`, resume
that exact execution with `Agent(resume_thread_id: ...)`; do not start over and
discard its context or side effects. If the contract itself was wrong, correct the
contract before retrying. After repeated failure, reduce scope, change the agent
type or model, or take back the blocked decision at the control plane.

This step is complete when no ready package remains unassigned and every finished
package is either accepted or has a concrete repair or escalation path.

## 4. Converge on evidence

Integrate accepted work and test the combined result. For non-trivial changes,
give an independent `verification` subagent the original user request, changed
files, implementation approach, and acceptance requirements. The verifier must
run the relevant checks and return `PASS`, `FAIL`, or `PARTIAL` with command
evidence; it does not repair files.

Treat `FAIL` as new work: assign the smallest repair package to an appropriate
writer, then repeat the affected verification. Treat `PARTIAL` as completion only
when the missing check is genuinely environmental, the delivered scope is still
sound, and the limitation is reported to the user. A polished summary, a worker's
self-test, or a background completion notification is not independent evidence.

Do not declare the task complete until:

- every acceptance requirement maps to an accepted artifact and evidence;
- no delegated task that matters to the result is still running;
- integration checks have reached terminal outcomes;
- no known, fixable gap remains hidden behind "mostly complete" language; and
- the final workspace preserves unrelated user changes.

Report the unified outcome, material files or artifacts, verification evidence,
and any real residual risk at the user's requested level of detail. Default to a
compact synthesis instead of reproducing the ledger or every subagent finding.
Mention individual subagents only when their division of responsibility helps the
user understand the result.
