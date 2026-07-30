> 归档于 2026-07-30，原路径 spec/issues/2026-07-25-subagent-orphan-spans-dangling-parent-observation.md

# Subagent span 父链断裂——49 个工具调用和 34 个推理步骤指向不存在的 parent_observation_id

**状态**：Verified
**优先级**：中
**创建日期**：2026-07-25
**类型**：Bug

## 问题描述

在 Langfuse trace `019f986c558e7f038ac1b80a2d8ed03b` 上，主 agent 在调用 Agent 工具后，后续所有 stage-reason、stage-act、tool-batch 和 TOOL observation 的 `parentObservationId` 指向一个**不存在的 observation**，导致 49 个工具调用和 34 个 GENERATION 完全游离在 agent 层级之外。

**实际结构（错误）**：

```
agent-run (AGENT)                                ✅ 正常
├── stage-reason step-1~11 → GENERATION          ✅ 正常
├── stage-act step-11 → tool-batch
│   └── Agent (TOOL)                             ✅ 正常（主 agent 调用）
│
└── [49 TOOL + 34 GEN span]                      ❌ 所有 parentObservationId 指向不存在对象
    ↑ 应该挂在 agent-run 下但父链断裂
```

**期望结构**：

```
agent-run (AGENT)
├── stage-reason step-1~11
│   └── GENERATION
├── stage-act step-11 → tool-batch → Agent (TOOL)
├── stage-reason step-1~34                       ← 主 agent 继续推理
│   └── GENERATION
├── stage-act step-1~34 → tool-batch
│   ├── Read × 27
│   ├── Edit × 21
│   └── TodoWrite × 1
...
```

## 症状详情

### Trace 数据证据（019f986c558e7f038ac1b80a2d8ed03b）

**孤儿 observation 统计**：

| 类型 | 数量 | 时间范围 |
|------|------|---------|
| GENERATION (step-1~34) | 34 | 08:42:53 ~ 08:46:42 |
| SPAN (stage-reason) | 34 | 同上 |
| SPAN (stage-act) | 34 | 同上 |
| SPAN (tool-batch) | 1 | 08:42:56 |
| TOOL (Read) | 27 | 08:42:56 ~ 08:44:46 |
| TOOL (Edit) | 21 | 08:43:45 ~ 08:44:49 |
| TOOL (TodoWrite) | 1 | 08:42:56 |
| **合计** | **152** | |

**父链断裂点**：

所有孤儿 span 的父链在以下节点断裂：

```
gen(step-1) ──→ span(stage-reason) ──→ obs_019f98710b7a77728c01ef64693f4052  ← 不存在！
                                         ↑
batch(tool-batch) ← span(stage-act) ────┘
  ↓ (49 TOOL)
```

而真实存在的 Agent TOOL 调用 ID 为：

```
obs_019f98710b7a77728c01ef5c9205e141   ← 真实存在（Agent 工具调用，08:42:53）
```

两个 ID 的 ULID 前缀相同（`019f98710b7a77728c01ef`），但随机 suffix 不同——**span context 传播时 parent ID 计算错误**。

### 时间线交叉验证

| 时间 | 事件 | 归属 |
|------|------|------|
| 08:37:44 | 主 agent step-1~5 | agent-run ✅ |
| 08:39:04 | Agent TOOL → subagent-1 启动 | agent-run ✅ |
| 08:39:07~08:41:58 | subagent-1 内部 steps | subagent-1 ✅ |
| 08:41:58 | 主 agent step-6~11 | agent-run ✅ |
| 08:42:53 | Agent TOOL → **孤儿序列开始** | agent-run ❌ 断裂 |
| 08:42:53~08:46:42 | 主 agent step-1~34 + 49 TOOL | **孤儿** ❌ |
| 08:47:40 | 主 agent step-12 恢复 | agent-run ✅ |

## 根因分析

**与旧 issue `2026-07-25-subagent-tool-calls-wrong-parent-observation` 的关系**：
- 旧 issue 的 3 个 Fix (A/B/C) 解决了工具挂到**错误父节点**（主 agent 而非子 agent）的问题
- 本次问题是工具挂到**不存在的父节点**，是更深层的 span context 传播 bug

**推测根因**：`on_tool_start` 中为 Agent 工具创建 `ToolBatch` 时，`parent_observation_id` 使用了错误的 span ID。具体来说：
1. 主 agent 调用 Agent 工具时，`on_tool_start` 创建 tool observation
2. 该 observation 的 parent 应该指向当前 act span
3. 但后续步骤的 `stage-reason`/`stage-act` span 的 `parent_observation_id` 使用了该 Agent TOOL observation 的一个**不同/计算错误的 ID**
4. 导致整个后续链断裂

## 涉及文件

- `peri-acp/src/langfuse/tracer/mod.rs` — `on_tool_start`、`on_tool_end`、span context 创建
- `peri-acp/src/langfuse/tracer/tool_batch.rs` — `ToolBatch` 的 `parent_observation_id` 设置
- `peri-acp/src/langfuse/tracer/subagent.rs` — `SubagentStack` 的 span ID 上下文管理

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-25 | — | Open | agent | 创建——Langfuse trace 019f986c... 分析发现 152 个 orphan observation |
| 2026-07-25 | Open | Fixed | agent | 修复：bridge.rs StageStarted 补调 mark_top_started()，消除 fork subagent 的 ObservationCreate 时序窗口 |
| 2026-07-25 | Fixed | Verified | agent | 验证：317 单元测试 + 5 e2e 测试通过，review APPROVED |

## 修复记录

### 修复 #1（2026-07-25）

- **操作人**：agent（auto-devflow: explore → plan → code → review → verify）
- **用户原意**：修复 fork subagent 的 ObservationCreate 缺失导致 152 个 orphan observation 父链断裂
- **修复内容**：
  - **文件**：`peri-acp/src/langfuse/bridge.rs`，新增 4 行（含注释）
  - **修改**：在 `UnifiedLangfuseEvent::StageStarted` handler 中，`stages.on_stage_start()` 调用之前，新增 `t2.subagent.mark_top_started()` 调用
  - **原理**：bridge 路径绕过了 `tracer.on_stage_start()`，缺少 `mark_top_started()` 调用。`LlmCallStart` 虽间接触发此调用但存在时序窗口——如果 subagent 首事件是 `StageStarted`（如 Receive 阶段先于 Reason），subagent 快速完成后 `on_tool_end("Agent")` 到达时 `top_has_started()` 仍为 false → fork cleanup 不执行 → ObservationCreate 不 emit → 所有子 span 成为孤儿
  - **幂等性**：`mark_top_started()` 仅设置 bool flag，多次调用无副作用；bg subagent 不受影响（走 else 分支）
- **涉及文件**：1 个（`bridge.rs`），+4 行
- **验证状态**：已验证（build ✅ / 317 unit tests ✅ / 5 e2e tests ✅ / code review APPROVED ✅）
- **验证反馈**：所有历史修复 (Fix A/B/C) 无回归；`test_e2e_fork_subagent_emits_observation_create` 直接覆盖本修复
