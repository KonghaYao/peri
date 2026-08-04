---
name: auto-devflow
description: >
  Use when starting an issue, bugfix, feature, or refactor that benefits from an
  adaptive development workflow. Select lite, normal, pro, max, or ultra from task
  complexity and risk, then use only the coordination, review, and verification
  phases that the task actually needs.
argumentHint: "[lite|normal|pro|max|ultra] <task>"
---

# Adaptive Devflow

Run the **lowest sufficient devflow**. The main agent remains the controller, but the amount of ceremony and delegation scales with task complexity:

```text
lite < normal < pro < max < ultra
```

A lower mode may absorb a phase into the controller; it must not discard that phase's responsibility. For example, `lite` replaces a review subagent with a controller self-review, not with no review.

## Non-Negotiables

These rules apply in every mode:

- The main agent owns scope, mode selection, approvals, TodoWrite, git safety, and final reporting.
- Use the smallest safe mode, but never trade away correctness, security, or required verification to save steps.
- Subagents do focused work only. Do not let a subagent silently broaden scope.
- Run writable phases sequentially. Never run two coding agents against the same checkout at the same time.
- Ask before destructive operations, branch changes, commits, or scope expansion. Never commit unless the user explicitly requested it.
- Inspect the actual diff and collect verification evidence before claiming completion.
- Treat user-visible behavior, public contracts, persistent data, security boundaries, and irreversible operations as risk signals, regardless of file count.

## Select a Mode

Accept an explicit mode in forms such as:

```text
/auto-devflow lite fix the typo
/auto-devflow pro implement issue 123
use max mode for this migration
```

Selection order:

1. If the user explicitly names a mode, use it as the requested mode.
2. Scan scope, uncertainty, coupling, reversibility, and verification burden.
3. Raise the requested mode to any applicable safety floor.
4. If no mode was named, choose the lowest mode whose conditions all fit.
5. Perform only enough initial inspection to validate the choice before coding.

Do not ask the user to choose a mode when the evidence is sufficient. State the decision in one line:

```text
Mode: pro — cross-module behavior needs dedicated exploration and independent review.
```

For `lite` and `normal`, record this line in the visible plan or TodoWrite. For `pro`, `max`, and `ultra`, also record it in `00-context.md`, including required and omitted gates.

### Mode Anchors

File counts are indicators, not quotas. Ownership boundaries and risk dominate raw size.

| Mode | Choose when | Typical strategy |
| --- | --- | --- |
| `lite` | The task is obvious, localized, low-risk, reversible, and has a clear targeted check. All of these must be true. | Controller works directly; no subagent or handoff directory. |
| `normal` | The task is bounded to one ownership area, requirements are clear, risk is low to moderate, and implementation needs limited codebase context. | Controller explores and plans; one `coder` implements; controller reviews and verifies. |
| `pro` | The task is non-trivial, crosses several files or one meaningful boundary, or needs dedicated discovery and planning to avoid guessing. No high-risk floor applies. | Standard `explorer → plan → coder → code-reviewer` pipeline with handoffs. |
| `max` | The task is subtle or high-risk: public contracts, persistent data, security/privacy, concurrency/lifecycle, broad refactors, or costly rollback. | Mandatory plan review and gate; sliced implementation, review loop, independent verification. |
| `ultra` | The task combines multiple `max`-level risks, spans independent subsystems/workstreams, is a large migration, or explicitly needs multi-agent orchestration. Parallel work can materially improve evidence. | Parallel read-only analysis and adversarial review; synthesized plan; sequential writes; multi-lens verification. |

Automatic selection uses this order after applying an explicit request and safety floors:

1. `ultra` when the task meets the `ultra` anchor.
2. `max` when any safety floor or `max` anchor applies.
3. `lite` when every `lite` condition is true.
4. `pro` when dedicated exploration/planning is needed or a meaningful boundary is crossed.
5. `normal` for the remaining bounded work.

This makes `normal` the fallback, not the universal default.

### Safety Floors

Use at least `max` for behavioral changes involving any of these:

- authentication, authorization, secrets, privacy, or other security boundaries
- schema/data migrations, destructive transforms, or difficult rollback
- public API, wire protocol, persisted serialization, or compatibility contracts
- concurrency, async lifecycle, event-loop ownership, or race-sensitive state
- cross-crate or cross-service architecture boundaries with user-visible impact

Use `ultra` when a `max`-level task also has multiple independent subsystems, rollout/rollback workstreams, or competing designs that need parallel evidence. Do not choose `ultra` merely because a task is described as “large.”

If the user requests a mode below a safety floor, state the override and reason before coding. A user may always request a higher mode.

### Reclassification

Mode selection is not permanent:

- Escalate immediately when new scope, uncertainty, or risk appears.
- An auto-selected mode may move down before coding if inspection disproves the higher-risk signal.
- Never silently downgrade an explicitly requested mode.
- Do not downgrade after coding merely to remove an outstanding review or verification gate.
- Record every change as `Mode: old → new — reason`; update `00-context.md` when it exists.
- If escalation adds user-visible cost, architecture decisions, or approval gates, tell the user before proceeding.

Completion criterion: the chosen mode, reason, safety floor, required phases, and deliberately absorbed/omitted phases are unambiguous.

## Strategy Matrix

| Mode | Required flow | Delegation | Persistent handoffs |
| --- | --- | --- | --- |
| `lite` | Intake/inspect → direct code → self-review → targeted verify → report | None | None |
| `normal` | Intake → controller explore/plan → code → controller review/verify → report | One `coder` | None |
| `pro` | Intake → explore → plan → code → review → verify → report | `explorer`, `plan`, `coder`, `code-reviewer` | Required |
| `max` | Intake → explore → plan → plan review → gate → sliced code ↔ review → independent verify → report | Pro agents plus plan reviewer and `verification` | Required |
| `ultra` | Intake → parallel explore → synthesized plan → parallel plan review → gate → sequential sliced code ↔ multi-lens review → integrated verify → report | Orchestrated specialist agents; one writer at a time | Required, including synthesis |

A phase shown as controller-owned is still required. Do not dispatch an agent merely to make the workflow look larger.

## Handoff Policy

`lite` and `normal` do not create `.peri/plans/` ceremony. In `normal`, the coder's returned result must state changed files, commands, residual risks, and blockers; the controller verifies those claims against the diff.

For `pro`, `max`, and `ultra`, create one directory per devflow:

```text
.peri/plans/<title>/
  00-context.md
  01-explore.md
  02-plan.md
  02-plan-review.md       # max/ultra
  03-code.md
  04-review.md
  05-verification.md
```

Use a kebab-case task title and append an issue id when present. `ultra` may add lens-specific files such as `01-explore-tests.md` or `02-plan-review-security.md`, but it must synthesize them into the canonical files above before the next phase.

Each handoff file must contain:

```markdown
# <Phase> Handoff

## Goal
<one sentence>

## Scope
<in scope / out of scope>

## Findings or Changes
<phase-specific details>

## Files
<relevant file paths>

## Commands
<commands run and outcomes, or "not run">

## Open Questions
<none, or exact blockers>

## Next Prompt
<self-contained prompt for the next subagent>
```

Use handoff files whenever two or more subagents exchange work. Read-only subagents return the complete handoff content to the controller; the controller writes it to the canonical file before dispatching the next phase. Completion criterion: the next subagent can proceed from the canonical handoffs plus the original request without hidden context or guessing.

## Flow

### 1. Intake and Mode Gate

For every mode:

- capture the original request, success criteria, and user constraints
- read applicable repository guides, standards, active specs, and neighboring code
- inspect current branch/status and identify overlapping dirty changes
- select and announce the mode before dispatching subagents or editing
- identify approvals required for commits, branch changes, destructive work, or scope expansion

For `pro+`, write `00-context.md` with the above plus the mode rationale, safety floor, required gates, and absorbed/omitted phases.

Stop and ask only when the target, success criteria, or a required user decision remains ambiguous. Do not use mode selection itself as a reason to ask when it can be inferred.

Completion criterion: the task has a checkable success condition and can safely enter its selected flow.

### 2. Explore

Scale exploration by mode:

- `lite`: the controller reads the target and enough surrounding code to confirm the obvious seam and test.
- `normal`: the controller performs bounded searches, reads neighboring conventions, and prepares a self-contained coder prompt.
- `pro`: dispatch one read-only `explorer` to inspect execution paths, ownership, tests, conventions, and risks; the controller writes its returned handoff to `01-explore.md`.
- `max`: use a dedicated `explorer`; require explicit contract, rollback, regression, and dirty-worktree findings, then have the controller write `01-explore.md`.
- `ultra`: run two or three independent read-only lenses in parallel, such as architecture/behavior, tests/regressions, and safety/rollout. Give each lens a distinct question, then synthesize all evidence into `01-explore.md`.

Parallelize only independent read-only work. If two streams need the same evolving result, sequence them instead.

Completion criterion: likely files, ownership boundaries, tests, known traps, and at least one implementation seam are known at the depth required by the selected mode.

### 3. Plan and Plan Review

- `lite`: state one to three implementation/verification steps inline. Do not create a plan file.
- `normal`: the controller writes a concise, self-contained coder prompt with exact scope and verification expectations.
- `pro`: dispatch `plan` using `00-context.md` and `01-explore.md`; the controller writes the returned handoff to `02-plan.md`, then self-checks scope, files, safety, and verification.
- `max`: create `02-plan.md`, then dispatch an independent read-only plan reviewer. The controller writes its verdict to `02-plan-review.md`, which must say `APPROVED` or list concrete issues. Resolve every critical issue before coding.
- `ultra`: synthesize one canonical plan with slice boundaries, dependencies, rollback points, and verification ownership. Run independent plan-review lenses in parallel, then merge their verdicts into `02-plan-review.md`. Resolve every critical issue before coding.

For `max` and `ultra`, the plan review is a mandatory gate. Present the plan summary to the user and wait when it changes user-visible behavior, public contracts, persistent data, migrations, architecture boundaries, or requires destructive work. Otherwise the controller may approve the gate after all critical findings are resolved.

Completion criterion: the coder can implement exact ordered slices without guessing, and every mandatory review gate is approved.

### 4. Code

- `lite`: the controller implements the minimal change directly.
- `normal`: dispatch exactly one `coder` with the controller's self-contained prompt. If the task is too small to justify one coder, reclassify it as `lite` before coding.
- `pro`: dispatch exactly one `coder` using the canonical handoffs; write `03-code.md`.
- `max`: split large plans into safe sequential slices. Use one `coder` at a time and review each slice before the next risky dependency.
- `ultra`: orchestration may schedule many read-only agents, but all writes remain sequential. Assign one coder per approved slice and preserve a single canonical `03-code.md` ledger.

Every coder must:

- edit only approved files and follow repository style
- use absolute worktree paths and read each target before editing
- stop instead of guessing when the plan is wrong or scope must expand
- run targeted checks when practical
- report changed files, commands, deviations, and residual risks

After each writable phase, inspect `git diff --stat` and the actual diff in the intended checkout. If a worktree is involved, also confirm that the main checkout has no unexpected edits.

Completion criterion: the implementation matches the approved scope, its ledger matches the actual diff, and no unexplained files changed.

### 5. Review and Fix Loop

- `lite`: controller self-reviews the diff for request fit, regressions, error handling, security, and accidental edits.
- `normal`: controller performs the same diff review and checks the coder's claims against the repository state.
- `pro`: dispatch one read-only `code-reviewer`; write `04-review.md` with `APPROVED` or severity-ranked findings.
- `max`: independently review every meaningful slice and the final integrated diff. Do not proceed past unresolved critical/high findings.
- `ultra`: run only relevant independent review lenses in parallel, such as spec correctness, architecture, security, performance, and test adequacy. Parallel reviewers must have read-only tool restrictions; otherwise sequence them. Synthesize one canonical verdict in `04-review.md`; do not create redundant lenses with the same question.

When review finds an in-scope issue, send a focused fix prompt to one `coder`, update the code ledger, and re-review the affected surface. Ask before accepting scope expansion or an unsafe workaround.

Completion criterion: review is approved at the selected mode's depth, or the task is explicitly reported as blocked.

### 6. Verify

Verification always uses the final diff, not only subagent claims:

- `lite`: run the smallest targeted test, lint, build, or deterministic inspection that proves the change.
- `normal`: run targeted tests plus the relevant module-level lint/build check discovered during exploration.
- `pro`: run every command planned in `02-plan.md`; the controller writes `05-verification.md`.
- `max`: dispatch an independent `verification` agent and include relevant integration/regression checks. The controller adjudicates the evidence.
- `ultra`: assign independent verification lenses only when they prove different properties, such as behavior, compatibility, and regression safety. Run them in parallel only when their tools are read-only or their checkouts are isolated; otherwise run them sequentially. Then run or inspect the final integration gate and synthesize `05-verification.md`.

For any mode, each planned check must be passed, explicitly skipped with a reason, or reported as blocked. Do not claim success because a command was merely started. After two failures for the same reason, stop repeating the same tactic and diagnose or ask.

Completion criterion: evidence directly covers the success criteria and the final report can distinguish passed, skipped, and blocked checks.

### 7. Report

Reply with:

- selected mode and any reclassification
- outcome and files changed
- verification evidence
- unresolved risks or follow-up work
- whether a commit was created, only when explicitly requested

Do not claim completion without final verification evidence.

## Mode-Aware Controller Checklist

Use TodoWrite whenever the work has three or more concrete steps; `pro+` always qualifies.

### `lite`

1. Announce mode and inline success check.
2. Inspect, edit, self-review, and run the targeted check.
3. Report evidence.

### `normal`

1. Announce mode and create a concise TodoWrite plan.
2. Explore and prepare one bounded coder prompt.
3. Dispatch one coder; inspect its diff.
4. Controller review and verification.
5. Report evidence.

### `pro`

1. Write context, explore, and plan handoffs.
2. Controller plan self-check.
3. Sequential code and independent review.
4. Worktree safety gate and planned verification.
5. Final report.

### `max`

1. Complete all `pro` items.
2. Mandatory independent plan review and approval gate.
3. Sequential implementation slices with review loops.
4. Independent verification with integration/regression evidence.

### `ultra`

1. Complete all `max` gates.
2. Use `ultracode`/Workflow or parallel Agent calls for genuinely independent read-only streams.
3. Synthesize canonical artifacts between parallel phases.
4. Keep all writers sequential and verify the integrated result.

## Subagent Prompt Contracts

Use these contracts for delegated phases. Include the selected mode in every prompt.

### Explorer

```text
Mode: <pro|max|ultra>. Goal: explore <task>.
Read .peri/plans/<title>/00-context.md first.

Investigate the assigned lens only. Inspect relevant paths, conventions, tests,
contracts, and risks. Do not edit files. Return a complete handoff using the
required template; the controller will persist it.
Completion: the planner can proceed without repeating this search or guessing.
```

### Planner

```text
Mode: <pro|max|ultra>. Goal: plan <task>.
Read 00-context.md and the canonical 01-explore.md first.

Return a complete 02-plan handoff with ordered slices, exact files, verification
commands, risks, rollback points, and stop/ask conditions. Do not edit files;
the controller will persist it.
Completion: a coder can implement each slice without guessing.
```

### Plan Reviewer

```text
Mode: <max|ultra>. Goal: challenge the plan for <task>.
Read 00-context.md, 01-explore.md, and 02-plan.md.

Return APPROVED or issues with severity, evidence, and fix guidance. Check hidden
assumptions, contract impact, scope creep, rollback gaps, missing verification,
and unsafe operations. Do not edit files. Return the complete review handoff;
the controller will persist it.
```

### Coder

```text
Mode: <normal|pro|max|ultra>. Goal: implement only <approved scope or slice>.
Read the supplied context and, for pro+, the canonical handoffs.

Use absolute paths in the intended checkout. Read every target before editing.
Modify only approved files; record adjacent ideas instead of implementing them.
Run targeted checks when practical. Stop if scope or the plan is wrong.
Report changed files, commands, deviations, blockers, and residual risks.
For pro+, update 03-code.md and verify git diff --stat in the intended checkout.
```

### Reviewer

```text
Mode: <pro|max|ultra>. Goal: review <task> against the original request and plan.
Read all canonical handoffs and inspect the actual diff.

Return APPROVED or severity-ranked issues with exact file references and fix
guidance. Check spec compliance before maintainability, tests, and handoff
integrity. Do not edit files. Return the complete review handoff; the controller
will persist it.
```

### Verifier

```text
Mode: <max|ultra>. Goal: independently verify <task> on the final diff.
Read all canonical handoffs and inspect changed files.

Run checks that prove the assigned property; do not duplicate another verification
lens. Record exact commands, outcomes, skipped checks, and blockers. Do not edit
source files. Return the complete verification handoff; the controller will
persist it.
```

## Red Flags

Stop and ask the user when:

- the issue/spec is missing, contradictory, or lacks a checkable target
- the requested mode is below a safety floor and the resulting approval/cost tradeoff needs consent
- the plan changes public behavior, persistent data, architecture, or destructive scope not requested
- unrelated dirty changes overlap planned files
- a subagent reports `BLOCKED`, `NEEDS_USER_DECISION`, or proposes an unsafe workaround
- a mandatory plan or code review has unresolved critical findings
- the worktree safety gate finds edits in the wrong checkout
- verification fails twice for the same reason

Escalate rather than ask when new evidence merely requires a higher mode and no user decision is needed.

## Relationship to Other Skills

- Use `diagnose` or `diagnosing-bugs` during exploration for hard bugs.
- Use `tdd` when a stable test seam exists and test-first work was agreed.
- Use `code-review` for specialized review guidance.
- Load `ultracode` in `ultra` mode when Workflow orchestration will add real parallelism.
- Use `verification-before-completion` when available before claiming success.
