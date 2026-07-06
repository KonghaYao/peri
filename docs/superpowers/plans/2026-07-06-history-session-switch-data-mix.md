# History 面板 session 切换数据混合修复

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 History 面板切换 session 后消息区出现新旧 session 数据混合的问题。

**Architecture:** 三处修复：① `build_session_view_commit_payload` 移除空检查，确保空 session 也发 ViewCommit 清空 atom；② `acp_bridge` 在 BRIDGE_RESET_COUNTER 复位后立即 `push_view_models` 清空 VIEW_MODELS atom；③ `render_bridge` 同步检测 BRIDGE_RESET_COUNTER 清空 RenderCache。

**Tech Stack:** Rust (peri-acp, peri-tui), ratatui-kit atom system

---

## 致因分析（背景）

切换 session 时的数据流：

```
thread_load_consumer: BRIDGE_RESET_COUNTER++ → load_session RPC
acp_bridge:          检测 counter 变化 → 清空内部 BridgeState（不推 atom！）
ACP Server:          build_session_view_commit_payload() 
                       → history 为空时返回 None（不发送 ViewCommit）
                       → history 非空时返回 ViewCommit → acp_bridge 写入 VIEW_MODELS
```

**缺陷**：

1. 空 session 不发送 ViewCommit → VIEW_MODELS atom 保留旧 committed → render_bridge 读旧数据构建 RENDER_CACHE
2. BRIDGE_RESET_COUNTER 复位后不调 push_view_models → VIEW_MODELS atom 在 ViewCommit 到达前保持旧值
3. render_bridge 不检测 BRIDGE_RESET_COUNTER → 只依赖 atom 指针变化

结果：新旧数据混合在同一个 RENDER_CACHE 中，用户看到的是一片混合内容。

---

## File Structure

| 文件 | 操作 | 职责 |
|------|------|------|
| `peri-acp/src/dispatch/session_load.rs` | 修改 | 移除 `history.is_empty()` 空检查，空 session 也发 ViewCommit（view_models=[]） |
| `peri-tui/src/kit/acp_bridge.rs` | 修改 | BRIDGE_RESET_COUNTER 复位后调用 `push_view_models` 立即清空 VIEW_MODELS atom |
| `peri-tui/src/kit/render_bridge.rs` | 修改 | 检测 BRIDGE_RESET_COUNTER 变化，立即清空 RenderCache 和 msg_hashes/msg_lines_cache |
| `peri-acp/src/dispatch/session_load_test.rs` | 新增 | 测试空 history 时 build_session_view_commit_payload 仍返回 Some |

---

### Task 1: 修复 build_session_view_commit_payload——空 history 也发 ViewCommit

**Files:**
- Modify: `peri-acp/src/dispatch/session_load.rs:40-54`
- Create: `peri-acp/src/dispatch/session_load_test.rs`

- [ ] **Step 1: 修改 build_session_view_commit_payload**

将 `peri-acp/src/dispatch/session_load.rs` 第 40-54 行改为：

```rust
/// Build the `peri/unstable-event` payload for a "view-commit" event
/// from the loaded session history.
///
/// Converts `history` to `Vec<ViewModel>` via a fresh `ViewMapperImpl`
/// and returns a `{ sessionId, event, data }` JSON payload suitable for
/// sending through the transport's `send_notification()` method.
///
/// Always returns Some——even for empty history. An empty (but present)
/// ViewCommit is required by the TUI bridge to clear stale VIEW_MODELS
/// from the previous session. Skipping it leaves old data visible.
pub fn build_session_view_commit_payload(
    session_id: &str,
    history: &[BaseMessage],
) -> Option<serde_json::Value> {
    let vms = if history.is_empty() {
        Vec::new()
    } else {
        let mut vm = crate::event::ViewMapperImpl::new();
        vm.convert(history)
    };
    Some(json!({
        "sessionId": session_id,
        "event": "view-commit",
        "data": { "view_models": vms },
    }))
}
```

与旧版 diff：移除了 `if history.is_empty() { return None }` 的分支，空 history 时直接用 `Vec::new()` 作为 view_models。

- [ ] **Step 2: 编译验证**

```bash
cargo build -p peri-acp 2>&1
```
Expected: PASS（无编译错误）

- [ ] **Step 3: 新增单元测试文件 peri-acp/src/dispatch/session_load_test.rs**

```rust
//! 测试 session_load 模块的 build_session_view_commit_payload 函数。

use super::build_session_view_commit_payload;
use peri_agent::messages::BaseMessage;

fn make_human_message(text: &str) -> BaseMessage {
    BaseMessage::human(text.to_string())
}

#[test]
fn 测试空history应返回包含空view_models的view_commit() {
    let history: Vec<BaseMessage> = vec![];
    let result = build_session_view_commit_payload("test-session", &history);
    assert!(result.is_some(), "空 history 也应返回 Some（空 view_models），以便 TUI bridge 清空旧数据");
    let payload = result.unwrap();
    assert_eq!(payload["sessionId"], "test-session");
    assert_eq!(payload["event"], "view-commit");
    assert_eq!(payload["data"]["view_models"].as_array().unwrap().len(), 0);
}

#[test]
fn 测试有history时应正常转换view_models() {
    let history = vec![
        make_human_message("hello"),
        BaseMessage::ai("hi there"),
    ];
    let result = build_session_view_commit_payload("test-session", &history);
    assert!(result.is_some());
    let payload = result.unwrap();
    let vms = payload["data"]["view_models"].as_array().unwrap();
    assert!(!vms.is_empty(), "非空 history 应有 ViewModel 输出");
}
```

- [ ] **Step 4: 在 session_load.rs 末尾添加测试模块声明**

在 `peri-acp/src/dispatch/session_load.rs` 末尾追加：

```rust
#[cfg(test)]
#[path = "session_load_test.rs"]
mod tests;
```

- [ ] **Step 5: 运行测试**

```bash
cargo test -p peri-acp --lib session_load_test 2>&1
```
Expected: PASS（两个测试通过）

- [ ] **Step 6: Commit**

```bash
git add peri-acp/src/dispatch/session_load.rs peri-acp/src/dispatch/session_load_test.rs
git commit -m "fix(acp): 空 session history 也发送 ViewCommit 通知，清空 TUI stale 数据"
```

---

### Task 2: acp_bridge——BRIDGE_RESET_COUNTER 复位后立即推送空快照

**Files:**
- Modify: `peri-tui/src/kit/acp_bridge.rs:46-59`

- [ ] **Step 1: 修改 acp_bridge.rs 的 reset 逻辑**

将 `peri-tui/src/kit/acp_bridge.rs` 第 46-59 行替换为：

```rust
                            let counter = atoms::BRIDGE_RESET_COUNTER.get();
                            if counter != last_reset_counter {
                                last_reset_counter = counter;
                                state.committed = Arc::from([]);
                                state.current_turn.reset();
                                state.has_view_commit = false;
                                state.is_loading = false;
                                state.popup_kind = None;
                                // 立即推送空快照到 VIEW_MODELS atom——
                                // 防止 render_bridge 在下一次事件到达前读到旧数据。
                                acp_events::push_view_models_for_reset();
                                tracing::info!(
                                    old = last_reset_counter,
                                    new = counter,
                                    "bridge: state reset by BRIDGE_RESET_COUNTER"
                                );
                            }
```

需要在 `peri-tui/src/kit/acp_events.rs` 中新增一个公开函数 `push_view_models_for_reset`：

```rust
/// 由 acp_bridge 在 BRIDGE_RESET_COUNTER 复位时调用——
/// 立即将空快照写入 VIEW_MODELS atom，防止其他 reader 读到旧 session 数据。
pub fn push_view_models_for_reset() {
    let snapshot = ViewModelsSnapshot {
        committed: Arc::from([]),
        current_turn: Arc::from([]),
    };
    *VIEW_MODELS.state().write() = snapshot;
}
```

- [ ] **Step 2: 编译验证**

```bash
cargo build -p peri-tui 2>&1
```
Expected: PASS

- [ ] **Step 3: 运行已有测试确保无回归**

```bash
cargo test -p peri-tui --lib 2>&1
```
Expected: PASS（所有已有测试通过）

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/kit/acp_bridge.rs peri-tui/src/kit/acp_events.rs
git commit -m "fix(tui): BRIDGE_RESET_COUNTER 复位后立即推送空 VIEW_MODELS 到 atom"
```

---

### Task 3: render_bridge——检测 BRIDGE_RESET_COUNTER 变化并清空缓存

**Files:**
- Modify: `peri-tui/src/kit/render_bridge.rs:51-163`

- [ ] **Step 1: 在 render_bridge 的事件循环开头添加 reset 检测**

修改 `peri-tui/src/kit/render_bridge.rs`，在 `spawn_render_bridge` 的循环体中，`Some(_event) = rx.recv()` 分支开头添加 BRIDGE_RESET_COUNTER 检测：

在 `per-tui/src/kit/render_bridge.rs` 第 81 行之后插入：

```rust
                Some(_event) = rx.recv() => {
                    // 检测 BRIDGE_RESET_COUNTER——acp_bridge 已清空 VIEW_MODELS，
                    // render_bridge 同步清空缓存，避免用旧数据重建 RENDER_CACHE。
                    let counter = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
                    if counter != last_reset_counter {
                        last_reset_counter = counter;
                        cache = RenderCache::default();
                        msg_hashes.clear();
                        msg_lines_cache.clear();
                        last_committed_ptr = 0;
                        last_committed_len = 0;
                        last_ct_ptr = 0;
                        *RENDER_CACHE.state().write() = cache.clone();
                        info!("render_bridge: cache cleared by BRIDGE_RESET_COUNTER");
                    }

                    let Some(snapshot) = read_ready_snapshot(last_committed_ptr, last_committed_len, last_ct_ptr).await else {
```

需要在函数开头新增追踪变量（在 `let mut last_ct_ptr: usize = 0;` 之后）：

```rust
        let mut last_reset_counter: u64 = 0;
```

- [ ] **Step 2: 编译验证**

```bash
cargo build -p peri-tui 2>&1
```
Expected: PASS

- [ ] **Step 3: 运行已有测试确保无回归**

```bash
cargo test -p peri-tui --lib 2>&1
```
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/kit/render_bridge.rs
git commit -m "fix(tui): render_bridge 检测 BRIDGE_RESET_COUNTER 变化同步清空缓存"
```

---

### Task 4: 端到端验证

**Files:** （无）

- [ ] **Step 1: 全量编译**

```bash
cargo build --workspace 2>&1
```
Expected: PASS

- [ ] **Step 2: 全量测试**

```bash
cargo test --workspace 2>&1
```
Expected: PASS（所有测试通过，无新增失败）

- [ ] **Step 3: 手动验证场景**

启动 TUI：
```bash
cargo run -p peri-tui
```

验证步骤：
1. 在 session A 中发送几条消息（确保有工具调用，产生多轮对话）
2. Ctrl+B 打开 History 面板
3. 选择一个有消息的 session B，按 Enter
4. → 预期：消息区仅显示 session B 的消息，无 session A 残留
5. 再切换到 session A
6. → 预期：消息区仅显示 session A 的消息
7. 切换到空 session（如果有）
8. → 预期：消息区清空，无任何残留消息

**Expected:** 每次切换，消息区仅显示目标 session 的内容，无混合。

- [ ] **Step 4: Commit**

```bash
# 如有任何问题修复，追加 commit
git commit --allow-empty -m "verification: history session switch data mix fix verified"
```
