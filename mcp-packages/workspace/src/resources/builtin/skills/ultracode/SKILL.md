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

An explicit user request for this skill or a workflow takes precedence over the convenience guidelines below. Use the requested workflow even when direct execution would be faster; keep its scope small. If the required tool is unavailable, explain the blocker and obtain the user's choice before replacing the requested execution method.

When the user has not specified a workflow, prefer direct execution for:
- Simple single-agent tasks (just do the work directly)
- Tasks requiring tight sequential conversation (use normal tool calls)
- Tasks that are faster to do inline than to script

## Capability preflight (before you write a script)

Match the brief against what each target can actually do **before** you dispatch anything. There is no capability query API and no tool negotiation: nothing to call that reports a target's tools, and `allowedTools` only narrows. Resolve it from what exists:

1. The `available_agents` line in your system prompt (`- <id> [<tier>] [writes|readonly]`) — a scheduling hint, not a tool list.
2. The agent definition resource, read with `mcp_read_resource`（未暴露时先用工具搜索发现；底层 MCP 方法是 `resources/read`）: `agent://builtin/plan/agent.md`. Its frontmatter `tools` (omitted = inherit the base tool set, `[]` = zero tools, a list or `*` = allowlist) and `disallowedTools` (always removed) decide the target's tools. `agentType` selects that definition; a non-empty `allowedTools` then intersects with it and can never add a tool back.
3. The engine's script API — `agent()` takes `agentType`, `allowedTools`, `label`, `model`, `schema`, `maxTokens`, `isolation` and `phase`; none of them reports what a target can actually do.

Write the brief's required tools next to the resolved target tools for every stage. If a required tool is not in the result, the stage cannot produce its evidence — fix the target or split the brief before launching; do not launch and hope.

| target | runs commands | why |
| --- | --- | --- |
| `coder` | yes | 未声明 `tools`，继承基础工具；`disallowedTools` 只移除 `Agent` |
| `explorer` | no | 显式 `tools: [Read, Grep, Glob]`（只读）；`disallowedTools` 只移除 `Agent` |
| `general-purpose` | yes | `tools: "*"` |
| `plan` | yes | 未声明 `tools`，继承基础工具；`disallowedTools` 只移除 `Agent` |
| `verification` | yes | 未声明 `tools`，继承基础工具；`disallowedTools` 只移除 `Agent` |
| `web-researcher` | yes | 未声明 `tools`，继承基础工具；`disallowedTools` 只移除 `Agent` |

The table is a claim about the shipped definitions (`mcp-packages/workspace/src/resources/builtin/agents/*.md`, readable as `agent://<scope>/<id>/agent.md`) and is locked against them by `resources/builtin_skill_capability_test.rs`; change a definition and the table must change with it. 该表只描述定义是否允许 Bash，不保证当前父工具集真的提供 Bash；运行时还会移除不可继承工具。派发前须核对实际可用工具，不能把表格当作完整运行时权限证明。

workflow 的 `allowedTools: []` 与省略该字段一样，不收窄工具；这不同于 Agent 定义中的 `tools: []`（零工具）。需要限制 stage 时，显式列出非空工具白名单。必需工具不在有效集合中就是派发错误，例如要求运行命令却设置 `allowedTools: ['Read']`；引擎不会替派发方校验 brief 所需工具，必须在启动前发现缺口。

Fail fast with the first probe: an agent's first action must exercise one of the brief's required tools — one command for a command-running brief, one read for a read-only one — never a long exploration first. A probe that reports the tool missing ends that stage: report the gap (the selected target has no `Bash` after tool filtering), do not keep going, do not narrate the command in place of running it, and do not widen the definition's permissions to make the stage work.

```javascript
// ✅ 只读 brief 派给 explorer：所需工具 `Read`/`Grep`/`Glob` 都在，且任务约束仍要求只读
() => agent('列出派发路径的调用点', { label: 'recon', agentType: 'explorer', allowedTools: ['Read', 'Grep', 'Glob'] })

// ✅ 需要命令证据时派给当前继承 Bash 的 plan；brief 仍限定其只执行指定检查
() => agent('运行 cargo test 并报告结果', { agentType: 'plan', allowedTools: ['Bash'] })

// 负例：非空白名单排除了 Bash，不能满足需要运行命令的 brief
() => agent('运行 cargo test 并报告结果', { agentType: 'plan', allowedTools: ['Read'] })
```

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
# 必须带显式版本号：npx 在全局已有同名 bin 时会静默复用旧版（0.1.x 无 CLI 子命令，
# 所有命令无输出且 exit 0，无任何报错），`@<version>` 才能强制使用 registry 目标版本。
npx -y @peri-code/workflow@0.3.0 validate scripts/parallel-review.mjs
# Machine-readable output for tooling:
npx -y @peri-code/workflow@0.3.0 validate scripts/parallel-review.mjs --json
```

Use it when a workflow script is already saved and non-trivial. It checks engine syntax and exports, rejects old `workflow.agent(...)`-style calls, requires `export const meta = { name, description }`, and warns when the script has no top-level `return`.

Do not create a file solely to validate an inline script passed directly to the Workflow tool.

### 8. Inspect saved workflow results with the CLI

After a run, the runner persists `state.json`, agent journal entries, and extracted long outputs under `.claude/workflow-runs/<run_id>/`. Long string fields of the result object (over 200 bytes) are **not** stored inline: `state.json` (and any raw dump of it, e.g. `list --json`) keeps a `${label}` token where that field's path used to be, and the text lives in `outputs/` under the same label with a `.txt` suffix — a `${...}` token is an externalized value, never a missing or unevaluated one. The same threshold applies only to object fields: a top-level array or bare string return is stored inline as-is. The Read tool remains suitable for a known result file or a referenced `outputs/*.txt`; use the built-in CLI when you need a complete report (it restores the extracted text inline) or need to discover runs:

```bash
npx -y @peri-code/workflow@0.3.0 list
npx -y @peri-code/workflow@0.3.0 list --json
npx -y @peri-code/workflow@0.3.0 read <run_id>
npx -y @peri-code/workflow@0.3.0 read <run_id> --short --json
```

`read` and `list` search upward from the current directory for `.claude/workflow-runs/`. `read` restores extracted long outputs in its report; `list` reports the stored state, so its output still shows the `${label}` references.

### 9. The workflow runs asynchronously

The Workflow tool returns immediately with a run_id. You'll receive a notification when it completes. The results are saved to `.claude/workflow-runs/<run_id>/state.json`.

Long strings are externalized as described above: the start message, the completion notification and `state.json` all mark them with a `${label}` reference whose content lives in `outputs/<label>.txt`. Read the referenced file instead of reporting the token as the result.

Use the Read tool or the `read` CLI command to examine the results after completion.

### 10. Monitor progress

Use `/workflows` to open the workflow panel and see real-time progress (phases, agents, token counts).

## Prerequisites

The Peri host runs a bundled, digest-verified `@peri-code/workflow` engine from the copy shipped next to the peri executable（首次使用会发布到本机缓存 `~/.peri/workflow/<version>/<内容身份>/`，之后离线复用）. No global `@peri-code/workflow` installation is required, and there is no silent network fallback: missing or digest-mismatched artifacts fail closed（显式 `PERI_WORKFLOW_ALLOW_NPX_FALLBACK=1` 才允许一次仍需同一摘要校验的 registry 获取）。The Workflow tool requires **Node.js** (the engine itself runs on `node`); if it is unavailable, it returns an error:

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
