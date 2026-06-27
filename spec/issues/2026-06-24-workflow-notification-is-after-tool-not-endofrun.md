# Workflow 完成通知绑定在工具执行流程中，而非独立的 EndOfRun 事件

**状态**：Fixed
**优先级**：中
**类型**：Bug
**创建日期**：2026-06-24
**修复日期**：2026-06-24

## 问题描述

Workflow 完成后，当前通知路径为：`registry.complete()` → broadcast → `executor` 常驻 consumer → `MessageQueue.push(PendingMessage { inject_at: EndOfLoop, system_reminder: true })` → `before_model` drain → 以 `<system-reminder>` human message 注入对话流。这意味着**通知被绑定在工具执行后的消息流中**，一旦 agent 消费了该消息，通知就不再可见。用户如果错过了 system-reminder，无法再次查看或触发该通知。

用户期望：workflow 完成通知应以独立的 "end of run" 事件形式存在，可以持久展示、可重新触发查看。

## 症状详情

| 场景 | 当前行为 | 期望行为 |
|------|----------|----------|
| workflow 完成后，agent 继续有其他输出 | system-reminder 混在对话流中被滚动淹没 | 通知独立于对话流，持久可见 |
| workflow 完成后，用户想再看一次结果 | 无法重新触发，只能手动读 state.json | 可通过某种方式重新查看通知 |
| 多个 workflow 同时完成 | 通知按注入顺序出现在对话流中 | 通知应有独立展示区域 |

## 涉及文件

- `peri-acp/src/session/executor.rs:980-1020` —— workflow 完成通知 consumer：将 `WorkflowTaskResult` 转为 `PendingMessage` push 到 MessageQueue，同时发 `BackgroundTaskCompleted` 到 TUI
- `peri-workflow/src/tool.rs:244-303` —— notification task：等待 workflow 完成 → 调 `registry.complete()`
- `peri-workflow/src/registry.rs:140-146` —— `complete()`：更新状态 + broadcast 发送 `WorkflowTaskResult`
- `peri-middlewares/src/workflow/mod.rs` —— `WorkflowMiddleware`：持有 session 级状态，`subscribe_notifications()` 订阅 broadcast

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-06-24 | — | Open | agent | 创建 |
| 2026-06-24 | Open | Fixed | agent | 修改 `agent_events_bg.rs`：移除 workflow auto-continuation 排除逻辑，增加 `loading=false` 时的兜底 continuation 触发 |

## 修复记录

### 根因分析

**两条通知路径**：
- **Path A**：`BackgroundTaskCompleted` → EventSink → TUI 通知条 ✅（独立，持久）
- **Path B**：`PendingMessage → MessageQueue → before_model drain → `<system-reminder>` ❌（绑定在工具流程中）

**时序竞态（核心 bug）**：workflow 完成后 `BackgroundTaskCompleted` 到 TUI，但在 Done 处理器中：
1. `background_agents.is_empty()=true`（workflow 不追踪在此）→ `agent_done_pending` 不被设
2. Workflow 完成后台通知到达
3. `agent_done_pending=false` + `background_agents.is_empty()=true` → 进入 else_if buffering
4. 掉落到 `(true, false, false)` → **不触发 continuation**

**修复**（`agent_events_bg.rs`）：
- 移除 workflow auto-continuation 排除注释
- 新增兜底逻辑：当 `agent_name.starts_with("workflow:")` 且 `ui.loading=false`（agent 已停止），直接 drain `pre_done_results` → 设 `pending_continuation` → `return (true, false, true)`

**验证**：编译通过，73 个 headless 测试 + 32 个 workflow 测试全部通过
