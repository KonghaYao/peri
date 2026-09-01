---
name: ultra-adlc
description: >
  End-to-end delivery orchestration for explicitly requested, very large software
  changes that must be completely implemented and independently proven. Use when
  the user invokes "ultra-adlc" or asks for an Ultra-ADLC-scale full delivery;
  do not use for ordinary features, small fixes, reviews, or ad hoc parallel work.
userInvocable: true
argumentHint: "[natural-language goal for a very large end-to-end delivery]"
---

# Ultra-ADLC

Turn one natural-language goal into a complete, audited delivery. The user owns
product intent and material risk decisions; the Main Agent discovers repository
facts, asks only questions the user can answer, and coordinates all execution.

This skill is self-contained. Use the existing deferred `Workflow` tool and its
existing `agent`, `parallel`, `pipeline`, `phase`, and ordinary JavaScript control
flow only. Do not add a DAG/runtime/RPC/event/TUI primitive or a third logical
workflow.

## Admission and completion invariants

- This mode is for a very large end-to-end task. If it was selected implicitly
  for ordinary work, do the work normally instead. An explicit `/ultra-adlc`
  invocation always selects this mode.
- There are exactly two **logical** workflows:
  `discovery-design` and `delivery-convergence`. A resumed physical run remains
  part of its original logical workflow.
- The Main Agent is the only user-interaction seam. Workflow Agents cannot and
  must not call `AskUserQuestion`.
- There are exactly three contracts: `intent.md`, `execution.md`, and
  `evidence.md`. Manifests, decisions, handoffs, artifacts, and learning records
  are audit records, not extra contracts.
- Legal terminal states are `complete`, `blocked`, and `cancelled`. There is no
  `partially_complete`. "Core complete", "mostly complete", an exhausted context,
  or a normally exited Workflow are never completion evidence.
- A task is `complete` only after one independent Completion Assessor in the
  current assessment round proves 100% coverage. Fixable gaps remain inside
  logical Workflow 2 and loop until they are fixed and reassessed.
- Never commit, push, publish, deploy, delete material data, or mutate an external
  system unless the user separately and explicitly authorizes that action.

## Preflight before fan-out

Before creating an expensive run:

1. Confirm the natural-language goal is non-empty and large enough for this mode.
2. Confirm `AskUserQuestion` is available in the Main Agent's current tool view.
   If it is absent, stop as `blocked`; Workflow Agents cannot replace it.
3. Discover the deferred Workflow capability with
   `SearchExtraTools("workflow")`, and execute it only through
   `ExecuteExtraTool("Workflow", ...)`. If it is unavailable, stop before fan-out.
4. Resolve `{cwd}/peri/adlc/`, canonicalizing existing ancestors. Refuse symlink or
   `..` traversal that escapes cwd. Verify the task directory is writable.
5. Confirm the required Peri profile aliases are usable: `haiku`, `sonnet`,
   `opus`, and, when escalation requires it, `fable`.
6. Inspect the working tree and preserve unrelated user changes. Load the
   repository and relevant module instructions before assigning work.

Workflow startup may still fail quickly when Node/the runner is unavailable.
Record that failure and stop; do not substitute an untracked inline process.

## Project record

The project-level root is always:

```text
./peri/adlc/
```

Create a safe task id outside Workflow scripts and inject it, the cwd, and all
timestamps through Workflow `args`. A task id may be
`YYYY-MM-DD-<lower-ascii-slug>[-NN]`; inspect the filesystem to choose a collision
suffix. Never use `Date.now()`, `new Date()`, `Math.random()`, random APIs, or
ambient time inside a Workflow script.

```text
peri/adlc/
├── tasks/<adlc-id>/
│   ├── manifest.json
│   ├── contracts/
│   │   ├── intent.md
│   │   ├── execution.md
│   │   └── evidence.md
│   ├── decisions/
│   ├── handoffs/
│   │   ├── workflow-1/
│   │   └── workflow-2/
│   ├── artifacts/
│   │   ├── designs/
│   │   ├── reviews/
│   │   ├── test-results/
│   │   └── workflow-provenance/
│   └── learning/
│       └── agent-performance.md
└── evolution/records/<adlc-id>.json
```

Raw Workflow state remains in `.claude/workflow-runs/<run-id>/`. Do not move or
copy its full journal into `peri/adlc/`; record the run ids in `manifest.json`
and, at the end, retain only a compact provenance summary.

The Main Agent is the single writer for `manifest.json`, user decisions, and
accepted `intent.md`/`execution.md` revisions. The Completion Assessor exclusively
owns the completion verdict and all substantive `evidence.md` sections. After the
physical Workflow run reaches a terminal state, the Main Agent may only append the
compact Workflow provenance; it must not change the assessor's verdict or coverage.
Initialize all three contract paths before Workflow 1; `evidence.md` remains a
draft until the final assessor passes.

Use this minimal manifest shape and append physical runs rather than replacing
history:

```json
{
  "schema": "peri.adlc/task-v1",
  "adlcId": "<injected-id>",
  "status": "discovering",
  "contracts": {
    "intent": { "path": "contracts/intent.md", "revision": 0 },
    "execution": { "path": "contracts/execution.md", "revision": 0 },
    "evidence": { "path": "contracts/evidence.md", "revision": 0 }
  },
  "workflowRuns": { "discoveryDesign": [], "deliveryConvergence": [] },
  "decision": { "status": "pending", "brief": null },
  "completion": { "round": 0, "verdict": null }
}
```

Allowed manifest states are:

```text
discovering -> awaiting_user_decision -> planning_delivery -> delivering
delivering -> verifying -> converging -> delivering
verifying -> complete
any active state -> blocked | cancelled
blocked -> delivering (after the required authority or external state exists)
```

`workflowRuns` has only `discoveryDesign` and `deliveryConvergence` logical slots;
each slot is an append-only list of physical run ids.

## The three contracts

`contracts/intent.md` is the user-facing truth and contains:

```markdown
# Intent
## User Goal
## Environment Facts Relevant to the Goal
## Desired Behavior
## Acceptance Scenarios
## Non-goals
## User Decisions
## Constraints
## Authorized Actions
## Stop and Escalation Conditions
```

Only explicit user choices may change user-visible behavior, scope, non-goals, or
authority. Increment `intent_revision` when they do and invalidate affected
downstream work.

`contracts/execution.md` is the Main-Agent-to-Workflow contract and contains:

```markdown
# Execution
## Intent Revision
## Repository Facts
## Selected Design
## Rejected Alternatives
## Impacted Areas
## Work Packages
## Completion Ledger
## Model Routing
## Concurrency and Write Ownership
## Verification Plan
## Handoff Plan
## Retry, Resume, and Escalation
```

Every Work Package records `id`, `goal`, `dependencies`, `profile`, allowed tools,
read scope, exclusive write scope, inputs, outputs, acceptance evidence, retry
limit, escalation profile/condition, and immutable handoff path. Exhausting one
Worker's retry limit escalates or replans the package; it does not defer or remove
the requirement. Every intent requirement must map to one or more packages, actual
implementation, and independent verification in the Completion Ledger. Unmapped
means incomplete.

`contracts/evidence.md` contains:

```markdown
# Evidence
## Delivered Outcome
## Intent Coverage Matrix
## Work-Package Coverage
## Acceptance Evidence
## Tool Evidence
## Independent Reviews
## Plan Deviations
## Remaining Risks
## Completion Verdict
## Workflow Provenance
```

Workers contribute raw evidence but cannot sign their own completion. Only the
Completion Assessor may make the final verdict complete.

## Filesystem handoff protocol

Every cross-Agent output is a small, structured Handoff. Large output belongs in
`artifacts/`; a Workflow return value contains only status, work-package id, and
handoff path. Downstream Agents read the current contracts, direct-dependency
handoffs, and necessary repository files—not the whole conversation or every
prior output.

Use this schema:

```markdown
---
schema: peri.adlc/handoff-v1
adlc_id: <injected-id>
logical_workflow: discovery-design | delivery-convergence
phase: <phase>
round: <injected-round>
work_package: <id>
agent_id: <label>
role: <role>
profile: haiku | sonnet | opus | fable
status: complete | blocked
intent_revision: <n>
execution_revision: <n>
---
# Assigned Scope
# Inputs Consumed
# Completed Work
# Decisions Within Authority
# Evidence
# Remaining Items
# Risks and Blockers
# Output References
# Next Consumer
```

Rules:

- One Agent owns one unique Handoff path. Never overwrite it; use `-r2`, `-r3`,
  and so on for revisions.
- `status: complete` requires an empty `Remaining Items` section. `blocked`
  identifies the precise external condition or authority needed.
- Each completion claim cites code, a test, a command result, or another concrete
  artifact. A narrative claim is not evidence.
- Never put a secret, token, password, private key, full connection string, or
  unnecessary user data in a Handoff, prompt, artifact, provenance, or test log.
- Agents write only their designated Handoff/artifact and exclusive product-code
  write scope. Shared code, manifest, or contract writes have a single owner.

Expected short result:

```json
{"status":"complete","workPackage":"WP-017","handoffPath":"handoffs/workflow-2/implementation/WP-017.md"}
```

## Profile routing for efficiency

Route every Agent node separately. Optimize expected wall time plus rework and
coordination cost, not the cheapest individual call.

| Profile | Default work |
| --- | --- |
| `haiku` | high-volume search, extraction, deterministic checks, evidence indexing |
| `sonnet` | implementation, local design, integration, normal review and repair |
| `opus` | global decomposition, cross-module synthesis, high-risk review, assessment |
| `fable` | root-cause arbitration or replanning after repeated Opus convergence failure |

Escalate `haiku -> sonnet` for conflicting/insufficient evidence and repeated
failure; `sonnet -> opus` for cross-module contracts or high-cost ambiguity; use
`fable` only after Opus cannot converge. Pass a compressed Handoff upward—do not
make the stronger profile repeat the whole scan. Profile misrouting is a
coordinator defect, not a Worker defect.

## Concurrency rules

Pass `maxConcurrency: 12` explicitly to both Workflow launches. Downshift only for
a known provider, budget, or host limit and record the reason. Maximize useful
concurrency:

- Start ready, independent read-only work immediately.
- Parallelize writes only when their declared write scopes do not overlap.
- Give shared files and integration to one owner.
- Prefer feature-level wavefronts (`plan -> implement -> self-test -> handoff`)
  over global phase barriers where dependencies allow.
- Prioritize critical-path packages over short non-critical work.
- Use `pipeline(items, ...stages)` for repeated homogeneous pipelines.
- `parallel` accepts zero-argument factories, never already-started promises:

```javascript
const results = await parallel([
  () => agent(promptA, { label: 'A · discovery · haiku', model: 'haiku' }),
  () => agent(promptB, { label: 'B · discovery · haiku', model: 'haiku' }),
])
```

Do not write `parallel([agent(...), agent(...)])`; it can yield a false successful
run with null results.

## Logical Workflow 1: discovery-design

Workflow 1 may read the repository and write only its unique ADLC handoffs and
artifacts. It must not begin product implementation.

1. `phase('ADLC/W1/Discover')`: fan out `haiku` Agents for architecture and entry
   points, current behavior, tests/acceptance seams, applicable repository rules,
   relevant history, compatibility, security, and existing reusable mechanisms.
2. `phase('ADLC/W1/Design')`: use parallel `sonnet` Agents for genuinely distinct
   candidate designs and risk/migration analysis. They consume discovery
   handoffs, not raw global output.
3. `phase('ADLC/W1/Synthesize')`: one `opus` owner reconciles facts and writes:
   `design-options.md`, `decision-brief.md`, an `intent.md` draft, and an
   `execution.md` draft.

`decision-brief.md` must separate confirmed facts from questions and contain:

```markdown
# Decision Brief
## Confirmed Environment Facts
## Recommended Outcome
## Decisions Only the User Can Make
## Options and User-visible Consequences
## Recommended Defaults
## Risks That Need Explicit Acceptance
```

Do not ask the user for crate names, file paths, test commands, Agent count,
Workflow structure, or facts discoverable from the repository.

Launch with an externally injected argument object:

```javascript
export const meta = {
  name: 'adlc:task:discovery-design',
  description: 'Discover the repository and prepare an Ultra-ADLC decision brief',
}

// args supplies adlcId, adlcTaskRoot, createdAt, goal, and revision values.
// Use only agent(), parallel(), pipeline(), phase(), log(), and normal JS.
```

After `ExecuteExtraTool` returns the run id, append it to the Workflow 1 manifest
slot and **do not continue to the decision seam yet**. Workflow is asynchronous.
End the current work segment and wait for the Workflow completion notification.
When notified, read `.claude/workflow-runs/<run-id>/state.json` and the referenced
handoffs. A start response or completion notification alone is not the result.

## Main Agent decision seam

Only after Workflow 1 has completed successfully:

1. Read and validate `decision-brief.md`; reject missing evidence or unresolved
   technical facts back into Workflow 1 rather than asking the user.
2. Call `AskUserQuestion` with at most three questions. Ask only user-visible
   behavior, intended scope, material risk, or irreversible commitments. Give
   concise background, mutually exclusive choices, consequences, and a recommended
   option. If the user delegates the choice, use the recommended option.
3. Persist `decisions/decision-001.md`, revise and accept `intent.md`, then produce
   the accepted `execution.md` with a complete ledger, ownership, profile, and
   verification plan.
4. Only then launch Workflow 2. Do not ask from inside either Workflow.

## Logical Workflow 2: delivery-convergence

Workflow 2 consumes accepted contract revision numbers through `args` and owns all
implementation and convergence. It is one logical workflow even when resumed after
an external blocker.

1. `ADLC/W2/Decompose`: one `opus` planning owner validates the full Work Package
   inventory and Completion Ledger. It may report contract gaps but may not narrow
   intent or move work to Non-goals.
2. `ADLC/W2/Implement/Round-N`: run ready `sonnet` implementation packages in
   parallel, with `haiku` for independent fixtures/checks. Every writer has an
   exclusive write scope and leaves a Handoff with self-test evidence.
3. `ADLC/W2/Integrate/Round-N`: one `sonnet` integration owner resolves shared
   changes, runs target integration checks, and accounts for every package.
4. `ADLC/W2/Verify/Round-N`: fan out independent `sonnet` correctness reviews,
   `opus` architecture/security reviews when warranted, and `haiku` deterministic
   checks/evidence reconciliation.
5. `ADLC/W2/Assess/Round-N`: after all verification joins, invoke exactly one new
   Completion Assessor for that round. Never run assessors in parallel or use a
   vote. Use `opus`, replacing it with one `fable` assessor only after documented
   repeated Opus convergence failure.

The assessor starts from a fresh context, did not design/code/fix/review the task,
and treats all completion claims as untrusted. Its product-code and test access is
read-only. It may write its designated assessment Handoff, an incomplete verdict's
gap Handoff, and, on a complete verdict, final `evidence.md`,
`learning/agent-performance.md`, and the task evolution record. It must not change
code or weaken tests to make the verdict pass.

Require a structured assessor result with:

```text
verdict: complete | incomplete | blocked
requirements_coverage_percent
work_packages_coverage_percent
acceptance_evidence_coverage_percent
required_tests_pass_percent
high_severity_open_count
unapproved_deferred_count
unexplained_deviation_count
gap_handoff_path
assessment_handoff_path
```

`complete` requires all four percentages to equal 100 and all three counts to
equal zero. Do not average them. For `incomplete`, write
`handoffs/workflow-2/gap-round-N.md`, compile every fixable gap into a Work
Package, then loop through parallel repair, integration, verification, and one new
assessment. Do not return from Workflow 2 while a gap is internally fixable.

For `blocked`, identify the missing external dependency or authorization precisely
and return a short blocked result. The Main Agent records `blocked`, asks the user
only if user authority can resolve it, then resumes the same logical Workflow 2
with `resumeFromRunId` and injected updated contract revisions. Append the new
physical run id; do not create Workflow 3. Cancellation likewise remains
`cancelled` and retains the audit files.

After launching Workflow 2, again wait for its completion notification and read
the saved state and Handoffs before acting. Never infer delivery from the immediate
run-id response.

## Performance and evolution record

Only when the assessor verdict is `complete`, it records every participating Agent
and the coordinator/model-routing/integration roles in
`learning/agent-performance.md`. Rate each 1–5 for completeness, correctness,
evidence quality, handoff quality, constraint adherence, rework cost, model
efficiency, and collaboration contribution; include an overall grade, strengths,
weaknesses, evidence paths, and an evolution signal.

Also write `evolution/records/<adlc-id>.json`. This task record is evidence for
later aggregation only; never modify this skill, repository instructions, or model
routing automatically from one task. An incomplete, blocked, or cancelled task
must not receive a success performance record.

## Final handoff to the user

Before reporting success, the Main Agent independently checks that the final
Workflow state is completed, the assessor verdict is `complete`, `evidence.md`
is finalized, the Completion Ledger is 100%, and the performance record exists.
Only then append compact Workflow provenance without modifying the verdict, and
update the manifest.

Report the delivered user outcome, `contracts/evidence.md` path, assessor verdict,
physical run ids under both logical workflows, remaining risks, and
`learning/agent-performance.md` path. A `blocked` or `cancelled` report must say so
plainly and must never use completion language.
