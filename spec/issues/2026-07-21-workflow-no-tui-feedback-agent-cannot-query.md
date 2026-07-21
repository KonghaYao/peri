# Workflow 运行时 TUI 无任何反馈且 Agent 无法感知状态

**状态**：已修复 | **优先级**：高 | **分类**：Bug / 显示异常 | **日期**：2026-07-21

## 问题描述

Workflow 工具被 Agent 调用（或手动 `/ultracode`）后，workflow 实际正常启动并执行完成（产生结果），但 TUI 界面上完全没有任何反馈——状态栏不显示 workflow 运行计数、无顶部通知弹出、Workflow Panel（Ctrl+W）面板为空。同时 Agent 侧也无法感知 workflow 正在运行，无法查询其进度或结果。这是一个近期引入的回归问题。

## 症状详情

| 缺失项 | 预期行为 | 实际行为 |
|--------|---------|---------|
| 状态栏任务计数 | 显示 `1 workflow` 等运行中任务数 | 无任何 workflow 计数 |
| 顶部通知 | workflow 启动/完成时弹出通知消息 | 无任何通知 |
| Workflow Panel | Ctrl+W 打开看板可见当前 run 的 phase/agent 状态 | 面板为空 |
| Agent 感知 | Agent 在后续 turn 能通过 system-reminder 收到 workflow 完成通知 | Agent 不知道有 workflow 在跑 |
| Agent 查询 | （设计上 Agent 通过 Workflow 工具返回值获取 run_id 后应可查询） | 无法查询进度或结果 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 在 TUI 对话中让 Agent 触发 Workflow 工具（派发一个简单的 workflow）
  2. 观察：Workflow 实际在后台启动并完成（`.claude/workflow-runs/` 下有结果文件）
  3. 观察：从步骤 1 到 workflow 完成，状态栏始终无变化，无通知弹出
  4. 打开 Workflow Panel（Ctrl+W）——面板为空
- **触发方式**：Agent 调用 Workflow 工具、手动 `/ultracode` 命令均复现
- **环境**：macOS 26.5.1，近期版本引入的回归（之前正常）

## 涉及文件

| 文件 | 角色 |
|------|------|
| `peri-tui/src/kit/workflow_snapshot.rs` | Workflow 状态轮询机制 |
| `peri-tui/src/kit/acp_events.rs` | WorkflowProgress 事件 → TUI 状态转换 |
| `peri-tui/src/kit/acp_notifier.rs` | ACP 通知路由分发 |
| `peri-tui/src/kit/panels/workflow.rs` | Workflow Panel 看板渲染 |
| `peri-middlewares/src/workflow/mod.rs` | Workflow 中间件（事件推送） |
| `peri-acp/src/session/async_router.rs` | 异步事件路由 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-21 | — | Open | agent | 创建 |
| 2026-07-21 | Open | 已修复 | agent | 修复 event_sink PeriCaps fallback |

## 修复记录

**根因**: `peri-acp/src/session/event_sink.rs` 中 `TransportEventSink` 的三处 `caps_registry.get(sid).unwrap_or_default()` 在 session 未注册时返回 `PeriCaps::default()`（全 false），导致 `push_unstable_event` 等方法的 PeriCaps 检查静默丢弃所有 bg-task-started/completed/WorkflowProgress 等事件。状态栏、通知、面板均无法获得运行时更新。

**修复**: 将 `unwrap_or_default()` 改为 `unwrap_or_else(|| PeriCaps::all_enabled())`，当 session 不在 caps_registry 中时 fallback 到全启用模式，同时记录 `tracing::error!` 留痕。

**修改文件**: `peri-acp/src/session/event_sink.rs` — 3 处 `unwrap_or_default` → fallback `all_enabled()`

**验证**: e2e `tests/workflow/workflow-run.test.ts` 通过——状态栏显示 `1 workflow`，面板显示 workflow 运行状态及完成结果。
