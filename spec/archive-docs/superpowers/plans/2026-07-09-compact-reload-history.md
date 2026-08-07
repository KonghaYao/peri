# Compact 后清空历史并 session/load 重放

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** compact 完成后，TUI 清空消息区，通过 session/load 重放压缩后的历史消息。

**Architecture:** 利用现有 THREAD_LOAD_TX 基础设施。CompactCompleted 事件在 BridgeState 中设 flag，TurnDone 事件检测 flag + current_turn.is_empty() 后通过 THREAD_LOAD_TX 触发 thread_load_consumer，后者调用 acp_client.load_session() 重放压缩后历史。ACP 服务端无需改动（compact 后 ThreadStore 已更新）。

**Tech Stack:** Rust, ratatui-kit atom system, ACP transport

---

## 背景

compact 完成后，ACP 服务端已通过 `prompt.rs:201-226` 将压缩后的消息持久化到 ThreadStore。但 TUI 的 BridgeState 仍持有 compact 前的旧 committed，消息区显示不更新。

agent 内部 compact（ReAct 循环中自动触发）也会发 CompactCompleted 事件，但不应触发 reload。区分方式：命令 compact（Immediate）后 current_turn 为空，agent 内部 compact 后 current_turn 有流式事件内容。

## 改动文件

- **Modify**: `peri-tui/src/kit/acp_events.rs`（BridgeState 结构体 + 2 处 match 分支 + 测试构造函数）
- **Modify**: `peri-tui/src/kit/acp_bridge.rs`（BridgeState 构造函数）

---

### Task 1: 在 BridgeState 中添加 `compact_just_completed` 字段

**Files:**
- Modify: `peri-tui/src/kit/acp_events.rs:29-46`
- Modify: `peri-tui/src/kit/acp_bridge.rs:48-56`
- Modify: `peri-tui/src/kit/acp_events.rs`（8 处测试构造函数，约 820/971/1023/1071/1114/1200/1233/1269 行附近）

- [ ] **Step 1: 在 BridgeState struct 中添加字段**

在 `acp_events.rs:45` `active_session_id` 字段后添加：

```rust
/// `/compact` 命令刚刚完成，TurnDone 时需触发 session/load 重放。
/// 与 agent 内部 compact 区分：命令 compact 后 current_turn 为空，
/// agent 内部 compact 后 current_turn 有后续流事件。
pub compact_just_completed: bool,
```

- [ ] **Step 2: 更新 acp_bridge.rs 中的 BridgeState 构造函数**

在 `acp_bridge.rs:56` `active_session_id: String::new(),` 后添加：

```rust
compact_just_completed: false,
```

- [ ] **Step 3: 更新测试中所有 BridgeState 构造函数**

在 `acp_events.rs` 的 8 处测试构造函数中，每个 `BridgeState { ... }` 的最后一个字段后添加：

```rust
compact_just_completed: false,
```

提示：搜索 `let mut state = BridgeState {` 定位所有构造函数。

- [ ] **Step 4: 编译确认无遗漏**

```bash
cargo build -p peri-tui 2>&1 | grep "missing field"
```

Expected: 无输出（所有构造函数已更新）

---

### Task 2: 在 CompactCompleted 分支设置 flag

**Files:**
- Modify: `peri-tui/src/kit/acp_events.rs:449-454`

- [ ] **Step 1: 修改 CompactCompleted 处理逻辑**

将 `acp_events.rs:449-454`：

```rust
CompactCompleted { summary, .. } => {
    tracing::info!(summary_len = summary.len(), "bridge: CompactCompleted");
    state.phase = SessionPhase::Idle;
    ACP_STATE.state().write().is_loading = false;
    push_acp_state(state);
}
```

改为：

```rust
CompactCompleted { summary, .. } => {
    tracing::info!(summary_len = summary.len(), "bridge: CompactCompleted");
    state.compact_just_completed = true;
    state.phase = SessionPhase::Idle;
    ACP_STATE.state().write().is_loading = false;
    push_acp_state(state);
}
```

- [ ] **Step 2: 编译确认**

```bash
cargo build -p peri-tui
```

Expected: 编译通过

---

### Task 3: 在 TurnDone 分支检测并触发 reload

**Files:**
- Modify: `peri-tui/src/kit/acp_events.rs:205-239`

- [ ] **Step 1: 在 TurnDone 末尾添加 reload 触发逻辑**

在 `acp_events.rs:238` `drain_input_buffer();` 之后，`}` 闭合之前，插入：

```rust
// C2: compact 命令完成后触发 session/load 重放。
// 区分 agent 内部 compact：命令 compact 后无后续流事件，
// current_turn 为空；agent 内部 compact 后 current_turn 有内容。
if state.compact_just_completed && state.current_turn.is_empty() {
    state.compact_just_completed = false;
    if let Some(tx) = THREAD_LOAD_TX.get() {
        let session_id = state.active_session_id.clone();
        tracing::info!(
            session_id = %session_id,
            "TurnDone: compact completed, triggering session/load replay"
        );
        let _ = tx.send(session_id);
    }
}
```

- [ ] **Step 2: 编译确认**

```bash
cargo build -p peri-tui
```

Expected: 编译通过

---

### Task 4: 编写测试

**Files:**
- Modify: `peri-tui/src/kit/acp_events.rs`（测试模块末尾追加）

- [ ] **Step 1: 编写测试——命令 compact 后 TurnDone 触发 THREAD_LOAD_TX**

在测试模块末尾（`mod tests { ... }` 闭合前）追加：

```rust
#[test]
#[serial]
fn test_compact_turndone_triggers_thread_load_tx() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();

    // 设置 THREAD_LOAD_TX 接收端
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let _ = THREAD_LOAD_TX.set(tx);

    let mut state = BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
        popup_kind: None,
        generation: 0,
        active_session_id: "test-session".to_string(),
        compact_just_completed: true, // CompactCompleted 已设
    };

    // TurnDone 到达：compact_just_completed=true + current_turn 为空 → 触发 reload
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnDone,
    );

    // THREAD_LOAD_TX 应收到 session_id
    let received = rx.try_recv().ok();
    assert_eq!(received.as_deref(), Some("test-session"));
    // flag 应被清除
    assert!(!state.compact_just_completed);
}
```

- [ ] **Step 2: 编写测试——agent 内部 compact 后 TurnDone 不触发 reload**

```rust
#[test]
#[serial]
fn test_agent_compact_turndone_does_not_trigger_reload() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let _ = THREAD_LOAD_TX.set(tx);

    // agent 内部 compact：CompactCompleted 后仍有流事件（TextChunk），
    // 所以 current_turn 非空
    let mut state = BridgeState {
        variant: 1,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::PromptRunning,
        popup_kind: None,
        generation: 0,
        active_session_id: "test-session".to_string(),
        compact_just_completed: false,
    };
    // 模拟 agent 内部 compact 后的 TextChunk（清除 flag）
    state.current_turn.append_text("agent response", None);
    // 重新设 flag（模拟事件乱序）
    state.compact_just_completed = true;

    // TurnDone 到达：compact_just_completed=true 但 current_turn 非空 → 不触发
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnDone,
    );

    assert!(rx.try_recv().is_err()); // THREAD_LOAD_TX 未收到消息
}
```

- [ ] **Step 3: 运行测试**

```bash
cargo test -p peri-tui --lib -- test_compact_turndone_triggers_thread_load_tx test_agent_compact_turndone_does_not_trigger_reload
```

Expected: 2 tests PASS

---

### Task 5: 运行全量测试 + clippy

- [ ] **Step 1: 运行 peri-tui 全量测试**

```bash
cargo test -p peri-tui --lib
```

Expected: 全部通过，无回归

- [ ] **Step 2: 运行 clippy**

```bash
cargo clippy -p peri-tui --lib -- -D warnings
```

Expected: 无新增警告

- [ ] **Step 3: 运行 workspace 全量测试（验证跨 crate 无影响）**

```bash
cargo test --workspace
```

Expected: 全部通过（ACP 侧无改动，不应有回归）

---

### Task 6: Commit

- [ ] **Step 1: Commit**

```bash
git add peri-tui/src/kit/acp_events.rs peri-tui/src/kit/acp_bridge.rs
git commit -m "feat(tui): compact 完成后通过 session/load 重放压缩历史

CompactCompleted 事件在 BridgeState 设 compact_just_completed flag。
TurnDone 检测 flag + current_turn.is_empty() 后通过 THREAD_LOAD_TX
触发 session/load 重放，清空消息区并重建压缩后的 ViewModels。

与 agent 内部 compact 的区分：命令 compact（Immediate）后无后续流
事件，current_turn 为空；agent ReAct 循环内 compact 后有 TextChunk
等流事件，current_turn 非空，不触发 reload。

ACP 服务端无需改动——compact 后 ThreadStore 已在 prompt.rs:201-226
更新为新消息。"
```

---

## 自检查

**1. Spec coverage:** ✅ 单一功能，所有需求由 Task 1-4 覆盖。

**2. Placeholder scan:** ✅ 无 TBD/TODO/placeholder。

**3. Type consistency:** ✅ `compact_just_completed: bool` 在 struct、构造函数、测试中统一。
