# SubAgent 卡片完全不显示（SubagentStarted 事件被 notifier 丢弃）

**状态**：Open
**优先级**：高
**创建日期**：2026-07-07

## 问题描述

Agent 工具派发 SubAgent 后，消息区**完全不出现 SubAgent 卡片**（头行和子内容都没有）。用户只能看到父 Agent 的 Agent 工具调用（ToolCard），看不到 SubAgent 运行进度。规范要求 SubAgent 执行期间消息区应出现可折叠 SubAgentGroup 卡片，显示 Agent 内部工具调用和 final_result。

## 症状详情

### 实际表现（2026-07-07 用户确认）

| 步骤 | 期望 | 实际 |
|------|------|------|
| Agent 工具调用开始 | 父 Agent 工具调用 Card 出现 | ✅ 父 Agent 工具调用 Card 出现 |
| SubAgent 开始执行 | `❯ Agent(fork) <任务描述> · ⏳` 头行出现 | ❌ **没有任何 SubAgent 卡片** |
| SubAgent 内部工具调用 | ToolCard 嵌套在 SubAgentGroup 内 | ❌ 不显示 |
| SubAgent 完成 | `❯ Agent(fork) <任务描述> · ✅` + final_result | ❌ 不显示 |

### 现象 2：回归时间线（2026-07-07 用户反馈）

用户描述"以前有工具调用，修过后就没有了"。现确认回归不是 `b9d9d9a2`（头行修复）引入，而是 Phase 2.6 kit 单路径迁移期间 `acp_notifier.rs` 的 `AgentEvent` 变体被标记为"暂未处理"静默丢弃。此前 view_messages 路径直接处理 SubAgent 渲染，kit 迁移后切换为 ACP 事件驱动，但 notifier 未接入新通道。

| 时间点 | SubAgent 卡片 | SubAgent 子内容 | 数据路径 |
|--------|-------------|----------------|---------|
| Phase 2.6 之前 | 正常显示 | 正常显示 | view_messages 直接处理 |
| Phase 2.6 kit 迁移后 | ❌ 不显示 | ❌ 不显示 | ACP 事件路径，notifier 丢弃 |

## 根因分析

### 数据流追踪

| 步骤 | 位置 | 状态 |
|------|------|------|
| ① SubAgentTool 发出 SubagentStarted | `build_agent.rs:155` → `handler.on_event()` | ✅ 发送 |
| ② 事件泵转发到 ACP Transport | `executor_helpers.rs:200-207` → `sink.push_event()` | ✅ 转发 |
| ③ ACP Transport 序列化并发送 | `event_sink.rs:124-138` → `peri/agent_event` 通知 | ✅ 发送 |
| ④ TUI Client 接收并解析 | `client.rs:116-145` → `AcpNotification::AgentEvent` | ✅ 解析成功 |
| ⑤ **kit notifier 分发** | `acp_notifier.rs:98-104` | ❌ **静默丢弃** |

### 断层点

**`peri-tui/src/kit/acp_notifier.rs:66-105`** `forward_notification` 函数：

```rust
// 暂未在 kit 路径处理——S5+ 接入 DTO 事件时再扩展
AcpNotification::AgentEvent { .. }   // ← SubagentStarted/Stopped 走此通道
| AcpNotification::RequestPermission { .. }
| AcpNotification::PredictionReady { .. }
| AcpNotification::Peri { .. }
| AcpNotification::Other { .. } => {
    debug!("kit ACP notifier: notification variant not yet handled, dropping");
    // ↑ SubagentStarted 在此被丢弃，永不到达 acp_bridge / dispatch_and_notify
}
```

Category ③ 事件（SubagentStarted/SubagentStopped/StateSnapshot/Compact* 等）走 `peri/agent_event` 通道，此通道在 kit notifier 中被整类丢弃。`dispatch_and_notify`（`acp_events.rs`）永远收不到 SubagentStarted → `CurrentTurn.subagents` 永远为空 → SubAgentGroup 从头到尾都不渲染。

### 单元测试为何通过

`acp_events.rs:468` 的 `test_dispatch_subagent_streaming_updates_current_turn_group` 直接构造 `AcpEventData::SubagentStarted` 传入 `dispatch_and_notify`，绕过了 notifier 层。测试测的是 downstream 逻辑，未覆盖 upstream 事件投递断层。

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 在 Peri TUI 中输入需要派发 SubAgent 的 prompt（如"用 explore agent 搜索 TODO"）
  2. 观察消息区——只有父 Agent 的工具调用 Card，无 SubAgent 卡片出现

## 涉及文件

- `peri-tui/src/kit/acp_notifier.rs:66-105` —— **根因所在**：`forward_notification` 丢弃 `AgentEvent` 变体
- `peri-tui/src/kit/acp_events.rs:315-322` —— SubagentStarted → `CurrentTurn.start_subagent()`（正常但从未被调用）
- `peri-acp/src/session/event_sink.rs:124-138` —— ACP 服务端发出 `peri/agent_event` 通知
- `peri-tui/src/acp_client/client.rs:116-157` —— TUI 客户端接收并解析 `peri/agent_event`

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-07 | — | Open | agent | 创建 |

## 修复记录

### 修复 #1（2026-07-07）

- **操作人**：agent
- **修复内容**：`peri-acp/src/event/view_mapper.rs` 中 `agent_name` 改为 task_preview（`b9d9d9a2`），已修复头行任务描述显示
- **涉及 commit**：`b9d9d9a2`
- **验证状态**：已验证（头行正确）
- **副作用**：无——头行修复本身正确。回归是 Phase 2.6 kit notifier 未接入 `AgentEvent` 通道的独立问题

> Issue 标题已从"子内容不显示"更新为"卡片完全不显示"，根因已定位至 `acp_notifier.rs` 事件丢弃。
