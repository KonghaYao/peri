# TUI Replay 消息流统一：消除 ReplayUserBubble/ReplayAssistantBubble 旁路

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 session/load replay 路径与正常消息流路径统一，删除 `ReplayUserBubble`/`ReplayAssistantBubble` 两条专属旁路，让 replay 使用与正常输入一致的 VM 类型和 atom 写入通道。

**Architecture:** 当前 `ReplayUserBubble` 与 `LocalUserBubble` 在 dispatch_and_notify 中产生**完全相同的 VM 和 atom 写入**（都是 `TuiRenderUnit::TuiUserBubble` → `committed.push_back` → `push_view_models`）。同样 `ReplayAssistantBubble` 也产生与 TurnDone 归档相同的 `TuiRenderUnit::TuiAssistantBubble`。本计划删除这两个冗余变体，让 replay 通道直接复用现有事件类型。

**Tech Stack:** Rust 2021, tokio, ratatui-kit

**约束:** ACP session/load 协议不变（不改 `peri-acp/src/dispatch/session_replay.rs`）。所有改动在 `peri-tui/src/kit/` 内。

---

### Task 1: 在 AcpEventData 中添加 CommittedAssistantText 变体，删除 ReplayUserBubble/ReplayAssistantBubble

**Files:**
- Modify: `peri-tui/src/kit/acp_types.rs:589-749`

- [ ] **Step 1: 删除 ReplayUserBubble 和 ReplayAssistantBubble 变体，添加 CommittedAssistantText**

在 `AcpEventData` 枚举中，找到以下两个变体（line 624-627）并删除：

```rust
    /// `"replay-user-bubble"` -- user bubble from session history replay.
    ReplayUserBubble { text: String },

    /// `"replay-assistant-bubble"` -- assistant bubble from session history replay.
    ReplayAssistantBubble { text: String },
```

在同位置添加新变体：

```rust
    /// TUI 内部事件：直接将完整 AI 文本气泡追加到 committed。
    /// 用于 session/load replay 及任何需要旁路 current_turn 直接归档的场景。
    CommittedAssistantText { text: String },
```

- [ ] **Step 2: 更新 decode 方法**

在 `AcpEventData::decode` 中（line 757-773），删除以下两行：

```rust
            "replay-user-bubble" => AcpEventData::ReplayUserBubble {
                text: data["text"].as_str().unwrap_or_default().to_string(),
            },
            "replay-assistant-bubble" => AcpEventData::ReplayAssistantBubble {
                text: data["text"].as_str().unwrap_or_default().to_string(),
            },
```

- [ ] **Step 3: 更新 decode 测试**

在 `peri-tui/src/kit/acp_types.rs` 的测试区（line 1039-1055），删除 `test_decode_replay_user_bubble` 和 `test_decode_replay_assistant_bubble` 两个测试函数。

- [ ] **Step 4: 编译检查**

Run: `cargo check -p peri-tui --lib 2>&1`

Expected: 编译失败——其他文件引用了已删除的变体。我们将在后续 Task 中逐一修复。

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/acp_types.rs
git commit -m "refactor(tui): delete ReplayUserBubble/ReplayAssistantBubble, add CommittedAssistantText"
```

---

### Task 2: 更新 dispatch_and_notify——用 CommittedAssistantText 取代 ReplayAssistantBubble，用 LocalUserBubble 取代 ReplayUserBubble

**Files:**
- Modify: `peri-tui/src/kit/acp_events.rs:325-346, 465-477`

- [ ] **Step 1: 删除 ReplayUserBubble 和 ReplayAssistantBubble dispatch 分支**

在 `dispatch_and_notify` 函数中（line 325-346），删除以下两个 match arm：

```rust
        // ── Replay events ──
        ReplayUserBubble { text } => {
            let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble {
                text: text.clone(),
                content_hash: tui_hash_str(text),
                is_system_reminder: false,
            });
            state.committed.push_back(vm);
            push_view_models(state);
            push_acp_state(state);
        }
        ReplayAssistantBubble { text } => {
            let vm = TuiRenderUnit::TuiAssistantBubble(
                crate::kit::tui_render_unit::TuiAssistantBubble {
                    text: text.clone(),
                    reasoning: None,
                    content_hash: 0,
                },
            );
            state.committed.push_back(vm);
            push_view_models(state);
            push_acp_state(state);
        }
```

- [ ] **Step 2: 添加 CommittedAssistantText dispatch 分支**

在 `LocalUserBubble` 分支后面（line 477 之后）添加：

```rust
        CommittedAssistantText { text } => {
            let vm = TuiRenderUnit::TuiAssistantBubble(
                crate::kit::tui_render_unit::TuiAssistantBubble {
                    text: text.clone(),
                    reasoning: None,
                    content_hash: 0,
                },
            );
            state.committed.push_back(vm);
            push_view_models(state);
            push_acp_state(state);
        }
```

- [ ] **Step 3: 删除 ReplayUserBubble/ReplayAssistantBubble 测试**

在 `peri-tui/src/kit/acp_events.rs` 的测试区（line 722-780），删除以下两个测试函数（如果存在）：
- `test_replay_user_bubble_appends_to_committed`
- `test_replay_assistant_bubble_appends_to_committed`

- [ ] **Step 4: 编译检查**

Run: `cargo check -p peri-tui --lib 2>&1`

Expected: 仍有编译错误——`event_kind_short` 和 `acp_notifier.rs` 引用了已删除的变体。

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/acp_events.rs
git commit -m "refactor(tui): replace ReplayUserBubble/ReplayAssistantBubble dispatch with CommittedAssistantText"
```

---

### Task 3: 更新 acp_notifier.rs——replay 路径映射到 LocalUserBubble 和 CommittedAssistantText

**Files:**
- Modify: `peri-tui/src/kit/acp_notifier.rs:324-348, 434-446`

- [ ] **Step 1: 修改 agent_message_chunk 的 replay 分支**

在 `handle_session_update` 函数中（line 338-339），将：

```rust
            if is_session_replay {
                Some(AcpEventData::ReplayAssistantBubble { text })
```

改为：

```rust
            if is_session_replay {
                Some(AcpEventData::CommittedAssistantText { text })
```

- [ ] **Step 2: 修改 user_message_chunk 的 replay 分支**

在 `handle_session_update` 函数中（line 438-446），将：

```rust
        Some("user_message_chunk") => {
            let text = update
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(AcpEventData::ReplayUserBubble { text })
        }
```

改为：

```rust
        Some("user_message_chunk") => {
            let text = update
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(AcpEventData::LocalUserBubble { text })
        }
```

- [ ] **Step 3: 更新测试**

在 `peri-tui/src/kit/acp_notifier.rs` 的测试区查找 `test_session_replay_agent_message_chunk_to_replay_assistant_bubble`（line 785 附近），将验证从 `ReplayAssistantBubble` 改为 `CommittedAssistantText`：

找到测试中的：
```rust
AcpEventData::ReplayAssistantBubble { text }
```

改为：
```rust
AcpEventData::CommittedAssistantText { text }
```

- [ ] **Step 4: 编译检查**

Run: `cargo check -p peri-tui --lib 2>&1`

Expected: 只剩 `event_kind_short` 的编译错误。

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/acp_notifier.rs
git commit -m "refactor(tui): route replay messages through LocalUserBubble + CommittedAssistantText"
```

---

### Task 4: 更新 event_kind_short 诊断 helper

**Files:**
- Modify: `peri-tui/src/kit/acp_bridge.rs:154-194`

- [ ] **Step 1: 更新 event_kind_short match arm**

在 `event_kind_short` 函数中（line 154-194），将：

```rust
        SessionReplayStarted => "SessionReplayStarted",
        SessionReplayDone => "SessionReplayDone",
        TurnDone => "TurnDone",
        TurnInterrupted { reason: _ } => "TurnInterrupted",
        ReplayUserBubble { .. } => "ReplayUserBubble",
        ReplayAssistantBubble { .. } => "ReplayAssistantBubble",
        LocalUserBubble { .. } => "LocalUserBubble",
```

改为：

```rust
        SessionReplayStarted => "SessionReplayStarted",
        SessionReplayDone => "SessionReplayDone",
        TurnDone => "TurnDone",
        TurnInterrupted { reason: _ } => "TurnInterrupted",
        LocalUserBubble { .. } => "LocalUserBubble",
        CommittedAssistantText { .. } => "CommittedAssistantText",
```

- [ ] **Step 2: 编译检查**

Run: `cargo check -p peri-tui --lib 2>&1`  
Expected: COMPILE SUCCESS

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/acp_bridge.rs
git commit -m "refactor(tui): update event_kind_short for CommittedAssistantText"
```

---

### Task 5: 运行全量测试验证

**Files:**
- None（验证步骤）

- [ ] **Step 1: 运行 peri-tui 测试**

```bash
cargo test -p peri-tui --lib 2>&1
```

Expected: 所有测试通过（关注 acp_types / acp_events / acp_notifier 模块的测试）

- [ ] **Step 2: 运行 peri-tui 编译检查**

```bash
cargo check -p peri-tui 2>&1
```

Expected: COMPILE SUCCESS

- [ ] **Step 3: 运行全 workspace 编译检查**

```bash
cargo check --workspace 2>&1
```

Expected: COMPILE SUCCESS

- [ ] **Step 4: 确认改动范围**

```bash
git diff --stat HEAD~4..HEAD
```

Expected: ~4 文件改动，净减少约 30 行

- [ ] **Step 5: Commit（如步骤 1-3 通过）**

```bash
# 本 Task 为验证步骤，无需单独 commit。
# 如果发现问题，在对应 Task 中修复后重新验证。
```

---

### Task 6: 自我审查 & 清理

- [ ] **检查点 1：ReplayUserBubble/ReplayAssistantBubble 是否彻底删除**

```bash
grep -rn "ReplayUserBubble\|ReplayAssistantBubble" peri-tui/src/kit/ --include="*.rs"
```

Expected: 无结果（除了注释中可能的历史引用）

- [ ] **检查点 2：verify AcpEventData::decode 不再注册旧事件名**

```bash
grep -rn "replay-user-bubble\|replay-assistant-bubble" peri-tui/src/kit/acp_types.rs
```

Expected: 无结果

- [ ] **检查点 3：验证 replay 行为不变**

手动确认 `LocalUserBubble` 和 `CommittedAssistantText` 的 dispatch 分支与原来 `ReplayUserBubble`/`ReplayAssistantBubble` 产生相同的结果：
- `LocalUserBubble` → `TuiRenderUnit::TuiUserBubble { content_hash: tui_hash_str(text), is_system_reminder: false }` → `committed.push_back` → `push_view_models`
- `CommittedAssistantText` → `TuiRenderUnit::TuiAssistantBubble { text, reasoning: None, content_hash: 0 }` → `committed.push_back` → `push_view_models`

```bash
grep -A 12 "LocalUserBubble { text }" peri-tui/src/kit/acp_events.rs
grep -A 12 "CommittedAssistantText { text }" peri-tui/src/kit/acp_events.rs
```

Expected: 两个分支的 VM 构建逻辑与原 Replay 分支一致

---

### 改动总结

| 文件 | 变更 | 行数 |
|------|------|------|
| `peri-tui/src/kit/acp_types.rs` | 删除 `ReplayUserBubble`/`ReplayAssistantBubble` 变体，添加 `CommittedAssistantText`，删除对应 decode 分支和测试 | -25 / +5 |
| `peri-tui/src/kit/acp_events.rs` | 删除 Replay* dispatch 分支和测试，添加 `CommittedAssistantText` dispatch | -35 / +15 |
| `peri-tui/src/kit/acp_notifier.rs` | replay 路径映射到 `LocalUserBubble` + `CommittedAssistantText`，更新测试 | -3 / +3 |
| `peri-tui/src/kit/acp_bridge.rs` | 更新 `event_kind_short` 诊断 helper | -2 / +1 |

**净变化**：约 -30 行 / +24 行。无协议变更，无 ACP 层变更。
