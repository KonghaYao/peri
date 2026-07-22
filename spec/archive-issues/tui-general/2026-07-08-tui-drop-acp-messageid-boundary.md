> 归档于 2026-07-10，原路径 spec/issues/2026-07-08-tui-drop-acp-messageid-boundary.md

# TUI 丢弃 ACP agent_message_chunk 的 messageId，消息边界靠推断而非协议字段

**状态**：Fixed
**优先级**：中
**类型**：重构
**创建日期**：2026-07-08

## 问题描述

TUI 在解码 ACP `session/update` → `agent_message_chunk` 事件时，只提取了 `content.text`，完全忽略了 ACP 协议中每条消息自带的 `messageId` 字段。Agent 层每次 ReAct 迭代创建一个新的 `BaseMessage`（带有唯一 `message_id`），但 TUI 收到的 `TuiTextChunk` 没有 `message_id`，只能靠 ContentSegment 变体切换来推断消息边界。

**结果**：ContentSegment 推断规则（"末段不是 Text 就新建段"）比协议字段脆弱——如果 ACP 事件到达顺序出现任何异常（如 TextChunk 连续到达且属于不同 message），推断就会失效。

## 症状详情

| 现象 | ACP 协议实际行为 | TUI 当前处理 |
|------|-----------------|-------------|
| AI 输出"1" → Read → "2" → Bash 时，文本"1"和"2"被合并 | LLM 为"1"创建 msg_A，为"2"创建 msg_B，携带不同 messageId | TUI 收到两个 TextChunk，无 messageId，靠 ContentSegment 检测 Tool 后新建段 |
| message 边界判断 | 协议自带：messageId 变化 = 新消息 | 推断：末段变体切换 = 新消息 |
| 重放历史时每条消息正确分离 | Replay 路径直接逐条 push TuiAssistantBubble，不走 CurrentTurn | ✅ 不受影响 |

## 涉及文件

- `peri-tui/src/kit/stream_data.rs` —— `TuiTextChunk` 缺少 `message_id` 字段
- `peri-tui/src/kit/acp_notifier.rs:324-337` —— 解码 `agent_message_chunk` 时丢弃 `messageId`
- `peri-tui/src/kit/acp_types.rs` —— `CurrentTurn.append_text()` 使用 ContentSegment 变体推断，应改用 `message_id` 变化判断
- `peri-acp-types/src/message.rs:45-51` —— `ContentChunk` 定义，包含 `message_id` 字段（已存在但未被消费）

## 期望改进方向

1. `TuiTextChunk` 加 `message_id: Option<String>`——从 ACP 协议透传每轮 ReAct 迭代的 message 标识
2. `acp_notifier.rs` 解码 `agent_message_chunk` 时提取 `messageId` 并填入 `TuiTextChunk`
3. `CurrentTurn` 用 `last_message_id` 跟踪——`append_text` 检测 `message_id` 变化时新建 ContentSegment，去掉变体推断逻辑

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-08 | — | Open | agent | 创建 |

## 修复记录

| 日期 | commit | 说明 |
|------|--------|------|
| 2026-07-08 | cdc63090 | `TuiTextChunk` / `TuiReasoningChunk` 增加 `message_id`，notifier 从 `ContentChunk.messageId` 提取透传 |
| 2026-07-08 | cfc5c33a | `CurrentTurn` 增加 `TurnSegment` 交错追踪 + `last_message_id` 检测边界，`build_view_models` 按段产出独立气泡 |
| 2026-07-08 | 07bbf011 | `AssistantText` 段增加 `reasoning_end_byte`，修复 reasoning 全部塞入首段的 bug |
