# Replay 工具调用卡片：通过 session/update 的 tool_call/tool_call_update 重放

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** session/load replay 时，AI 消息中的 `ToolUse`/`ToolResult` block 通过标准 ACP `session/update` 的 `tool_call`/`tool_call_update` 事件重放，TUI 渲染为 `TuiToolCard`，解决 history 面板恢复后看不到工具调用的 bug。

**Architecture:** 当前 `replay_session_history()` 对 `BaseMessage::Ai` 只提取 text block（`extract_text`），丢弃 `ToolUse`/`ToolResult`；对 `BaseMessage::Tool` 直接 `continue`。修改后，ACP 侧将每个 `ToolUse` 转为 `tool_call` 通知、每个 `ToolResult` 转为 `tool_call_update` 通知（均带 `periReplay=true` meta），TUI 侧在 `handle_session_update` 中检测 replay 标记后走 `ReplayToolStarted`/`ReplayToolEnded` 事件直接写入 `state.committed`，绕开 `current_turn` 管道。

**Tech Stack:** Rust 2021, agent-client-protocol-schema 1.4, serde_json, ratatui-kit

**约束:** ACP 标准 `session/update` 通道不变（用已有 `tool_call`/`tool_call_update` tag）。TUI 侧新增 `ReplayToolStarted`/`ReplayToolEnded` 两个内部事件类型。

**前置依赖:** 上一轮 plan（`2026-07-08-tui-replay-unify-vm-pipeline.md`）已完成 `ReplayUserBubble`/`ReplayAssistantBubble` 删除和 `CommittedAssistantText` 引入。`acp_types.rs` 中 `AcpEventData` 枚举已无 `ReplayUserBubble`/`ReplayAssistantBubble`，`CommittedAssistantText` 已可用。

---

### 文件结构

| 文件 | 角色 | 操作 |
|------|------|------|
| `peri-acp/src/dispatch/session_replay.rs` | ACP 侧：重放循环，emit `tool_call`/`tool_call_update` | 修改 |
| `peri-tui/src/kit/acp_types.rs` | `AcpEventData` 枚举：新增 `ReplayToolStarted`/`ReplayToolEnded` | 修改 |
| `peri-tui/src/kit/acp_notifier.rs` | `handle_session_update`：`tool_call`/`tool_call_update` 分支检测 `is_session_replay` | 修改 |
| `peri-tui/src/kit/acp_events.rs` | `dispatch_and_notify`：新增两个 replay dispatch 分支 + `update_committed_tool_card` 辅助函数 | 修改 |
| `peri-tui/src/kit/acp_bridge.rs` | `event_kind_short`：新增两个 match arm | 修改 |

---

### Task 1: ACP 侧——`replay_session_history` 发射 `tool_call` / `tool_call_update` 通知

**Files:**
- Modify: `peri-acp/src/dispatch/session_replay.rs`

- [ ] **Step 1: 更新 imports**

在现有 import 基础上增加 `ToolCall`、`ToolCallUpdate`、`ToolCallUpdateFields`、`ToolCallStatus`：

```rust
use agent_client_protocol_schema::v1::{
    ContentBlock, ContentChunk, SessionId, SessionNotification, SessionUpdate, TextContent,
    ToolCall, ToolCallId, ToolCallUpdate, ToolCallUpdateFields, ToolCallStatus,
};
```

（`ToolCallId` 已在同一 crate 中，通过 `ToolCallId::new(id)` 构造。）

- [ ] **Step 2: 重写 `replay_session_history` 循环体**

将当前 `for msg in history.iter().filter(|m| !m.is_system())` 循环体中的单条 match 改为：`BaseMessage::Ai` 逐 content block 分发（text → `agent_message_chunk`，ToolUse → `tool_call`）；`BaseMessage::Tool` 逐 block 分发（ToolResult → `tool_call_update`）。

```rust
pub async fn replay_session_history(
    session_id: &str,
    history: &[BaseMessage],
    sender: &dyn ReplaySender,
) -> Result<(), ReplayError> {
    for msg in history.iter().filter(|m| !m.is_system()) {
        match msg {
            BaseMessage::Human { content, .. } => {
                let update = SessionUpdate::UserMessageChunk(replay_chunk(
                    ContentBlock::Text(TextContent::new(extract_text(content))),
                ));
                let notif =
                    SessionNotification::new(SessionId::new(session_id.to_string()), update);
                sender.send(notif).await?;
            }
            BaseMessage::Ai { content, .. } => {
                let blocks = match content {
                    PeriMessageContent::Text(s) => {
                        // 简单文本 → 单个 agent_message_chunk
                        let update = SessionUpdate::AgentMessageChunk(replay_chunk(
                            ContentBlock::Text(TextContent::new(s.clone())),
                        ));
                        let notif =
                            SessionNotification::new(SessionId::new(session_id.to_string()), update);
                        sender.send(notif).await?;
                        continue;
                    }
                    PeriMessageContent::Blocks(blocks) => blocks,
                    PeriMessageContent::Raw(_) => continue,
                };

                for block in blocks {
                    match block {
                        PeriContentBlock::Text { text } => {
                            let update = SessionUpdate::AgentMessageChunk(replay_chunk(
                                ContentBlock::Text(TextContent::new(text.clone())),
                            ));
                            let notif = SessionNotification::new(
                                SessionId::new(session_id.to_string()),
                                update,
                            );
                            sender.send(notif).await?;
                        }
                        PeriContentBlock::ToolUse { id, name, input } => {
                            let tc = ToolCall::new(ToolCallId::new(id.clone()), name.clone())
                                .raw_input(Some(input.clone()))
                                .status(ToolCallStatus::InProgress);
                            let update = SessionUpdate::ToolCall(replay_tool(tc));
                            let notif = SessionNotification::new(
                                SessionId::new(session_id.to_string()),
                                update,
                            );
                            sender.send(notif).await?;
                        }
                        // Image / Document / Reasoning / Unknown → 跳过（replay 无法渲染）
                        _ => {}
                    }
                }
            }
            BaseMessage::Tool {
                content,
                is_error,
                tool_call_id: _,
                ..
            } => {
                let blocks = match content {
                    PeriMessageContent::Text(s) => {
                        // 简单文本结果（极少见）→ 跳过，因为无 tool_use_id 可关联
                        tracing::trace!(
                            session_id,
                            "replay: Tool message with plain text, skipping (no tool_use_id)"
                        );
                        continue;
                    }
                    PeriMessageContent::Blocks(blocks) => blocks,
                    PeriMessageContent::Raw(_) => continue,
                };

                for block in blocks {
                    if let PeriContentBlock::ToolResult {
                        tool_use_id,
                        content: result_content,
                        is_error: result_is_error,
                        ..
                    } = block
                    {
                        let result_text = extract_tool_result_text(result_content);
                        let fields = ToolCallUpdateFields::new()
                            .status(Some(if *result_is_error || *is_error {
                                ToolCallStatus::Failed
                            } else {
                                ToolCallStatus::Completed
                            }))
                            .raw_output(Some(serde_json::Value::String(result_text)));
                        let update = SessionUpdate::ToolCallUpdate(replay_tool_update(
                            ToolCallUpdate::new(
                                ToolCallId::new(tool_use_id.clone()),
                                fields,
                            ),
                        ));
                        let notif = SessionNotification::new(
                            SessionId::new(session_id.to_string()),
                            update,
                        );
                        sender.send(notif).await?;
                    }
                }
            }
            _ => continue,
        }
    }
    Ok(())
}
```

- [ ] **Step 3: 新增 `extract_tool_result_text` helper**

从 `ToolResult.content`（`Vec<ContentBlock>`）提取文本摘要（与 `extract_text` 逻辑一致）：

```rust
/// 从 ToolResult 的 content blocks 中提取文本字符串。
fn extract_tool_result_text(content: &[PeriContentBlock]) -> String {
    content
        .iter()
        .filter_map(|b| match b {
            PeriContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
```

- [ ] **Step 4: 新增 `replay_tool` / `replay_tool_update` helper**

给 `ToolCall` 和 `ToolCallUpdate` 打 `periReplay` meta 标签：

```rust
/// 给 ToolCall 打上 periReplay meta 标记。
fn replay_tool(mut tc: ToolCall) -> ToolCall {
    let mut meta = serde_json::Map::new();
    meta.insert("periReplay".to_string(), serde_json::Value::Bool(true));
    tc.meta = Some(meta);
    tc
}

/// 给 ToolCallUpdate 打上 periReplay meta 标记。
fn replay_tool_update(mut tu: ToolCallUpdate) -> ToolCallUpdate {
    let mut meta = serde_json::Map::new();
    meta.insert("periReplay".to_string(), serde_json::Value::Bool(true));
    tu.meta = Some(meta);
    tu
}
```

- [ ] **Step 5: 编译检查**

```bash
cargo check -p peri-acp --lib 2>&1
```

Expected: COMPILE SUCCESS

- [ ] **Step 6: Commit**

```bash
git add peri-acp/src/dispatch/session_replay.rs
git commit -m "feat(acp): replay tool calls via session/update tool_call/tool_call_update"
```

---

### Task 2: TUI 侧——`AcpEventData` 新增 `ReplayToolStarted` / `ReplayToolEnded` 变体

**Files:**
- Modify: `peri-tui/src/kit/acp_types.rs`

- [ ] **Step 1: 在 `AcpEventData` 枚举中添加两个变体**

在 `CommittedAssistantText { text: String }` 之后（§4.6 之前）添加：

```rust
    /// replay 工具调用开始——直接写入 committed 的 TuiToolCard（is_running=true）。
    ReplayToolStarted {
        tool_id: String,
        tool_name: String,
        input_summary: String,
    },

    /// replay 工具调用结束——更新 committed 中对应 tool_id 的 TuiToolCard。
    ReplayToolEnded {
        tool_id: String,
        output_summary: String,
        is_error: bool,
    },
```

- [ ] **Step 2: 编译检查**

```bash
cargo check -p peri-tui --lib 2>&1
```

Expected: 编译失败（其他文件引用新变体的 match 不够 exhaustive）。

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/acp_types.rs
git commit -m "feat(tui): add ReplayToolStarted/ReplayToolEnded to AcpEventData"
```

---

### Task 3: TUI 侧——`handle_session_update` 检测 replay 分支

**Files:**
- Modify: `peri-tui/src/kit/acp_notifier.rs`

- [ ] **Step 1: 修改 `tool_call` handler**

在 `handle_session_update` 的 `Some("tool_call")` 分支（约 line 388）开头添加 `is_session_replay` 检测：

```rust
        Some("tool_call") => {
            let tool_id = update
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let tool_name = update
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let input_summary = {
                let raw_input = update.get("rawInput").unwrap_or(&Value::Null);
                summarize_input(&tool_name, raw_input)
            };
            if is_session_replay {
                Some(AcpEventData::ReplayToolStarted {
                    tool_id,
                    tool_name,
                    input_summary,
                })
            } else {
                let tool_started = crate::kit::stream_data::TuiToolStarted {
                    tool_id,
                    tool_name,
                    input_summary,
                    agent_id,
                };
                Some(AcpEventData::ToolStarted(tool_started))
            }
        }
```

- [ ] **Step 2: 修改 `tool_call_update` handler**

在 `Some("tool_call_update")` 分支（约 line 412）同理添加 replay 检测：

```rust
        Some("tool_call_update") => {
            let tool_id = update
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let output_summary = update
                .get("rawOutput")
                .or_else(|| update.get("fields").and_then(|f| f.get("rawOutput")))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let is_error = update
                .get("status")
                .or_else(|| update.get("fields").and_then(|f| f.get("status")))
                .and_then(|v| v.as_str())
                .map(|s| s == "failed")
                .unwrap_or(false);
            if is_session_replay {
                Some(AcpEventData::ReplayToolEnded {
                    tool_id,
                    output_summary,
                    is_error,
                })
            } else {
                let tool_ended = crate::kit::stream_data::TuiToolEnded {
                    tool_id,
                    output_summary,
                    is_error,
                    agent_id,
                };
                Some(AcpEventData::ToolEnded(tool_ended))
            }
        }
```

- [ ] **Step 3: 编译检查**

```bash
cargo check -p peri-tui --lib 2>&1
```

Expected: 仍有编译错误（`event_kind_short` 和 `dispatch_and_notify` match 不完整）。

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/kit/acp_notifier.rs
git commit -m "feat(tui): route replay tool_call/tool_call_update to ReplayToolStarted/ReplayToolEnded"
```

---

### Task 4: TUI 侧——`dispatch_and_notify` 处理新事件

**Files:**
- Modify: `peri-tui/src/kit/acp_events.rs`

- [ ] **Step 1: 添加 `ReplayToolStarted` dispatch 分支**

在 `CommittedAssistantText` 分支之后添加：

```rust
        ReplayToolStarted {
            tool_id,
            tool_name,
            input_summary,
        } => {
            use crate::kit::tui_render_unit::{TuiToolCard, tui_hash_str};
            let card = TuiToolCard {
                tool_id: tool_id.clone(),
                tool_name: tool_name.clone(),
                input_summary: input_summary.clone(),
                output_summary: String::new(),
                is_error: false,
                is_running: true,
                running_duration_ms: None,
                diff: None,
                content_hash: tui_hash_str(&format!(
                    "{}|{}|{}||false|true",
                    tool_id, tool_name, input_summary
                )),
            };
            state
                .committed
                .push_back(TuiRenderUnit::TuiToolCard(card));
            push_view_models(state);
            push_acp_state(state);
        }
```

- [ ] **Step 2: 添加 `ReplayToolEnded` dispatch 分支 + 辅助函数**

在 `ReplayToolStarted` 分支之后添加：

```rust
        ReplayToolEnded {
            tool_id,
            output_summary,
            is_error,
        } => {
            update_committed_tool_card(state, tool_id, output_summary, *is_error);
            push_view_models(state);
            push_acp_state(state);
        }
```

- [ ] **Step 3: 添加 `update_committed_tool_card` 辅助函数**

在 `dispatch_and_notify` 函数之前（与 `extract_message_text` 同层级）添加：

```rust
/// 在 `state.committed` 中按 `tool_id` 查找并更新 TuiToolCard。
///
/// 用于 replay 场景：`ReplayToolStarted` 先 push 一张 is_running=true 的卡片，
/// 后续 `ReplayToolEnded` 到达时更新 output + is_running=false。
/// 如果找不到对应 tool_id（工具调用先于卡片到达），静默忽略。
fn update_committed_tool_card(
    state: &mut BridgeState,
    tool_id: &str,
    output_summary: &str,
    is_error: bool,
) {
    use crate::kit::tui_render_unit::{TuiToolCard, tui_hash_str};
    for i in 0..state.committed.len() {
        if let TuiRenderUnit::TuiToolCard(card) = &state.committed[i] {
            if card.tool_id == tool_id && card.is_running {
                let updated = TuiToolCard {
                    tool_id: card.tool_id.clone(),
                    tool_name: card.tool_name.clone(),
                    input_summary: card.input_summary.clone(),
                    output_summary: output_summary.to_string(),
                    is_error,
                    is_running: false,
                    running_duration_ms: None,
                    diff: card.diff.clone(),
                    content_hash: tui_hash_str(&format!(
                        "{}|{}|{}|{}|{is_error}|false",
                        card.tool_id, card.tool_name, card.input_summary, output_summary,
                    )),
                };
                state.committed = state
                    .committed
                    .update(i, TuiRenderUnit::TuiToolCard(updated));
                return;
            }
        }
    }
}
```

- [ ] **Step 4: 编译检查**

```bash
cargo check -p peri-tui --lib 2>&1
```

Expected: 只剩 `event_kind_short` 编译错误。

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/acp_events.rs
git commit -m "feat(tui): dispatch ReplayToolStarted/ReplayToolEnded to committed tool cards"
```

---

### Task 5: TUI 侧——更新 `event_kind_short`

**Files:**
- Modify: `peri-tui/src/kit/acp_bridge.rs`

- [ ] **Step 1: 添加 match arm**

在 `event_kind_short` 函数中、`CommittedAssistantText` 之后添加：

```rust
        ReplayToolStarted { .. } => "ReplayToolStarted",
        ReplayToolEnded { .. } => "ReplayToolEnded",
```

- [ ] **Step 2: 编译检查**

```bash
cargo check -p peri-tui --lib 2>&1
```

Expected: COMPILE SUCCESS

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/acp_bridge.rs
git commit -m "chore(tui): add event_kind_short entries for ReplayToolStarted/ReplayToolEnded"
```

---

### Task 6: 运行全量测试验证

- [ ] **Step 1: peri-tui 测试**

```bash
cargo test -p peri-tui --lib 2>&1
```

Expected: 所有测试通过

- [ ] **Step 2: peri-acp 测试**

```bash
cargo test -p peri-acp --lib 2>&1
```

Expected: 所有测试通过

- [ ] **Step 3: 全 workspace 编译**

```bash
cargo check --workspace 2>&1
```

Expected: COMPILE SUCCESS

---

### 改动总结

| 文件 | 变更 | 行数估计 |
|------|------|----------|
| `peri-acp/src/dispatch/session_replay.rs` | 重写循环体，逐 content block 分发；新增 `extract_tool_result_text`、`replay_tool`、`replay_tool_update` | +80 / -20 |
| `peri-tui/src/kit/acp_types.rs` | 新增 `ReplayToolStarted` / `ReplayToolEnded` 变体 | +12 |
| `peri-tui/src/kit/acp_notifier.rs` | `tool_call` / `tool_call_update` 分支增加 `is_session_replay` 路由 | +30 / -10 |
| `peri-tui/src/kit/acp_events.rs` | 新增 dispatch 分支 + `update_committed_tool_card` 辅助函数 | +50 |
| `peri-tui/src/kit/acp_bridge.rs` | event_kind_short 加两行 | +2 |

**净变化**：约 +174 / -30 行。

### 数据流图

```
session/load → replay_session_history()
  │
  ├─ BaseMessage::Ai → blocks 逐条遍历
  │   ├─ Text     → agent_message_chunk (periReplay=true) → CommittedAssistantText → committed.push(TuiAssistantBubble)
  │   └─ ToolUse  → tool_call            (periReplay=true) → ReplayToolStarted     → committed.push(TuiToolCard{running})
  │
  ├─ BaseMessage::Human
  │   └─ Text     → user_message_chunk   (periReplay=true) → LocalUserBubble       → committed.push(TuiUserBubble)
  │
  └─ BaseMessage::Tool → blocks 逐条遍历
      └─ ToolResult → tool_call_update   (periReplay=true) → ReplayToolEnded        → update_committed_tool_card
```

所有 replay 事件复用标准 `session/update` 通道，TUI 侧通过 `periReplay` meta 标记区分正常流 vs replay 流。
