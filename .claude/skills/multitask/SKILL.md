---
name: multitask
description: >-
  Coordinates multitasking in Peri: a foreground coordinator with background
  workers when needed, defaulting to one worker for end-to-end delivery without
  duplicate work. Use when the user enables multitask, assigns independent
  parallel tasks, or discusses Agent delegation, thread follow-up, or model tiers.
---

# Multitask Mode

Multitask Mode separates work into a foreground **coordinator** and **workers** delegated through `Agent`. Use `run_in_background: true` when the foreground genuinely needs to remain available to coordinate other independent work; otherwise prefer synchronous execution. Do not create unnecessary background tasks just because of the mode's name.

Follow host permissions, current tool availability, and user authorization. This skill neither extends tool capabilities nor overrides host requirements to handle small tasks directly. Stop applying this coordination mode when the user exits it.

## Roles

- **Coordinator**: establish the delivery goal, assign ownership, launch or follow up with workers, handle clarification and independent tasks, and synthesize results. Necessary coordination reads and tool calls may be performed directly.
- **Worker**: own the authorized investigation, edits, commands, and verification loop; return results, evidence, and unfinished work.

For substantial work suited to delegation, default to one end-to-end worker. Background multitasking preserves foreground coordination capacity; it does not maximize agent count.

## Core rules

1. **Delegate substantial work, not every small task.** Independent investigation, implementation, and verification loops suit workers. Handle simple reads, small changes across a few files, and coordination directly as required by the host; needing a tool does not by itself require an agent. If `Agent` is unavailable or the user prohibits delegation, perform permitted work directly or report the blocker.
2. **Background execution needs a real purpose.** Set `run_in_background: true` only when other independent work needs to be handled or coordinated while the worker runs. Tell the user the background scope. Continue independent work if any remains; otherwise briefly state that you are waiting, stop making tool calls, and await the automatic completion notification.
3. **Default to one worker.** Keep investigation → implementation → verification for a shared-context deliverable in one worker. Do not split ordinary small or medium tasks into multiple mutually waiting roles.
4. **Do not duplicate work.** Do not repeat delegated investigation, implementation, or verification in the foreground or another worker. Follow up only when results expose a gap.
5. **Delegation is single-level.** Subagents do not have `Agent` and cannot recursively launch subagents. Workers may organize their work sequentially; the foreground coordinator decides whether to launch additional sibling workers.
6. **Make write ownership explicit.** Parallel work must be independent and have non-overlapping write scopes. The foreground must not edit files owned by a running worker either. Serialize and explicitly hand off work when files are shared, dependencies exist, or tool permissions are unclear. Follow stricter host scheduling rules. `readonly` / `writes` labels are conservative hints, not file locks or isolation guarantees; do not assume automatic worktree creation.

## When to delegate to one worker

- One end-to-end deliverable with shared context.
- An investigation → fix → verification or implementation → testing → reporting loop.
- A non-trivial task that benefits from independent context or a specialized agent.
- Substantial background work while the foreground genuinely needs to coordinate other independent tasks.

Keep ordinary bug fixes, feature implementations, and medium refactors in one worker where possible. Choose synchronous versus background execution based on the need for independent foreground work, not the task name or command duration alone.

## When to use sibling workers

Use siblings only when top-level workstreams are clearly independent, the benefit exceeds coordination cost, and the host permits concurrency:

- Separate write ownership with interfaces and dependencies already established.
- Unrelated files or services, or independent requests from the user.
- Read-only reviews seeking independent coverage, with different scopes rather than duplicate searches.

One worker remains the default. When splitting work, the coordinator assigns goals, inputs, allowed files, boundaries, and acceptance criteria. Workers do not delegate further. When runtime concurrency limits are reached, wait for completion notifications rather than repeatedly launching workers to bypass the limit.

## Coordinator checklist

Before a foreground tool call, check:

1. Is a worker already responsible for this work? If so, do not duplicate it.
2. Is this necessary coordination, synthesis, independent work, or a small task the host permits handling directly?
3. Does its write scope overlap with a running worker? If so, wait or complete an ownership handoff first.
4. Are the tool and agent type actually available, and are the invocation and follow-up actions authorized?

Provide a short `description` when launching a worker. A new defined-type worker needs a self-contained `prompt` covering the goal, context, file scope, constraints, verification, and expected return. It does not automatically inherit parent conversation history. A launch acknowledgment is not delivery completion.

## Worker follow-up

- Keep the returned `child_thread_id` and use its actual UUID. Do not pass a `task_id` or invented placeholder as `resume_thread_id`.
- Follow up on the same work with `Agent(resume_thread_id: ..., prompt: ...)`. Read the returned `action` to distinguish sending from resuming.
- **Active background worker in the current session**: a non-empty `prompt` is queued as Info, returning `action: send` / `status: queued`. This does not interrupt an in-flight model or tool call, resume execution, or trigger additional reasoning. Info enters the transcript at the next Receive, but the worker may finish before the model sees it. `queued` does not mean read or durably saved; do not claim a changed requirement has been implemented based on queuing alone. `run_in_background` is ignored when sending.
- **Non-active thread**: resume the existing thread and its records, returning `action: resume`. Omit `prompt` to continue the original task. This invocation's `run_in_background` selects the execution mode, subject to the background-use rules.
- **Errors or interruptions**: if work remains and the user has not canceled it, resume using the returned `child_thread_id` instead of creating a replacement worker and repeating the work. If the thread is still active but has no live background receiver in this session, the tool returns an error. Report the blocker rather than silently recreating it.
- Background results arrive through automatic system notifications. Do not call or poll `AgentResult`: it is a placeholder for system-generated results, not a query interface. When no independent work remains, do not poll indirectly through other tools or waiting loops.
- After completion, perform only necessary gap follow-up and user-facing synthesis. Combine parallel results belonging to one deliverable before reporting changes, verification evidence, blockers, and unverified items. Do not repeat confirmations when nothing remains.
- Identify ownership with clear task or agent names. If needed, display actual IDs as inline code; do not invent unsupported agent links.

## Approval and cancellation

- `Agent` calls follow the host's current approval mechanism. Approval to launch authorizes the subagent to execute its inherited tools; internal subagent tool calls do not receive individual HITL approvals. Define the permitted scope before delegation. Never use delegation to bypass a rejection, expand permissions, or perform unauthorized high-impact actions.
- Honor cancellation and scope changes. Synchronous subtasks inherit parent cancellation; independent background tasks have their own cancellation policy. Do not assume stopping the foreground stops the background. Use the host's actual cancellation interface and confirm the result when stopping is required. Info delivery is not immediate cancellation, and canceled work must not be automatically resumed.
- Treat actual tool responses and runtime notifications as evidence of availability, cancellation, and completed writes. Do not invent stop tools or completion states.

## Delegation examples

| Request | Strategy |
| --- | --- |
| Simple Q&A, reads, or small edits across a few files | Handle directly under host rules; tool use alone does not require delegation |
| Non-trivial bug fix | One worker owns investigation → fix → verification |
| Ordinary feature | One worker owns implementation and focused verification; run synchronously unless independent foreground work is needed |
| Large feature | Start with one end-to-end worker; the foreground assigns sibling ownership only for genuinely independent workstreams |
| Broad review while handling other independent tasks | Assign scoped read-only background workers; do not repeat their review in the foreground |
| Ordinary build / test / install | Use foreground `Bash` with a longer `timeout`, such as `300000`; duration alone does not justify background execution |
| Development server or watcher that must keep running | `Bash(run_in_background: true)` is appropriate; inspect actual status and follow its stopping and log-handling instructions |

A foreground `Bash` timeout does not necessarily mean the process has terminated. If the response says it is still running, do not start a duplicate replacement process.

## Agent invocation and model defaults

### New defined-type agents

- The tool is `Agent`. A new defined-type agent requires `subagent_type`. Choose an appropriate type from the current available catalog or valid loader suggestions; do not guess IDs or capabilities.
- This skill defaults to explicitly passing `model: haiku` for new defined-type agents, both synchronous and background. This is a skill-level choice, not Peri's global tool default. When `model` is omitted, the tool uses the agent definition's configuration.
- Select tiers such as `sonnet` or `opus` according to task complexity, risk, and user instructions. Supported tiers are `inherit`, `haiku`, `sonnet`, `opus`, and `fable`; do not use concrete model names. Unknown tiers are rejected, and actual available configuration is determined by the host.
- When the user requests the parent's model, a new defined-type agent may use `model: inherit`. Do not interrupt a running worker merely to switch models or discard progress by creating a replacement thread.

### Fork and resume

- Use `fork: true` when the task needs the current conversation context. It is mutually exclusive with `subagent_type`; do not use `subagent_type: "fork"`. A fork inherits the parent history snapshot at launch, the frozen system prompt, and a restricted core tool set. It does not receive `Agent` or all extension tools. Its `prompt` only adds the task directive.
- Forks always inherit the parent model and ignore `model`. Do not force `model: haiku` onto fork calls.
- `resume_thread_id` takes priority over `fork` / `subagent_type`, follows up on the original thread, and ignores the supplied `model`. Do not use resume as a model-switching interface. Resume rebuilds the execution environment; the defined-type path reloads the agent definition and tool restrictions, so it does not guarantee preserving the initial invocation's model override.
- Neither fork nor resume uses the `haiku` default for new defined-type agents.
