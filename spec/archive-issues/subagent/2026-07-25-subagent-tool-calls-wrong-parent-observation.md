# Subagent 工具调用挂载到错误的父 Observation——主 agent 层级而非子 agent 层级

**状态**：Archived
**优先级**：中
**创建日期**：2026-07-25
**类型**：Bug

## 问题描述

在 Langfuse trace 上，子 agent (subagent) 的内部工具调用（Grep、Glob、Read、SandboxWrite 等）被挂载到主 agent 的 `tool-batch` span 下，而非子 agent 自己的 `stage-act` span 下。

**实际结构（错误）**：

```
agent-run (AGENT)
├── subagent-xxx (AGENT)
│   ├── stage-reason step-1 (SPAN) → GENERATION ✅
│   ├── stage-act step-1 (SPAN)     → 0 children  ❌ 空
│   ├── stage-reason step-2 (SPAN) → GENERATION ✅
│   ├── stage-act step-2 (SPAN)     → 0 children  ❌ 空
│   ...
│
└── stage-act (SPAN) — 主 agent
    └── tool-batch (SPAN)
        ├── Agent (TOOL)         ← 主 agent 的调用，正确
        ├── Grep × 2 (TOOL)      ← 实际是子 agent 的工具，错误
        ├── Glob × 3 (TOOL)      ← 实际是子 agent 的工具，错误
        ├── Read × 15 (TOOL)     ← 实际是子 agent 的工具，错误
        └── SandboxWrite (TOOL)  ← 实际是子 agent 的工具，错误
```

**期望结构**：

```
agent-run (AGENT)
├── stage-act (SPAN) — 主 agent
│   └── tool-batch
│       └── Agent (TOOL)     ← 仅此一个
│
└── subagent-xxx (AGENT)
    ├── stage-reason step-1 → GENERATION
    ├── stage-act step-1 → tool-batch
    │   ├── Grep × 2
    │   └── Glob × 3
    ├── stage-reason step-2 → GENERATION
    ├── stage-act step-2 → tool-batch
    │   └── Read × 9
    ...
```

## 症状详情

### Trace 数据证据

验证 trace：`019f92db9f917311ae24a7ca7b8a1ad1`（2026-07-24）

**观察树分析**：
- 主 agent 的 `tool-batch` (batch_019f92dbb7d5...) 包含 22 个工具，包括 Agent + 21 个其他工具
- 子 agent (obs_019f92dbb7d57811b591f1495c) 有 5 个 `stage-act` span，但全部 0 子节点
- 子 agent 有 5 个 `stage-reason` span，各有 1 个 GENERATION 子节点（正常）

**时间线交叉验证**：

| 子 agent step | stage-act 时间 | 主 agent batch 中的工具（应在子 agent 下） |
|---|---|---|
| step-1 | 43.909Z-44.034Z | Grep×2, Glob×3 @ 43.909Z |
| step-2 | 48.934Z-48.956Z | Read×9 @ 48.934Z |
| step-3 | 52.501Z-52.516Z | Read×4 @ 52.501Z |
| step-4 | 57.077Z-57.089Z | Read×2 @ 57.077Z |
| step-5 | 19.001Z-19.023Z | SandboxWrite @ 19.001Z |

## 根因分析

在 `peri-acp/src/langfuse/tracer/mod.rs` 的 `on_tool_start` 中：

1. **`parent_id` 被提前捕获**：在 `begin_subagent()` 之前，`parent_id` 已设为主 agent 的 `stage-act` span ID
2. **惰性 ToolBatch 创建**：`Agent()` 工具调用触发了新 `ToolBatch`，其 `parent_observation_id` 被固定为主 agent 的 act span ID（`tool_batch.rs:73-86`）
3. **后续工具复用相同批次**：子 agent 执行的所有工具被加入同一 `ToolBatch`，`parent_observation_id` 不变
4. **子 agent 的 stage-act 跨度被孤立**：它们正确创建了，但没有工具子节点

## 涉及文件

- `peri-acp/src/langfuse/tracer/mod.rs` — `on_tool_start` 方法（parent_id 捕获时机）、`emit_tools_flush`
- `peri-acp/src/langfuse/tracer/tool_batch.rs` — `ToolBatch.on_tool_start`（惰性创建 + parent_observation_id 固定）
- `peri-acp/src/langfuse/tracer/subagent.rs` — `SubagentStack.begin_subagent` / `current_tool_batch_mut`

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-25 | — | Open | agent | 创建——Langfuse trace 019f92db... 分析发现子 agent 工具调用挂载错误 |
| 2026-07-25 | Open | Fixed | agent | 修复：3 个 Fix (A/B/C) 在 mod.rs，+188/-26 行，317 tests pass |
| 2026-07-25 | — | Archived | agent | 归档

## 修复记录

### 修复 #1（2026-07-25）

- **操作人**：agent（auto-devflow: explore → plan → plan-review → code → review → verify）
- **用户原意**：子 agent 工具调用应挂载在子 agent 自己的 stage-act span 下
- **修复内容**：
  - **Fix A** (`on_tool_start`)：Agent/Task 工具先写入主 agent ToolBatch 再 `begin_subagent`，使子 agent 工具走 else 分支创建独立 batch，parent = 子 agent act span
  - **Fix B** (`on_tool_end`)：Agent 工具结束显式路由到主 batch（`self.tool_batch`），避免 `current_tool_batch_mut` 返回 Sub 导致工具记录丢失
  - **Fix C** (`on_stage_end`)：Act 阶段结束时若 `!subagent.is_empty()` 则 flush 子 batch，否则 flush 主 batch
  - 测试更新：3 处断言修正 + 2 个新测试（验证 parent_observation_id + on_stage_end 路由）
- **涉及文件**：2 个（`mod.rs` + `tracer_test.rs`），+188/-26 行
- **验证状态**：已验证（build ✅ / 329 tests pass 含 e2e ✅ / code review APPROVED ✅）
- **已知限制**：嵌套子 agent (agent spawns agent) 不支持——Fix A 总是写入主 batch；bg 子 agent parent_id 时序局限（见 plan review）
