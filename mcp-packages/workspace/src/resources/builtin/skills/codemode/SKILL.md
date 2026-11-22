---
name: codemode
description: >
  Orchestrate deterministic MCP tool calls from one workflow script with `tool()`.
  Use when the task is a fixed pipeline of tool calls whose control flow can be
  written as JavaScript and must be reproducible on resume. Use ultracode instead
  when stages need independent LLM agents or judgment between steps.
userInvocable: true
argumentHint: "[tool pipeline description or /codemode]"
---

# Codemode: Deterministic Tool Pipelines

Use the **Workflow** tool through `ToolSearch` and `ToolInvoke`. The run executes a
JavaScript script; `tool(name, args)` calls a real MCP tool through the host, and
`agent(prompt)` delegates judgment to an LLM agent. Prefer `tool()` for work a
deterministic program can express: reads, greps, shell commands, web fetches, and
custom MCP tools. Prefer `agent()` when a step needs interpretation or decisions.

Do not use codemode for a single tool call that you can run directly, and do not
replace a required multi-agent workflow (`ultracode`) with a tool-only script.

## Script shape

```js
export const meta = { name: 'audit', description: 'Collect and compare files' }

// `Glob` returns text: one path per line, `No files found.`, or a listing
// followed by an unnumbered `[Output truncated: ...]` note.
const listing = await tool('Glob', { pattern: 'src/**/*.rs' })
if (listing.trim() === 'No files found.') return { count: 0, verdict: 'no files matched' }
if (listing.split('\n').some(line => line.startsWith('[Output truncated:'))) {
  throw new Error('Glob hit the result limit; rerun with a narrower pattern')
}

const targets = listing.split('\n').filter(Boolean)
const results = await parallel(targets.map(file => async () => {
  try {
    const text = await tool('Read', { file_path: file })
    const lines = text.split('\n')
    // `Read` content lines are numbered; status lines are not. Match only the
    // latter so a file containing the same literal is still counted.
    if (lines[0] === '[EMPTY FILE]') return { file, lines: 0 }
    if (lines.some(line => line.startsWith('[Output truncated:'))) return { file, truncated: true }
    return { file, lines: lines.length }
  } catch (error) {
    // Keep the typed failure code instead of letting parallel() drop it.
    if (!error.code) throw error
    return { file, error: error.code }
  }
}))

const counts = results.filter(result => result && !result.error)
const unread = results
  .map((result, index) =>
    result && !result.error ? null : { file: targets[index], error: result?.error ?? 'unexpected_error' },
  )
  .filter(Boolean)
const verdict = await agent(
  `Rank the largest files and flag anomalies. ` +
    `Line counts: ${JSON.stringify(counts)}. Unread files: ${JSON.stringify(unread)}`,
  { label: 'rank' },
)
return { verdict, counts, unread }
```

Available hooks: `agent`, `parallel`, `pipeline`, `phase`, `log`, `workflow`, plus
`tool`. A run is always a background workflow run: report the `run_id`, wait for the
completion reminder, and read `state.json` or `outputs/` for full results.

## Calling tools

```js
const result = await tool('Read', { file_path: '/abs/path' })
```

- Pass an **effective tool name**: the same name the current model sees (for builtin
  tools, the original name such as `Read`; for external servers, the exposed name).
  Deferred tools must first be discovered with `ToolSearch`; the host resolves names
  against the same session-visible catalog.
- Host-only meta tools (`Agent`, `AskUserQuestion`, `TodoWrite`, `SkillTool`,
  `ToolSearch`, `ToolInvoke`) and the workflow's own tools are not callable from a
  script. Expect `tool_not_found` or `tool_unavailable`.
- `arguments` must be a plain object or omitted. Schema validation belongs to the MCP
  server; do not restate or pre-validate a schema in the script.
- The return value is `structuredContent` when present, otherwise the text content.
  Results are **not truncated**. Text-only tools keep their formatted text: `Glob`
  returns paths joined by newlines and `Grep` returns matching lines, so parse the
  string (for example `listing.split('\n').filter(Boolean)`) before iterating it.
- Failures throw a typed `WorkflowToolError` with `code`, `tool`, `message`, and
  optional `details`. Catch it when a failure is expected:

```js
try {
  await tool('Bash', { command: 'test -f marker', description: 'probe marker' })
} catch (error) {
  if (error.code !== 'tool_error') throw error
  log('marker missing; continuing')
}
```

Codes: `tool_not_found`, `invalid_arguments`, `tool_unavailable`, `policy_denied`,
`approval_unavailable`, `not_executed`, `tool_error`, `protocol_error`,
`task_accepted`, `outcome_unknown`, `cancelled`.

## Approvals, limits, and safety

- Every call passes the host's existing permission and approval surface. A background
  run that cannot obtain an approval waits until the approval times out and then fails
  closed with `approval_unavailable` or `policy_denied`; the tool is not executed.
- `tool()` calls do not consume `maxConcurrency` slots, `maxAgents`, `maxToolCalls`, or
  token budgets. They run with no script-level concurrency cap.
- The script is **not a security boundary**: it can use Node built-ins such as
  `process` and `require`. `tool()` routes MCP calls through host approval; it does not
  sandbox the script or grant new permissions.
- `task_accepted` means the target accepted asynchronous work and v1 does not poll it.
  Treat the side effect as unconfirmed instead of assuming it did not happen.

## Resume and observability

- Tool results are recorded per run and replayed on resume. Replay stops at the first
  mismatch (sequence, tool name, or arguments); later calls run live.
- `unknownSideEffects: true` in the run state means at least one tool call had no
  settled result (interrupted, accepted, or outcome unknown). Verify external effects
  before trusting the run; a resumed run may retry an unsettled call.
- Long strings in the `return` value (including a top-level string or array items) are
  externalized to `outputs/`; a `${label}` token in `state.json` is a reference, not the
  literal result.

## Checklist

- The pipeline is expressible as deterministic control flow, not agent judgment.
- Every tool name is an effective name visible to this session; deferred ones were
  discovered first.
- Expected failures are caught and classified, not swallowed.
- The final `return` contains the evidence needed for the handoff; large values are
  allowed and will be externalized.
- The report states the `run_id` and whether the run reported `unknownSideEffects`.
