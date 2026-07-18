# Workflow Panel Agent 进度列（token 消耗 / 工具调用数）始终显示 0，且列未对齐

**状态**：Partial
**优先级**：中
**创建日期**：2026-07-18

## 问题描述

Workflow Panel 的 Agents 列表中，每个 agent 名称右侧有两列数值，但两列始终显示为 0，且没有列标题说明含义，用户无法区分哪列是 token 消耗、哪列是工具调用数（容易误认为"运行时间"）。同时，两列数值在视觉上没有对齐整齐。

## 症状详情

| 现象 | 当前行为 | 期望行为 |
|------|---------|---------|
| Agent 右侧 token 消耗列 | agent 运行期间始终显示 `0`；仅 `agent_done` 后短暂更新 | agent 运行期间周期性更新，展示实时累计 token |
| Agent 右侧工具调用数列 | 始终显示 `0`（结果中 `tool_count` 硬编码 `None`） | 展示实际工具调用次数 |
| 列标题 | Agents 区域只有 "Agents" 一行标题，无数值列的 sub-header | 应标注列含义（如 "Tokens" / "Tools"） |
| 列对齐 | 两列数值视觉上未整齐对齐 | 各列左对齐或右对齐一致 |

### 现象 1：token_count 始终为 0（agent 运行期间）

`agent_progress` 事件（`peri-workflow/src/protocol.rs:124`）负责在 agent 执行期间周期性更新 `token_count` 和 `tool_count`。但从 workflow agent 的执行路径（`peri-acp/src/agent/workflow_agent.rs`）来看，该 agents 从未主动发送 `agent_progress` 事件——它只是在 `run_react_loop` 完成后，从 `usage_stats` 获取累计 token 数并写入 `agent_done` 结果。

因此进度存储 `WorkflowProgressStore` 在整个 agent 运行期间读到的 `token_count` 为 `None`，TUI 渲染时 `unwrap_or(0)` 显示为 0。

### 现象 2：tool_count 始终为 0

`workflow_agent.rs:477,488` 两处构造 `AgentRunResult::Ok` 时，`tool_count` 字段硬编码为 `None`。`AgentDone` 事件处理时（`progress.rs:184-185`）采用 `.or(agent.tool_count)` 语义，由于 result 中无值且 `AgentProgress` 事件也未设置，tool_count 始终为 `None` → 0。

### 现象 3：列标题缺失

Agents 区域头部（`workflow.rs:314-316`）仅渲染 "Agents" 文本，没有标明后续数值列的含义。用户对两列 0 的认知只能靠猜测，容易将 `tool_count` 列误认为"运行时间"。

### 现象 4：列对齐问题

当前渲染格式（`workflow.rs:268-274`）：

```rust
format!("{name:18}")     // agent 名称，Rust char 填充（CJK 字符宽度 2）
format!(" {tokens:>8}")  // token 列，abbreviate_count 返回变长字符串
format!("  {tools:>4}")  // tool 列
```

- `abbreviate_count()` 返回变长字符串（"0" / "1k" / "1.2k" / "1.5M"），搭配 `:>8` 格式说明符在不同值长度下，视觉列宽不一致
- `{name:18}` 使用 Rust char 计数而非终端列宽，CJK 名称会导致后续列偏移
- 缺少列标题使得对齐基准不可见

## 涉及文件

| 文件 | 角色 |
|------|------|
| `peri-tui/src/kit/panels/workflow.rs` | Workflow Panel 渲染组件，Agents 列表的 UI 展示和数据格式化 |
| `peri-tui/src/kit/workflow_snapshot.rs` | TUI 侧 DTO 定义（`TuiAgentProgress`），仅含 `token_count`/`tool_count`，无 runtime |
| `peri-workflow/src/progress.rs` | 进度存储，`apply_event()` 处理 `AgentProgress`/`AgentDone` 事件更新 agent 状态 |
| `peri-acp/src/agent/workflow_agent.rs` | workflow agent 执行入口，结果中 `tool_count` 硬编码 `None`，无运行时 `agent_progress` 发送 |
| `peri-workflow/src/protocol.rs` | `AgentRunResult` 和 `ProgressEvent::AgentProgress` 定义，含 `tool_count`/`token_count` |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-18 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
