---
name: ultracode
description: >
  Execute one deliverable through a multi-agent workflow with explicit dependencies,
  parallel investigations, bounded repair, and a complete result handoff. Use when
  requested by the user or selected by multitask for dependent stages within one task.
userInvocable: true
argumentHint: "[task description or /ultracode]"
---

# Ultracode: Multi-Agent Workflow Orchestration

Use the **Workflow** tool through `ToolSearch` and `ToolInvoke`.
Main owns the business task and its current contract; the run owns its stage graph.
When multitask is active, a single-role task uses one fresh Agent and a task with
ordered multi-agent stages uses one Workflow. Do not nest a coordinator agent or
manually dispatch stages that the run already owns.

An explicit workflow request takes precedence even for a small task. If the tool is
unavailable, explain the concrete blocker before substituting another requested method.
Without that requirement, a simple task does not need a workflow. Do not add planning,
verification, or a summarizer just because those roles exist: use the task's actual
risk, unresolved design, and explicit acceptance requirements.

## Capability preflight

Read each target definition, including mandatory instructions and tool policy, before
writing a script. Use the available resource reader, such as `mcp_read_resource`, for
`agent://builtin/plan/agent.md`; discover the reader if it is not exposed. There is no
capability-query API. `available_agents` is a scheduling hint, not a tool list.

Omitted definition `tools` inherits; `tools: []` grants none; a list or `*` is an allowlist;
`disallowedTools` always removes tools. Workflow `allowedTools` can only narrow inherited
permissions. In the current workflow API, `allowedTools: []` means no additional narrowing,
unlike definition `tools: []`. Use a nonempty allowlist to restrict a stage.

| target | runs commands | why |
| --- | --- | --- |
| `coder` | yes | Inherits tools; excludes Agent |
| `explorer` | no | Allows Read, Grep, Glob, ReadContextArchive; excludes Agent |
| `general-purpose` | yes | Inherits tools through `*` |
| `plan` | yes | Inherits tools; excludes Agent; brief remains read-only |
| `verification` | yes | Inherits tools; excludes Agent; do not modify the implementation |
| `web-researcher` | yes | Inherits tools; excludes Agent |

This table describes shipped definitions, not the current parent's complete permissions.
It is checked against those definitions by `resources/builtin_skill_capability_test.rs`.
Runtime filtering can remove more tools. Record each brief's required tools against its
effective target. A command stage needs `Bash`; file inspection commonly needs `Read`
and `Grep`. Fail fast with one required-tool probe; a missing tool ends that stage.
Do not widen permissions or claim unrun checks succeeded.

Each assignment has one role and a fresh instance. Execution includes locating code,
local decisions, test code, and self-checks. Independent verification uses a fresh
verifier when risk or the user requires it. Planner and explorer are not implementers.
A pure investigation or acceptance-only request does not require implementation.

## Task, resource, and version boundaries

Keep independently deliverable business outcomes as separate tasks. The investigation,
implementation, combined acceptance, performance checks required for that implementation,
and its release notes normally remain stages of one deliverable. An independent benchmark
or training document may remain a separate task with explicit artifact dependencies.
Mechanical aggregation belongs in the script or Main, not an additional agent.

A shared worktree, index/HEAD, repository-wide baseline, mutable fixture, or port can
conflict even for different files. The `isolation` option does not currently prove that
a separate worktree/cwd was created. Serialize conflicting work unless actual isolation
is established. Keep independent read-only branches parallel when their inputs are stable.

A dependent write or acceptance stage waits for relevant writes to settle. Do not turn
an unsettled scope into a blanket pause: stable-input planning, contract preparation, and
independent tasks can continue. A retained preview needs a real owner-transfer receipt
and cleanup responsibility. A URL or a terminal agent message does not prove settlement.

Track the business task, goal revision, run_id, stage assignment, artifact version,
required evidence, resource owner, budget and stop conditions. Clarifications that do not
invalidate the current work update the contract without an automatic cancel/restart.
For a real goal change, cancel only affected execution and confirm it stopped before
fresh replacement. Old results cannot close the new goal. Repair new artifact versions
within budget while retaining valid upstream work; do not reopen finished agent threads.

On `scope-close-incomplete`, the host fences the entire affected Workflow run and
cancels its remaining assignments before allowing an engine retry to execute. It cannot
identify a smaller resource-conflict subgraph. Other runs and independent stable-input
work may continue. Do not restart the fenced run until its outstanding resources settle.

## Discover and launch

```
ToolSearch("workflow")
ToolInvoke({
  "name": "Workflow",
  "arguments": {
    "script": "...workflow script...",
    "args": {},
    "maxConcurrency": 3,
    "writeIntent": { "kind": "read_only" }
  }
})
```

Declare the actual `writeIntent`; a writing workflow must use the write form with the
real `repo_root`, `cwd`, and `path_allowlist`. Do not claim read_only to bypass the post-condition.
Inspect the exposed tool schema for current parameters and limits. A missing intent or
baseline can leave an otherwise completed run blocked. Limits are enforced by the host;
do not increase them silently after a failure.

The tool returns a run_id asynchronously. Bind it to the task immediately. A launch
acknowledgment is not completion. After a timeout or compaction, reconcile the current
run from available bindings/results; do not assume absence from a lookup means it never
started. Do not launch duplicate work or resume an old worker to query its status.

## Script API

Scripts use the engine's ESM-like format with `export const meta = { name, description }`
and a top-level `return`. Both metadata fields are required.

- `agent(prompt, options?)` runs a fresh assignment and returns its output. Options include
  agentType, allowedTools, label, model, schema, maxTokens, isolation, and phase.
- `parallel([...factories])` accepts zero-argument factories such as `() => agent(...)`,
  not already-started promises. Check every required branch before using its result.
- `pipeline(items, ...stages)` calls each stage as `(prev, item, index)` for each item;
  its return becomes the next stage's prev. Failed entries can be null.
- `phase(title)` marks subsequent agents; it does not take a callback and returns undefined.
- `log(message)` emits progress. Never log credentials.
- `workflow(nameOrScriptPath, args?)` invokes a child workflow, with a one-level nesting limit.
  Use it only for a reusable workflow boundary, not another coordinator around ordinary stages.

Required outputs that are null, missing, failed, blocked, or inconsistent with the current
artifact must not feed a dependent stage or be reported as success. The engine can return
null for dead/skipped agents and failed parallel branches; process exit/completed alone is
not business acceptance. Preserve a concrete blocker and available evidence.

### Example: execution with optional independent verification

```javascript
export const meta = {
  name: 'bounded-task',
  description: 'Implement one task and verify only when its acceptance requires it'
}

function requiredReport(value, stage) {
  if (value == null) throw new Error(`${stage} returned no usable result`)
  return typeof value === 'string' ? JSON.parse(value) : value
}

phase('Implementation')
const implementation = requiredReport(await agent(
  `${args.goal}\nReturn JSON with status, summary, and artifacts. Report actual checks and unresolved blockers.`,
  {
    agentType: 'coder', label: 'implementation',
    schema: { type: 'object', required: ['status', 'summary', 'artifacts'], properties: {
      status: { type: 'string' }, summary: { type: 'string' }, artifacts: { type: 'array' }
    } }
  }
), 'implementation')
if (implementation.status !== 'completed') throw new Error(implementation.summary)

let verification = null
if (args.requireVerification === true) {
  phase('Verification')
  verification = requiredReport(await agent(
    `Independently verify this task against its original acceptance: ${args.goal}\nImplementation evidence: ${JSON.stringify(implementation)}\nReturn JSON with status (passed/failed/partial), summary, and evidence. Do not modify files.`,
    {
      agentType: 'verification', label: 'verification',
      schema: { type: 'object', required: ['status', 'summary', 'evidence'], properties: {
        status: { type: 'string' }, summary: { type: 'string' }, evidence: { type: 'array' }
      } }
    }
  ), 'verification')
  if (verification.status !== 'passed') throw new Error(verification.summary)
}

return {
  taskId: args.taskId, goalRevision: args.goalRevision,
  summary: implementation.summary, artifacts: implementation.artifacts,
  verificationBasis: verification ? 'independent_verification' : 'execution_self_check',
  verification
}
```

The contract must provide the original acceptance and real artifact identities. The
example's control flow does not prove that an agent's reported checks are true. Do not
upgrade self-checks to independent verification or use old validation for new artifacts.
Use planning only when material design is unresolved; independent investigations can
use `parallel([() => agent(...), () => agent(...)])` before their actual dependents.

### Validate a saved script

For an existing nontrivial script, validate with the pinned CLI version:

```bash
npx -y @peri-code/workflow@0.3.0 validate scripts/task.mjs --json
```

The explicit version prevents reuse of an incompatible globally installed binary.
Do not create a file just to validate a small inline script. Avoid `Date.now()`,
`new Date()`, random values, and other nondeterministic globals in scripts; inject
needed external values through args. Follow the actual engine restrictions.

## Formal handoff and one completion summary

Workflow agents receive a host handoff instruction: put the final assignment output
in an entire assistant message enclosed in `<workflow-result>` and `</workflow-result>`.
The host selects the latest complete block, so later cleanup-only text does not replace
it. A corrected result needs a new complete block. With a schema, the block body must be
valid JSON without fences. Legacy unmarked output falls back to the last nonempty
assistant message. The marker identifies output; it does not certify acceptance.

The workflow script owns the final deliverable. Return a concise summary with actual
status, task/goal/artifact identities, evidence, verification basis, blockers and next
steps. Aggregate existing results without spawning an agent just to rephrase them.
Keep execution, business acceptance, resource settlement and external delivery separate.
If a required external delivery is only accepted/pending, verify its actual outcome;
retry a known retryable failure with the original message identity rather than rerunning work.

Completion notifications include a bounded reported-result preview (up to 8192 UTF-8
bytes before envelope escaping), with safe external references expanded when available.
Use a complete preview directly when it contains the necessary evidence. A partial
preview is not a complete result and may not be valid JSON. Read the saved aggregate
report for missing details, without restarting agents or polling unchanged state.
Engine acceptance `unknown` is expected unless an external acceptance layer supplies a
verdict; it does not itself require another verifier. Blocked delivery still needs its
specific reason even if execution completed.

Full results remain under `.claude/workflow-runs/<run_id>/`. Object string fields over
200 bytes are stored as `${label}` references to `outputs/<label>.txt`; top-level arrays
and bare strings are not externalized by that gate. References are stored values, not
missing template interpolation. Use a known file directly or retrieve an aggregate:

```bash
npx -y @peri-code/workflow@0.3.0 read <run_id> --short --json
npx -y @peri-code/workflow@0.3.0 list --json
```

`read` restores referenced text; `list` shows stored references. Both search upward for
`.claude/workflow-runs/`. `/workflows` displays progress. Do not use routine monitoring
as another model-driven stage handoff. Summarize each business task once; a new verifiable
error is a correction, not an invitation to free-form repeated completion claims.

## Runtime prerequisites

Peri uses the bundled, digest-verified `@peri-code/workflow` engine beside its executable,
materialized into `~/.peri/workflow/<version>/<content-identity>/` and reused offline.
Node.js is required. Missing or mismatched artifacts fail closed; there is no silent
network fallback. Only an explicit `PERI_WORKFLOW_ALLOW_NPX_FALLBACK=1` permits a registry
fetch with the same digest check. Report the actual missing prerequisite rather than
inventing successful execution or silently changing the requested method.
