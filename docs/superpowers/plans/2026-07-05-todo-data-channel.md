# Todo 数据通道实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 Todo 数据从现有的 ACP `SessionUpdate::Plan` 事件传递到 message_area 的 `render_todo_lines()`。

**Architecture:** 完整链路已就绪：TodoWriteTool → executor todo forwarder → ExecutorEvent::TodoUpdate → mapper → SessionUpdate::Plan → acp_notifier。唯一断点在 acp_notifier 丢弃了 Plan 事件。修复方案：在 acp_notifier 消费 Plan → 写入 atom → message_area 读取 atom。不涉及 Agent 层或 ACP 映射层变更。

**Tech Stack:** Rust 2021 + ratatui-kit atoms

**文件总览：**

| 文件 | 职责 | 变更类型 |
|------|------|---------|
| `peri-tui/src/kit/acp_notifier.rs` | 消费 `SessionUpdate::Plan` → 写入 `TODO_ITEMS` atom | 扩展 |
| `peri-tui/src/kit/atoms.rs` | 新增 `TODO_ITEMS` atom | 扩展 |
| `peri-tui/src/kit/acp_events.rs` | 从 `SessionUpdate::Plan` 解析 `Vec<PlanEntry>` | 扩展 |
| `peri-tui/src/kit/message_area.rs` | 从 atom 读取 todo 数据替代空 `&[]` | 修改 |

**现有数据流（完全可用）：**

```
TodoWriteTool.execute()
  → notify_tx.send(Vec<TodoItem>)           // peri-middlewares/src/tools/todo.rs:183
  → executor todo forwarder                // peri-acp/src/session/executor.rs:1235-1260
  → ExecutorEvent::TodoUpdate(entries)     // peri-agent/src/agent/events.rs:252
  → mapper → SessionUpdate::Plan(plan)     // peri-acp/src/event/mapper.rs:206-229
  → ACP notification → TUI acp_notifier    // ❌ 当前被丢弃
```

---

### Task 1: 新增 `TODO_ITEMS` atom

**Files:**
- Modify: `peri-tui/src/kit/atoms.rs`

- [ ] **Step 1: 添加 TODO_ITEMS atom**

读取 `atoms.rs` 找到现有 atom 定义区域（如 `ACP_STATE`、`RENDER_CACHE`），在附近添加：

```rust
use crate::kit::message_area::TodoItem;

/// Todo 列表数据（来自 SessionUpdate::Plan）
pub static TODO_ITEMS: AtomStatic<Vec<TodoItem>> = AtomStatic::new(|| Vec::new());
```

**重要**：需要确认 `TodoItem` 类型在 message_area.rs 中定义为 `pub`。检查 `peri-tui/src/kit/message_area.rs:55` 附近的 `struct TodoItem` 是否有 `pub` 修饰符。如果是 private，修改为：

```rust
#[derive(Debug, Clone)]
pub struct TodoItem {
    pub status: TodoStatus,
    pub content: String,
}
```

- [ ] **Step 2: 构建验证**

```bash
cargo build -p peri-tui 2>&1 | tail -5
```

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/atoms.rs peri-tui/src/kit/message_area.rs
git commit -m "feat(tui): add TODO_ITEMS atom for Plan data channel"
```

---

### Task 2: acp_notifier 消费 `SessionUpdate::Plan`

**Files:**
- Modify: `peri-tui/src/kit/acp_notifier.rs`
- Modify: `peri-tui/src/kit/acp_events.rs`

- [ ] **Step 1: 在 acp_events 中添加 Plan 解析函数**

在 `peri-tui/src/kit/acp_events.rs` 中添加：

```rust
use agent_client_protocol::schema::{PlanEntry, PlanEntryStatus};

/// 将 ACP PlanEntry 状态转换为 TodoStatus
fn plan_status_to_todo_status(s: &PlanEntryStatus) -> crate::kit::message_area::TodoStatus {
    match s {
        PlanEntryStatus::InProgress => crate::kit::message_area::TodoStatus::InProgress,
        PlanEntryStatus::Pending => crate::kit::message_area::TodoStatus::Pending,
        PlanEntryStatus::Completed => crate::kit::message_area::TodoStatus::Completed,
    }
}

/// 从 SessionUpdate::Plan 中提取 TodoItem 列表，写入 TODO_ITEMS atom
pub fn handle_plan_update(plan: &Plan) {
    let entries: Vec<crate::kit::message_area::TodoItem> = plan
        .entries
        .iter()
        .map(|e| crate::kit::message_area::TodoItem {
            status: plan_status_to_todo_status(&e.status),
            content: e.content.clone(),
        })
        .collect();
    *crate::kit::atoms::TODO_ITEMS.state().write() = entries;
}
```

**重要**：确认 `PlanEntry` / `PlanEntryStatus` / `Plan` 类型来自 `agent_client_protocol::schema`。检查现有 use 语句——`acp_notifier.rs:17-25` 可能有相关 import。如果类型路径不同，使用正确的 import。

- [ ] **Step 2: 在 acp_notifier 的 handle_session_update 中添加 Plan 分支**

读取 `acp_notifier.rs:102-135` 的 `handle_session_update` 函数。当前逻辑：

```rust
fn handle_session_update(params: serde_json::Value) {
    let update: SessionUpdate = match serde_json::from_value(...) { ... };
    match update {
        SessionUpdate::AvailableCommandsUpdate(cmd) => { ... }
        _ => { /* drop */ }
    }
}
```

在 match 的 `_ =>` 之前添加：

```rust
SessionUpdate::Plan(plan) => {
    crate::kit::acp_events::handle_plan_update(&plan);
}
```

- [ ] **Step 3: 更新 use 语句**

在 `acp_notifier.rs` 或 `acp_events.rs` 中添加必要的 import（如果缺失）：

```rust
use agent_client_protocol::schema::{Plan, PlanEntry, PlanEntryStatus, SessionUpdate};
use crate::kit::atoms::TODO_ITEMS;
```

- [ ] **Step 4: 构建和测试**

```bash
cargo build -p peri-tui 2>&1 | tail -5
cargo test -p peri-tui --lib 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/acp_notifier.rs peri-tui/src/kit/acp_events.rs
git commit -m "feat(tui): consume SessionUpdate::Plan in acp_notifier → TODO_ITEMS atom

Parses Plan entries into TodoItem list and writes to TODO_ITEMS atom
for message_area consumption."
```

---

### Task 3: message_area 从 atom 读取 Todo 数据

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs`

- [ ] **Step 1: 用 atom 读取替换空 `&[]`**

在 `MessageArea` 组件函数中（约 line 215-220），将：

```rust
let todo_items: &[TodoItem] = &[];
if !todo_items.is_empty() {
    for line in render_todo_lines(todo_items) {
        all_lines.push(line);
    }
}
```

替换为：

```rust
// ── Todo 列表（从 ACP SessionUpdate::Plan 消费） ──
let todo_atom = hooks.use_atom(&crate::kit::atoms::TODO_ITEMS);
let todo_items = todo_atom.read();
if !todo_items.is_empty() {
    for line in render_todo_lines(&todo_items) {
        all_lines.push(line);
    }
}
```

**重要**：如果 `render_todo_lines` 和 `TodoItem`/`TodoStatus` 不是 `pub`，需要先修改为 `pub`。检查当前定义在 `message_area.rs:48-75` 是否使能外部访问——需要 `pub` 才能让 `atoms.rs` 引用 `TodoItem` 和 `acp_events.rs` 引用 `TodoStatus::InProgress` 等。

- [ ] **Step 2: 构建和测试**

```bash
cargo build -p peri-tui 2>&1 | tail -5
cargo test -p peri-tui --lib 2>&1 | tail -5
```

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/message_area.rs
git commit -m "feat(tui): connect Todo list to TODO_ITEMS atom in message_area

Replaces empty &[] with hooks.use_atom(&TODO_ITEMS) read.
TodoWrite tool output now renders in real-time via existing ACP Plan pipeline."
```

---

## 完成标准

- [ ] `cargo build -p peri-tui` 零错误零 warning
- [ ] `cargo test -p peri-tui --lib` 全部 PASS
- [ ] `TodoWrite` 工具写入后，message_area 实时显示 Todo 图标和文字
- [ ] 空 Todo 列表不渲染额外行
- [ ] 原有 spinner 渲染不受影响

## 现有测试验证

已有 ACP 层集成测试覆盖完整链路：

| 测试 | 文件 | 验证内容 |
|------|------|---------|
| `test_todo_update_maps_to_session_update` | `peri-acp/src/event/mapper_test.rs:319` | TodoUpdate → Plan, 3 条目状态映射 |
| `test_todo_update_empty_entries` | `peri-acp/src/event/mapper_test.rs:360` | 空 TodoUpdate → 空 Plan |
| `test_event_mapper_todo_update_maps_to_plan` | `peri-acp/tests/integration_test.rs:124` | 端到端 TodoUpdate → SessionUpdate::Plan |
