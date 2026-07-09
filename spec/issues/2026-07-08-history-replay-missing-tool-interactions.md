# History 面板恢复的对话历史缺少工具调用和工具结果，消息内容与原始对话不一致

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-08
**类型**：Bug

## 问题描述

用户通过 History 面板（`/history`）切换到历史 session 后，消息区显示的历史消息缺少原对话中的**工具调用卡片**（如 Read / Bash / Edit 等）及**工具执行结果**。恢复后用户只能看到交替的 Human/AI 文本气泡，整个对话流程不完整。此外 AI 回复的文字内容也与原始对话不完全一致。

## 症状详情

| 维度 | 观察 |
|------|------|
| 触发操作 | History 面板选择 session → 按 Enter 切换 |
| 实际表现 | 消息区只显示 Human/AI 文本气泡，工具调用卡片和工具结果均不可见 |
| 期望表现 | 恢复结果应与原始对话一致，包含工具调用卡片和工具执行结果 |
| 复现频率 | 每次切换必现 |
| 影响范围 | 所有包含工具调用的历史 session |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 进行一次包含工具调用的对话（比如 "帮我读取 src/main.rs 的内容"）
  2. 观察实时对话中工具调用的正常展示（含工具卡片和结果）
  3. 退出或切换到其他 session
  4. 通过 `/history` 打开 History 面板
  5. 选择刚才的对话，按 Enter 恢复
  6. 观察消息区——工具调用卡片和工具结果消失，仅有交替的 Human/AI 文本气泡
- **环境**：任意模型、macOS、任意配置

## 涉及文件

- `peri-tui/src/kit/acp_notifier.rs`（line 307-447）—— `handle_session_update` 处理 `agent_message_chunk` 时，`is_session_replay=true` 分支硬编码映射为 `ReplayAssistantBubble`，直接写入 committed，绕过了正常的 `current_turn` 管道
- `peri-tui/src/kit/acp_events.rs`（line 325-346）—— `ReplayUserBubble` 和 `ReplayAssistantBubble` 处理分支直接将 ViewModel 追加到 `state.committed`，不经过 `current_turn` 的 `ToolStarted`/`ToolEnded` 流式路径
- `peri-tui/src/kit/acp_bridge.rs`（line 58-93）—— BRIDGE_RESET_COUNTER 检测和 session filter 的时序窗口

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-08 | — | Open | deepseek-v4-pro | 创建 |
| 2026-07-08 | Open | Fixed | deepseek-v4-pro | 完成修复 |

## 修复记录

### 修复 #1：删除 ReplayUserBubble/ReplayAssistantBubble 旁路，引入 CommittedAssistantText / ReplayToolStarted / ReplayToolEnded（2026-07-08）

- **操作人**：agent
- **用户原意**：replay 历史消息应该复用正常消息流路径，ACP 层不改（session/load 数据机制已经完善）
- **修复内容**：
  - **`peri-tui/src/kit/acp_types.rs`**：删除 `ReplayUserBubble` / `ReplayAssistantBubble` 枚举变体及对应 `decode` 分支，新增 `CommittedAssistantText { text }`、`ReplayToolStarted { tool_id, tool_name, input_summary }`、`ReplayToolEnded { tool_id, output_summary, is_error }` 三个变体
  - **`peri-tui/src/kit/acp_events.rs`**：删除 Replay* dispatch 分支，新增 `CommittedAssistantText` / `ReplayToolStarted` / `ReplayToolEnded` dispatch 分支（写入 `state.committed`），新增 `update_committed_tool_card` 辅助函数（`im::Vector::update` 原子替换卡片）
  - **`peri-tui/src/kit/acp_notifier.rs`**：`user_message_chunk` → `LocalUserBubble`（复用已有路径），`agent_message_chunk` replay → `CommittedAssistantText`，`tool_call` replay → `ReplayToolStarted`，`tool_call_update` replay → `ReplayToolEnded`，`agent_thought_chunk` replay → `CommittedAssistantText`
  - **`peri-tui/src/kit/acp_bridge.rs`**：`event_kind_short` 同步新增变体
  - **`peri-acp/src/dispatch/session_replay.rs`**：重写 `replay_session_history`，AI 消息逐 content block 分发（`Text` → `agent_message_chunk` + `ToolUse` → `tool_call` + `tool_calls` 字段去重发射），Tool 消息用 `extract_text` + `tool_call_id` 发射 `tool_call_update`
- **涉及文件**：5 个（见上）
- **验证状态**：已验证（peri-tui 415 tests / peri-acp 278 tests passed）

### 关键踩坑记录

#### 坑 1：`#[serde(rename = "_meta")]` 导致 `is_session_replay` 永远为 false

ACP schema 中 `ContentChunk`、`ToolCall`、`ToolCallUpdate` 均标注 `#[serde(rename = "_meta")]`，序列化后的 JSON key 是 `"_meta"`（带下划线），但 `acp_notifier.rs` 的 `is_session_replay` 检测只查 `"meta"`（无下划线）。**后果**：所有 replay 事件走非 replay 路径（`current_turn`），AI 文本没有 `messageId` 被合并成大气泡 + 工具卡片穿插其中 → 顺序混乱。

**修复**：检测链改为 `"_meta"` → `"meta"` → `"content._meta"` → `"content.meta"` 四级 fallback。

#### 坑 2：`BaseMessage::Tool` 存储格式是 `MessageContent::Text(String)`，不是 `Blocks`

`tool_dispatch.rs` 构造工具消息时使用 `BaseMessage::tool_result(id, output.as_str())`，`output` 是 `String` → 存入 `MessageContent::Text(String)`。初始实现假定是 `MessageContent::Blocks(Vec<ContentBlock>)` 包含 `ContentBlock::ToolResult`，导致所有 Tool 消息被 `continue` 跳过，`tool_call_update` 从未发射 → 工具卡片永远显示为 `InProgress` 状态。

#### 坑 3：AI 消息有两个工具调用来源，需去重

`BaseMessage::Ai` 的工具调用既可能出现在 `content` blocks 中的 `ContentBlock::ToolUse`（Anthropic 风格），也可能出现在独立字段 `tool_calls: Vec<ToolCallRequest>`（OpenAI 风格）。需用 `HashSet` 收集 content blocks 的 id 去重，避免同一工具调用被两次发射为 `tool_call` 通知。

#### 坑 4：空工具输出被跳过

初始实现在 `Tool` 消息输出为空字符串时 `continue` 跳过，导致 `ReplayToolEnded` 不发射 → 工具卡片永远停留在 `InProgress`。修复为移除该检查，允许空输出（`ToolCallUpdate { raw_output: "" }`）正常发射。

#### 坑 5：`CommittedAssistantText` 硬编码 `content_hash: 0`

与 live 路径（`build_view_models` 使用 `tui_hash_str(&format!("{}|{}", text, reasoning))`）不一致，破坏 `content_hash` 契约。修复为 `tui_hash_str(&format!("{}|", text))`，保持与单段消息的 live 路径一致。

#### 坑 6：`agent_thought_chunk` 缺少 replay 处理

`agent_message_chunk` 和 `tool_call`/`tool_call_update` 都有 `is_session_replay` 分支，但 `agent_thought_chunk` 没有 → replay 时 reasoning 文本走 `current_turn` 流式路径后被丢弃。修复为 replay 时走 `CommittedAssistantText`。
