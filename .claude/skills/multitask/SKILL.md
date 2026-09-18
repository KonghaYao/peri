---
name: multitask
description: >-
  Multitask mode hands non-trivial workstreams to exactly one owner through
  `Agent`, keeping the foreground free to coordinate. Use when the user enables
  multitask (`/multitask`) or hands over two or more independent workstreams; the
  ownership and tier rules also answer questions about delegation without enabling
  the mode.
---

# Multitask Mode

Work splits into a foreground **coordinator** and **owners** that take work through `Agent`. The user enables the mode, and the injected body then stays in the transcript for the rest of the session. A compact drops it — re-load it, or say the mode lapsed, rather than letting it disappear quietly.

Two branches: **enabled** — the hand-off rules below are binding. **Consulted** — the user asked about ownership, follow-up, or tiers without enabling the mode; answer from these rules and change nothing about how the current task runs.

## What this mode changes

The host default works in the foreground whenever it fits, and puts direct work at two to three files. Enabled, this mode **hands off everything outside that zone**.

Hand off when any of these holds:

1. More than three files are in scope.
2. The change crosses a crate boundary.
3. The user hands over two or more independent workstreams — one owner each.

Direct work stays direct: reads, searches, and edits of at most three files in one crate, plus coordination — reading results, synthesizing, asking the user, deciding the next step. Reading and searching to size the work is not a delivery, so it never triggers a hand-off by itself.

If `Agent` is unavailable, or the user prohibits delegation, do the permitted work directly and say so. Never refuse work the user has authorized.

## One owner

A handed-off deliverable — investigate → implement → verify — has exactly one owner, and the coordinator is never that owner. Absorbed:

- Do not redo handed-off work in the foreground or in a second owner; follow up only where results expose a gap.
- Do not split one deliverable into roles that wait on each other.
- Siblings only for genuinely independent workstreams, with non-overlapping write scopes and settled interfaces. The foreground must not edit files an owner is holding; wait, or hand ownership over explicitly.
- Delegation is one level deep, so the coordinator alone adds siblings. At the concurrency limit, wait for completion notifications instead of launching around it.

Before handing off: does this work already have an owner, and does my write scope overlap it? Then give the brief a file scope, constraints, acceptance criteria, and the expected return. A launch acknowledgment is not delivery.

## Model choice

- Lookup and mechanical sweeps → `haiku`. Implementation with verification → `sonnet`, or omit `model` and take the definition's tier. Cross-crate contracts and architecture trade-offs → `opus`. The user asks for the parent's model → `inherit`.
- Do not cut implementation work to `haiku` to save cost — that trades the price of delegation for a weaker executor.
- `resume_thread_id` and `fork` reuse the original environment and ignore `model`, so never drive a model change through either. Do not interrupt a running owner to switch tiers.

## Follow-up

- Keep the `child_thread_id` and follow up on the same work through `Agent(resume_thread_id: ...)` rather than creating a replacement owner; read the returned `action` to tell sending from resuming.
- Interrupted with work left and no cancellation: resume that thread. If it is still active but has no live receiver in this session, report the blocker rather than silently recreating it.
- A sent `prompt` is queued rather than interrupting an in-flight call. Until it appears in the transcript, do not claim the changed requirement is implemented.
- On completion, do only gap follow-up and one synthesis for the user: changes, verification evidence, blockers, unverified items.

## Cancellation

Synchronous tasks inherit the parent's cancellation; background tasks have their own, so stopping the foreground does not stop them. Use the host's cancellation interface and confirm the result. Cancelled work is not resumed automatically. Evidence of state is the actual tool response or runtime notification, never an assumption.
