---
name: ultracode
description: >
  Multi-agent workflow orchestration. Use when the user says "ultracode", "workflow",
  "parallel agents", "pipeline", or asks to orchestrate multiple agents for complex tasks.
  Also use when the user wants to break work into phases or parallel streams.
userInvocable: true
argumentHint: "[task description or /ultracode]"
---

# Ultracode: Multi-Agent Workflow Orchestration

You have access to the **Workflow** tool (via `SearchExtraTools` → `ExecuteExtraTool`), which lets you orchestrate multiple agents working in parallel or pipeline phases.

## When to Use

Use the Workflow tool when:
- The task can be decomposed into independent parallel subtasks
- The task benefits from a pipeline (phased execution where later phases depend on earlier results)
- You need to explore multiple approaches simultaneously
- The user explicitly asks for "ultracode", "workflow", or parallel execution

Do NOT use workflows for:
- Simple single-agent tasks (just do the work directly)
- Tasks requiring tight sequential conversation (use normal tool calls)
- Tasks that are faster to do inline than to script

## How to Use

### 1. Discover the Workflow tool

The Workflow tool is a deferred tool. First search for it:

```
SearchExtraTools("workflow")
```

Then execute it:

```
ExecuteExtraTool("Workflow", {
  "script": "...your workflow script...",
  "args": {},
  "maxConcurrency": 3
})
```

### 2. Write a workflow script

Workflow scripts are JavaScript ESM modules using these primitives:

- **`agent(prompt, options?)`** — Run a single agent. Returns the agent's output.
- **`parallel([...factories])`** — Run multiple agents concurrently. **入参为返回 promise 的零参工厂函数（thunks），不是 promise。** Returns array of results.
- **`pipeline(items, ...stages)`** — 数据流水线：对 items 每个元素顺序执行所有 stage，`stage=(prev, item, index) => result`，stage 的返回值为下一个 stage 的 prev。返回与 items 等长的数组，出错位置为 null。
- **`phase(title)`** — 切换当前阶段标记（发 phase_started/done 事件），后续 agent 自动带该阶段名。**不接 fn，返回 undefined。**
- **`log(message)`** — Emit a log message visible in the workflow panel.
- **`workflow(nameOrScriptPath, args?)`** — 运行子 workflow（一层嵌套上限）。`workflow('sub-name', args)` 按名字查找，`workflow({ scriptPath: '...' }, args)` 按文件路径加载。

### 3. Example: Parallel Code Review

```javascript
export const meta = {
  name: 'parallel-review',
  description: 'Review code from multiple perspectives in parallel'
}

const [security, perf, bugs] = await parallel([
  () => agent('Review this code for security vulnerabilities. Focus on injection, auth, and data exposure.', { label: 'security', allowedTools: ['Read', 'Grep'] }),
  () => agent('Review this code for performance issues. Focus on N+1 queries, unnecessary allocations, and hot paths.', { label: 'performance', allowedTools: ['Read', 'Grep'] }),
  () => agent('Review this code for bugs. Focus on edge cases, error handling, and logic errors.', { label: 'bugs', allowedTools: ['Read', 'Grep'] }),
])

return { security, perf, bugs }
```

> ⚠️ **`parallel` 入参必须是工厂函数 `() => agent(...)`，不能直接写 `agent(...)`。**
> 直接传 `agent(...)` 调用（已是 Promise、不可调用）会被 runtime 静默吞掉：`parallel` 把每个元素当函数调用，抛 `TypeError` 后被内部 catch 返回 `null`，workflow 以「假成功 completed + 全 null 返回值」结束、且不写 `journal.jsonl`。务必写成 `() => agent(...)`。

### 4. Example: Pipeline Data Processing

```javascript
// pipeline(items, ...stages): 对每个数据项顺序跑 stage
const files = ['src/a.rs', 'src/b.rs']

const results = await pipeline(files,
  // stage1: 审查每个文件
  (file) => agent(`审查文件 ${file} 的代码质量`, { label: `review:${file}`, model: 'haiku' }),
  // stage2 (可选): 对 stage1 输出做二次处理，prev=上阶段结果
  (prev) => agent(`总结审查结果: ${prev}`, { label: 'summarize', model: 'haiku' }),
)

return results  // [{stage2_result_a}, {stage2_result_b}]
```

### 5. Example: Phase Marking

```javascript
// phase(title): 切换阶段标记，后续 agent 自动带该阶段名
phase('Review')
const r1 = await agent('审查代码', { label: 'reviewer' })

phase('Fix')
const r2 = await agent('修复问题', { label: 'fixer' })

return { r1, r2 }
```

### 6. Example: Sub-Workflow Invocation

```javascript
// workflow(nameOrScriptPath, args?): 运行子 workflow（一层嵌套上限）
const subResult = await workflow(
  { scriptPath: 'scripts/sub-check.mjs' },
  { threshold: 0.8 },
)
return subResult  // 子 workflow 的 return 值
```

### 7. Validate a saved script before running it

`@peri-code/workflow` provides a CLI preflight check for an existing workflow file:

```bash
npx -y @peri-code/workflow validate scripts/parallel-review.mjs
# Machine-readable output for tooling:
npx -y @peri-code/workflow validate scripts/parallel-review.mjs --json
```

Use it when a workflow script is already saved and non-trivial. It checks engine syntax and exports, rejects old `workflow.agent(...)`-style calls, requires `export const meta = { name, description }`, and warns when the script has no top-level `return`.

Do not create a file solely to validate an inline script passed directly to the Workflow tool.

### 8. Inspect saved workflow results with the CLI

After a run, the runner persists `state.json`, agent journal entries, and extracted long outputs under `.claude/workflow-runs/<run_id>/`. The Read tool remains suitable for a known result file; use the built-in CLI when you need a complete report or need to discover runs:

```bash
npx -y @peri-code/workflow list
npx -y @peri-code/workflow list --json
npx -y @peri-code/workflow read <run_id>
npx -y @peri-code/workflow read <run_id> --short --json
```

`read` and `list` search upward from the current directory for `.claude/workflow-runs/`. `read` restores extracted long outputs in its report.

### 9. The workflow runs asynchronously

The Workflow tool returns immediately with a run_id. You'll receive a notification when it completes. The results are saved to `.claude/workflow-runs/<run_id>/state.json`.

Use the Read tool or the `read` CLI command to examine the results after completion.

### 10. Monitor progress

Use `/workflows` to open the workflow panel and see real-time progress (phases, agents, token counts).

## Prerequisites

The Peri host prefers its locally installed `@peri-code/workflow` Node bundle and automatically falls back to `npx -y @peri-code/workflow` when that bundle is unavailable. No global `@peri-code/workflow` installation is required. The Workflow tool requires **Node.js** (provides `npx`); if npx is unavailable, it returns an error:

> npx is not available. Install Node.js (https://nodejs.org/) to enable workflow support.

In that case, tell the user to install Node.js and retry.

## Best Practices

- Keep agent prompts focused and specific
- Use `allowedTools` to restrict each agent's capabilities
- Use `label` to identify agents in the progress panel
- Use `phase()` to organize work into named stages
- Set `maxConcurrency` appropriately (default 3)
- Cache results with journal/resume for expensive operations

## Script Constraints

workflow 脚本运行在确定性沙箱中（保证 journal resume 一致性），以下运行时能力受限：

- **禁止 `Date.now()` / `new Date()`**：时间戳不得在脚本内自行获取，需经 `args` 由调用方注入。使用会触发 `failed: "Date.now()/new Date() is not available in workflow scripts"`。
- **`meta` 必须同时含 `name` 和 `description`**：`export const meta` 缺少任一字段时 workflow 立即 `failed: "meta must include string name and description"`。
- 其他受限制的全局对象以 engine 运行时为准（如 `Math.random` 等非确定性 API 可能也受限，待补充完整清单）。
