# MQ 注入的 user message 不通过 ACP 反馈到 TUI，导致用户气泡缺失 + AI 消息重叠

**状态**：Open
**优先级**：高
**类型**：Bug
**创建日期**：2026-07-08

## 问题描述

通过 `MessageQueue` 注入主 agent inbox 的 user message（如 bg agent 完成时的 `[后台任务 bg-xxx 已完成] 输出：...` 回调消息）不产生任何 ACP 事件，TUI 侧完全看不到这些用户消息的气泡。由此衍生的 AI 响应（bg 回调触发的新一轮 ReAct 迭代产出的文字）在 TUI 上与前一轮的 AI 输出之间没有用户气泡分隔，视觉上形成 AI 消息"重叠"。

**具体场景**：用户启动了一个 bg agent，run 结束后过了 1s，MQ 中注入了一条回调消息（`[后台任务 bg-xxx 已完成]`）。agent 侧正确收到了这条消息并产出了 AI 回复，但：
1. TUI 侧**完全没有**出现这条回调消息的用户气泡
2. 回调触发的 AI 回复文字与前一轮 AI 输出之间**没有**用户气泡分隔，AI 消息"重叠"

## 症状详情

| 观察点 | 期望行为 | 实际行为 |
|--------|---------|---------|
| bg agent 完成后的回调消息 | TUI 应显示 `❯ [后台任务 bg-xxx 已完成] 输出：...` 用户气泡 | ❌ **完全不显示** |
| 回调触发的 AI 回复 | 应在新用户气泡之后独立显示 | ❌ **与前一轮 AI 输出之间无用户气泡分隔**，视觉上形成"重叠" |
| TUI 是否知道发生了新 turn | 应有 TurnDone → 新的 UserBubble → 新的 AssistantBubble 流程 | ❌ UserBubble 断层——只有 AssistantBubble，无对应的用户消息 |

## 数据流断点

MQ 注入消息的完整链路存在**三重断点叠加**：

### 断点 ①：`append_messages_to_transcript` 零事件产出

```
bg agent 完成
  → AsyncRouter::route_bg_result() [async_router.rs:58]
    → BaseMessage::human(result.to_notification())
  → InboxHandle::push_defer(MessageSource::SubAgentComplete, human_message) [inbox.rs:147]
  → MessageQueue 入队
  → End 阶段 drain_for_end() 取走 [end.rs:14-15]
  → append_messages_to_transcript() 写入 transcript [stages/mod.rs:614]
    → Defer 消息被 <system-reminder> 包裹 [mod.rs:500-504]
    → transcript.append(content) [mod.rs:507] ← ❌ 零 ExecutorEvent 产出
```

`MessageTranscript::append()` (`transcript.rs:271-278`) 仅做 `entries.push` + `send_persist`（磁盘持久化），不发射任何事件。`StageContext` (`mod.rs:70-99`) 虽持有 `event_bus: Arc<EventBus>` 字段，但 `append_messages_to_transcript` 是一个纯函数不接收 `&StageContext`，接触不到 event_bus。

### 断点 ②：MessageAdded 被 mapper + router 双层过滤（防御性）

即使假设 `MessageAdded` 事件被构造，也会被丢弃：

| 过滤层 | 位置 | 行为 |
|--------|------|------|
| `map_event()` | `mapper.rs:303` | `ExecutorEvent::MessageAdded(_) => MappedEvent::none()` |
| `route()` | `router.rs:116` | `ExecutorEvent::MessageAdded(_) => None` |

注：`MessageAdded` 变体在 `events.rs:169` 中定义但**全代码库无构造调用点**——它从未被使用。

### 断点 ③：TUI 侧 `user_message_chunk` 仅用于 session replay

```rust
// acp_notifier.rs:434-445
if is_session_replay {
    Some(AcpEventData::ReplayAssistantBubble { text })  // agent_message_chunk
}
// ...
Some("user_message_chunk") => {
    Some(AcpEventData::ReplayUserBubble { text })  // 仅 session replay
}
```

- **用户手动输入** → 气泡来自 TUI 本地 `LocalUserBubble`（`LOCAL_EVENT_TX` channel → `dispatch_and_notify` → `acp_events.rs:399`）
- **Session replay** → 气泡来自 ACP `user_message_chunk`（仅 `is_session_replay=true`）
- **MQ 注入** → ❌ 既不是本地输入（无 `LocalUserBubble`），也不是 replay（无 `user_message_chunk`）→ **零路径产生气泡**

### 完整链路 vs 断点一览

```
bg agent 完成 ─┬─ BgRegistryEvent::Completed → ACP unstable event
               │   → TUI 通知条 ✅（通知条生效）
               │
               └─ route_bg_result(result)
                  → push_defer(BaseMessage::human(...))
                  → transcript（<system-reminder> 包裹）
                  → ❌ 零 ACP 事件 → TUI 无气泡 → AI 消息重叠
```

## 验证记录（2026-07-08 ultra-batch 对抗验证）

三个 subagent 从不同维度独立验证，全部确认根因假设：

| 验证维度 | Agent | 结果 | 关键发现 |
|---------|-------|------|---------|
| MQ→transcript 零事件 | Agent 1 | ✅ 确认 | `append_messages_to_transcript` 纯函数；`transcript.append()` 仅持久化；`run_react_loop` 调用前后无事件发送 |
| 输入路径对比 | Agent 2 | ✅ 确认 | 手动输入走 `LocalUserBubble`（本地），replay 走 `ReplayUserBubble`（仅 replay），MQ 两端都不通 |
| bg 完成→MQ 入口 | Agent 3 | ✅ 确认 | `async_router.rs:58` push `BaseMessage::human` → `push_defer` → `<system-reminder>` 包裹写入，链路完整但事件通路断裂 |

## 复现条件

- **复现频率**：必现（每次 bg agent 完成后回调消息都不显示）
- **触发步骤**：
  1. 启动 TUI（`cargo run -p peri-tui -- -a`）
  2. 输入 `/bg <prompt>` 启动后台 agent
  3. 等待 bg agent 完成
  4. 观察：主消息区不会出现后台 agent 完成时的回调用户气泡
  5. 如果 bg 回调触发了后续 AI 回复（主 agent 基于回调结果继续输出），新 AI 文字与前一轮 AI 输出之间无用户气泡
- **环境**：任意 OS/模型

## 涉及文件

用户提到的文件及从症状理解中确认的相关文件：

- `peri-agent/src/agent/stages/mod.rs:490-614` —— `append_messages_to_transcript` + ReAct 循环中 awakened_messages 写入逻辑（MQ → transcript 的断点）
- `peri-agent/src/session/queue.rs` —— `MessageQueue` / `drain_for_end` 定义（消息排队语义）
- `peri-agent/src/agent/session/inbox.rs` —— `InboxHandle::push_defer` 生产者入口
- `peri-acp/src/event/mapper.rs:303` —— `MessageAdded` 被过滤（即使有 Event 也不转发）
- `peri-acp/src/event/router.rs:116` —— `MessageAdded` 同样被丢弃
- `peri-acp/src/session/executor.rs:446-456` —— bg_results 注入 MQ 的入口注释
- `peri-tui/src/kit/acp_notifier.rs:434-445` —— `user_message_chunk` 处理（当前仅用于 session replay）

## 期望改进方向

1. MQ 注入的 user message（`append_messages_to_transcript` 写入的 Prompt / Defer 消息）应生成对应的 ACP 事件，让 TUI 能渲染用户气泡
2. 建议方式：在 `append_messages_to_transcript` 后（或在 ReAct 循环中 consume awakened_messages 之后），通过 event sink 发送 `session/update` → `user_message_chunk` 事件
3. 或者：在 ACP executor 层，当检测到 MQ 消息被消费并写入 transcript 后，主动生成对应的 `ReplayUserBubble` 事件推入 TUI 通道

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-08 | — | Open | agent | 创建（issue-create skill） |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
