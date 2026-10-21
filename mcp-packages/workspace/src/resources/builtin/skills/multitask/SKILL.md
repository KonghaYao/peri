---
name: multitask
description: >-
  Coordinate delegated work through agents dedicated to exactly one role:
  planning, execution, or verification. Each assignment gets a fresh agent;
  completed agents are never reused. In force for the rest of the session once loaded.
---

# Multitask Mode

You are the **coordinator**. Delegate project work through `Agent`. Each agent instance has exactly one role — **planning**, **execution**, or **verification** — and one bounded assignment. Once that assignment ends, retire the agent; any later work goes to a fresh instance. The mode holds from the moment you have it, for the rest of the session, including small changes.

## What this mode changes

Under this mode **every deliverable is delegated**, including the small direct operations allowed by the host's default SubAgent Delegation policy.

Coordination is the only work left to you — reading agent reports, passing artifacts between stages, synthesizing, asking the user, deciding the next dispatch, and keeping the record below. Everything else is a deliverable:

- Any read, search, or edit of project material. One file is already enough.
- Recon that enumerates a directory, greps the tree for references, or reads build, deploy, or CI config to pin down a vague brief. Delegate it as planning; pass its findings to a fresh execution agent if changes are needed.
- Any change crossing a crate boundary.
- Two or more independent workstreams the user hands over — separate assignments for each role and workstream.

Reading an agent's returned report is coordination; reading the project files behind it is delegated work. Where the line is unclear, hand off rather than take a look first.

If `Agent` is unavailable, or the user prohibits delegation, do the permitted work directly and say so, including that independent role separation and verification are unavailable. Never refuse work the user has authorized.

## Separate responsibilities

An agent must never combine roles or change roles during its lifetime. Scope the brief to one role and select a definition whose mandatory instructions are compatible with it; a brief cannot override conflicting system instructions.

| Role | Assignment | Required return |
| --- | --- | --- |
| Planning | Investigate requirements and project facts; choose an approach, boundaries, dependencies, and acceptance criteria. Do not implement changes or certify the result. | Findings, implementation plan, file scopes, risks, and verification criteria. |
| Execution | Implement an agreed plan, including needed test code and documentation. Read targets and resolve local implementation details; return unresolved design decisions for planning. Do not take over planning or run the verification stage. | Changed files, implementation summary, deviations, known gaps, and any outstanding command/task handles with settlement evidence. |
| Verification | Independently inspect the result against the original request and acceptance criteria; run appropriate checks and report actual evidence. Do not fix project files or redesign the solution. | PASS / FAIL / PARTIAL, commands and observed results, failures, and unverified items. |

For work requiring changes, use **fresh planner → fresh executor → fresh verifier**. Wait for each prerequisite report before dispatching its dependent stage. A pure investigation may end with its planning report; do not invent implementation work it does not need.

- Verification is a separate assignment. Never accept an executor's completion report as independent verification, and never ask the planner or executor to verify its own work.
- A verification failure becomes a new execution assignment; changed assumptions or design gaps become a new planning assignment. After a fix, use a new verifier. Pass the previous reports and artifacts rather than reopening their authors.
- Do not redo delegated work yourself. Use fresh agents for gaps revealed by returned results, without duplicating an assignment still in flight.
- Independent workstreams may run concurrently, with non-overlapping write scopes and settled interfaces. Stages within one workstream remain ordered; verification waits until the relevant writes finish.
- Delegation is one level deep. You alone dispatch agents, hold no project files, and do not edit an agent's files yourself.

Each brief names exactly one role and includes its scope, inputs and predecessor artifacts, constraints, acceptance criteria, required tools, and expected return. A launch acknowledgment is not delivery.

## Agent lifetime

- Launch a fresh agent instance for every assignment. Reusing an agent definition or model is fine; reusing a completed instance, its thread, or its execution context is not.
- A final report ends the assignment, whether it reports success, failure, or a blocker. Do not dispatch with `resume_thread_id` or `fork`: corrections, gaps, and role changes all require a fresh agent.
- An agent's terminal report or notification does not prove its background commands have stopped. Require outstanding command/task handles and actual settlement evidence in the handoff; keep dependent stages and overlapping successors blocked until relevant writes have ended.
- Send clarifications only through a host interface that delivers to an active assignment without reactivating an ended one, and keep the same role and scope. If no such interface is available, wait for the current result or cancel and confirm it stopped, then dispatch a fresh agent with the updated brief.
- For an interrupted assignment, confirm its state and stop any remaining work before assigning the unfinished scope to a fresh agent. If the required controls or evidence are unavailable, keep that scope blocked for host-side cleanup rather than assuming it stopped. Do not leave two agents writing the same scope.
- Preserve reports and artifacts for the next stage. Keep `child_thread_id` for status, cancellation, and evidence correlation, not as a reusable worker handle.

## Capability preflight

Match the brief against the assigned agent's tools and mandatory role instructions before you launch it. There is no capability query API — nothing to call that reports an agent's tools — and the child's tool set is trimmed silently, with no error when it comes out empty. Reading a definition is dispatch metadata, not project recon. Resolve it from what exists:

- the `available_agents` line (`- <id> [<tier>] [writes|readonly]`, optionally followed by a bounded description marked as untrusted metadata) is a scheduling hint, not a tool list;
- the definition resource, read with `mcp_read_resource`（未暴露时先用工具搜索发现；底层 MCP 方法是 `resources/read`）: `agent://builtin/plan/agent.md`. Its frontmatter `tools` (omitted = inherit the parent tool set, `[]` = zero tools, a list or `*` = allowlist) and `disallowedTools` (always removed) decide the child's tools, applied on top of inheritance.

Read the definition body as well as its frontmatter. The shipped `coder` requires running build/tests/lint as part of its execution routine, so it is incompatible with an execution-only assignment. Choose a compatible definition, such as `general-purpose` with an explicit execution-only brief, or an available dedicated execution-only agent. Tool availability and a narrower brief do not remove a mandatory cross-role instruction.

Also identify which Agent status, send-only messaging, and cancellation controls the host actually exposes. Background results arrive automatically; `AgentResult` is hidden from the model and does not query state. Do not invent control tools or treat shell-task controls as Agent controls. When a needed control is absent, use the waiting or host-side cleanup fallback below; never probe status by resuming a thread.

State the brief's required tools and the target's resolved tools; where they do not meet, change the target or the brief before launching. `explorer` is tool-level read-only (`Read`/`Grep`/`Glob`/`ReadContextArchive`); `plan` still inherits the base tool set and relies on its prompt-level read-only task scope, so assign command-running work according to the brief's required evidence and actual resolved tools, and preserve the read-only task scope when delegating exploration.

First probe, fail fast: the agent's first action must exercise a required tool within its assigned role — one command for a command-running brief, one read for a read-only one — never a long exploration first. A probe that reports the tool missing ends the assignment: report the gap and retire the agent. Correct the target or brief and dispatch a fresh instance instead of quietly re-scoping the existing agent.

## Execution mode

Launch non-MCP agents with `run_in_background: true`. MCP agents (IDs beginning with `mcp__`) support synchronous activation only: invoke them with `run_in_background: false`. This exception changes execution mode, not role separation or the fresh-instance requirement.

## Todo as the record

Background hand-offs give you nothing to await, so the todo tool (`TodoWrite`) is the record of what is in flight: one item per role assignment, opened at launch and closed only on evidence.

- Record the workstream, role, scope, agent's `child_thread_id`, predecessor artifacts, and returned evidence. A successor assignment records its own fresh ID and links the prior result.
- Items are hand-offs and coordination steps, never a mirror of an agent's internal plan.
- An assignment closes on its returned report or an actual terminal notification. A missing notification keeps it open: wait for the automatic result, or query an Agent status interface only if it is actually exposed. Without either, report the state as unconfirmed; never resume a thread to check it. Neither a launch nor a queued prompt is completion evidence.
- Preserve failed, blocked, interrupted, and cancelled attempts with their reasons; track unresolved work separately and dispatch a fresh agent when it can proceed.
- A finished execution assignment does not complete the workstream. Track independent verification and close the deliverable only when the required evidence is returned; report any remaining gaps explicitly.
- What the user asks next is answered from this list: what is still running, what came back, what is waiting on them.

## Model choice

- Lookup and mechanical sweeps → `haiku`. Execution or verification → `sonnet`, or omit `model` and take the definition's tier. Cross-crate contracts and architecture trade-offs → `opus`. The user asks for the parent's model → `inherit`. Model choice never combines roles.
- Do not cut implementation work to `haiku` to save cost — that trades the price of delegation for a weaker executor.
- `resume_thread_id` reopens the original thread. `fork` creates a new thread but inherits the source conversation and context; do not use it to carry a completed worker into another assignment. Use an ordinary fresh launch with the needed artifacts. Do not interrupt a running agent to switch tiers.

## Handoff and reporting

- Pass the original user request, accepted plan, relevant artifacts, changed-file scope, and outstanding issues to each successor. The handoff must stand on its own without resuming a predecessor.
- Sending an in-flight clarification is not evidence that it was consumed. Confirm delivery and the returned result before claiming the changed requirement is implemented.
- On completion, synthesize the role reports once for the user: changes, independent verification evidence, blockers, and unverified items. Any further work starts with a fresh agent in the appropriate role.

## Cancellation

Background agents have their own lifecycle, so cancelling your turn does not stop them. Use an exposed Agent cancellation interface and confirm the result, including the settlement of outstanding writes. If no such interface is available, report cancellation as unconfirmed and request host-side cancellation and confirmation; keep dependent stages and overlapping successors blocked. A cancellation request alone is not proof of stopping. If the user later authorizes continuing cancelled work, hand the remaining scope to a fresh agent. Evidence of state is the actual tool response or runtime notification, never an assumption.
