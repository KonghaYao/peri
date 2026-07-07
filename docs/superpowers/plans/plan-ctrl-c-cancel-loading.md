# Plan: Ctrl+C 在 Loading 中应中断 Agent 而非退出

**Issue**: [spec/issues/2026-07-07-ctrl-c-exits-during-loading.md](../../../spec/issues/2026-07-07-ctrl-c-exits-during-loading.md)  
**Created**: 2026-07-07  
**Status**: draft

---

## 0. 一句话目标

Loading 状态下首次按 `Ctrl+C` 发送 cancel 给 Agent（而非启动退出倒计时），空闲状态下保持原有双击退出行为。

## 1. 问题诊断（仅描述代码行为，非根因分析）

**当前 `event_handlers.rs:46-70` 的行为**：

| 状态 | 首次 Ctrl+C | 1 秒内第二次 | 1 秒后再次 |
|------|------------|-------------|-----------|
| 空闲 | 设 `QUIT_PENDING_SINCE`，显示通知 | `exit(())` | 重新设 timer |
| Loading | 清空 `INPUT_BUFFER`，设 `QUIT_PENDING_SINCE`，显示通知 | `exit(())` | 重新设 timer |

`loading = ACP_STATE.state().read().is_loading` 被读取后仅用于清空 `INPUT_BUFFER`，**从未触发 agent cancel**。因此 loading 中 Ctrl+C 走的是和空闲完全相同的退出倒计时路径。

**对比 peri-main 旧版行为（`event/keyboard/normal_keys.rs:270-340`）**：
旧版有三个独立的优先级分支：有文本时清空 → loading 时 `cancel_current()` → 空闲时退出倒计时。kit 迁移时丢失了 cancel 分支。

**已有基础设施**：
- `AcpTuiClient::cancel_current(session_id)` 已实现（`acp_client/client.rs:461-467`），发送 `session/cancel` ACP notification
- ACP Server `notify.rs:24-31` 已处理 `session/cancel` → `token.cancel()`
- 三阶段 submit/rewind/thread_load consumer 模式已在 `entry.rs` 中建立

## 2. 修改范围（4 个文件，约 60 行净增）

### 2.1 `peri-tui/src/kit/atoms.rs` — 新增 CANCEL_TX 通道

**变更**：在 `SUBMIT_TX` 旁边（约 line 180）新增：

```rust
pub static CANCEL_TX: OnceLock<UnboundedSender<String>> = OnceLock::new();
```

类型为 `UnboundedSender<String>`，传 session_id。与 `SUBMIT_TX`/`REWIND_ACTION_TX` 模式一致。

### 2.2 `peri-tui/src/kit/entry.rs` — 启动 cancel_consumer

**变更**：在 submit/rewind/thread_load 通道创建区域（约 line 108-140）新增：

```rust
// 4a2. CANCEL channel：event_handlers → cancel_consumer
let (cancel_tx, cancel_rx) = mpsc::unbounded_channel::<String>();
let _ = atoms::CANCEL_TX.set(cancel_tx);
```

在 submit_consumer spawn 之后新增：

```rust
let _cancel_handle =
    spawn_cancel_consumer(client.clone(), cancel_rx, shutdown.clone());
```

### 2.3 `peri-tui/src/kit/submit_consumer.rs` — 新增 `spawn_cancel_consumer`

**变更**：在同一文件末尾（与 submit/rewind consumer 同文件，减少模块碎片）新增函数：

```rust
pub fn spawn_cancel_consumer(
    client: AcpTuiClient,
    mut rx: UnboundedReceiver<String>,
    mut shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                Some(session_id) = rx.recv() => {
                    if let Err(e) = client.cancel_current(&session_id).await {
                        tracing::warn!(%e, "cancel_consumer: cancel_current 失败");
                    }
                }
                else => break,
            }
        }
    })
}
```

需要新增 import：`use crate::acp_client::client::AcpTuiClient;`

### 2.4 `peri-tui/src/kit/event_handlers.rs` — 修复 Ctrl+C 优先级

**变更**：替换 `register_global_handlers` 中 `GlobalShortcut::Quit` 分支（约 line 47-70）的逻辑。

当前逻辑：

```rust
Some(GlobalShortcut::Quit) => {
    let loading = ACP_STATE.state().read().is_loading;
    let now = std::time::Instant::now();
    let pending = *QUIT_PENDING_SINCE.state().read();
    match pending {
        None => {
            if loading {
                INPUT_BUFFER.state().write().clear();  // ← 只清 buffer，不 cancel
            }
            *QUIT_PENDING_SINCE.state().write() = Some(now);
            show_quit_pending_notification(now);
            info!("再次按 Ctrl+C 退出");
        }
        // ... exit / reset branches
    }
    EventResult::Consumed
}
```

新逻辑：

```rust
Some(GlobalShortcut::Quit) => {
    let loading = ACP_STATE.state().read().is_loading;
    let pending = *QUIT_PENDING_SINCE.state().read();

    if loading {
        // loading 状态：每次 Ctrl+C 都发送 cancel（不累积退出倒计时）
        *QUIT_PENDING_SINCE.state().write() = None;
        // 尝试发送 cancel 通知
        if let Some(tx) = atoms::CANCEL_TX.get() {
            let client = atoms::ACP_CLIENT_HANDLE.get();
            if let Some(sid) = client.and_then(|c| c.current_session_id()) {
                let _ = tx.send(sid);
                show_cancel_notification(std::time::Instant::now());
            }
        }
        EventResult::Consumed
    } else {
        // 空闲状态：保持原有双击退出逻辑
        let now = std::time::Instant::now();
        match pending {
            None => {
                *QUIT_PENDING_SINCE.state().write() = Some(now);
                show_quit_pending_notification(now);
                info!("再次按 Ctrl+C 退出");
            }
            Some(t) if now.duration_since(t) < std::time::Duration::from_secs(1) => {
                *QUIT_PENDING_SINCE.state().write() = None;
                exit(());
            }
            Some(_) => {
                *QUIT_PENDING_SINCE.state().write() = Some(now);
                show_quit_pending_notification(now);
            }
        }
        EventResult::Consumed
    }
}
```

等等，这里有问题。event_handlers 是闭包注册到 ratatui-kit hooks 中的，它不能直接访问 `AcpTuiClient`。需要加一个 ACP_CLIENT_HANDLE 来暴露 session_id。

**重新设计**：取消消费者的参数改为 `()`：无需传递 session_id，由 consumer 内部从 client 获取。

```rust
// atoms.rs
pub static CANCEL_TX: OnceLock<UnboundedSender<()>> = OnceLock::new();

// entry.rs
let (cancel_tx, cancel_rx) = mpsc::unbounded_channel::<()>();
let _ = atoms::CANCEL_TX.set(cancel_tx);
let _cancel_handle = spawn_cancel_consumer(client.clone(), cancel_rx, shutdown.clone());

// submit_consumer.rs
pub fn spawn_cancel_consumer(
    client: AcpTuiClient,
    mut rx: UnboundedReceiver<()>,
    mut shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                Some(()) = rx.recv() => {
                    if let Some(sid) = client.current_session_id() {
                        if let Err(e) = client.cancel_current(&sid).await {
                            tracing::warn!(%e, "cancel_consumer: cancel_current 失败");
                        }
                    }
                }
                else => break,
            }
        }
    })
}
```

event_handlers 中的逻辑简化为：

```rust
if loading {
    *QUIT_PENDING_SINCE.state().write() = None;
    if let Some(tx) = atoms::CANCEL_TX.get() {
        let _ = tx.send(());
        show_cancel_notification(Instant::now());
    }
} else {
    // 原有双击退出逻辑
}
```

这样 event_handlers 完全不需要知道 AcpTuiClient 的存在。

还需新增 `show_cancel_notification` 函数：

```rust
fn show_cancel_notification(now: std::time::Instant) {
    *NOTIFICATION.state().write() = Some(Notification {
        message: "已发送取消请求".to_string(),
        until: now + std::time::Duration::from_secs(2),
    });
}
```

### 2.5 `peri-tui/src/kit/mod.rs` — 注册模块

`spawn_cancel_consumer` 与 `spawn_submit_consumer` 同属 `submit_consumer` 模块，无需变更 mod.rs。

### 2.6 `peri-tui/src/kit/event_handlers.rs` — 测试级变更

在现有 `mod tests` 中新增单元测试：

```rust
#[test]
fn test_ctrl_c_during_loading_does_not_set_quit_pending() {
    setup_atoms();
    // Set loading = true
    *ACP_STATE.state().write() = AcpStateSnapshot { is_loading: true, ..Default::default() };
    assert!(QUIT_PENDING_SINCE.state().read().is_none());
    // (实际测试需构造 key event → 验证 QUIT_PENDING_SINCE 仍为 None)
}

#[test]
fn test_ctrl_c_during_idle_sets_quit_pending() {
    setup_atoms();
    *ACP_STATE.state().write() = AcpStateSnapshot { is_loading: false, ..Default::default() };
    // (实际测试需构造 key event → 验证 QUIT_PENDING_SINCE 被设置)
}

#[test]
fn test_ctrl_c_double_idle_exits() {
    // (验证 1 秒内双击退出)
}
```

注意：由于 `register_global_handlers` 使用闭包注册到 ratatui-kit hooks 中，直接单元测试事件处理逻辑需要**提取纯函数**。实现时先重构：

```rust
/// 提取 Ctrl+C 行为判定为纯函数，便于单元测试
fn determine_ctrl_c_action(
    loading: bool,
    quit_pending: Option<Instant>,
    now: Instant,
) -> CtrlCAction {
    if loading {
        CtrlCAction::Cancel
    } else {
        match quit_pending {
            None => CtrlCAction::FirstQuit,
            Some(t) if now.duration_since(t) < Duration::from_secs(1) => CtrlCAction::Quit,
            Some(_) => CtrlCAction::FirstQuit,
        }
    }
}

enum CtrlCAction {
    Cancel,
    FirstQuit,
    Quit,
}
```

闭包内改为：

```rust
match determine_ctrl_c_action(loading, *QUIT_PENDING_SINCE.state().read(), Instant::now()) {
    CtrlCAction::Cancel => { /* send CANCEL_TX, clear quit_pending */ }
    CtrlCAction::FirstQuit => { /* set quit_pending, show notification */ }
    CtrlCAction::Quit => { exit(()); }
}
```

## 3. 测试清单

| # | 测试 | 类型 | 验证点 |
|---|------|------|--------|
| 1 | `test_determine_ctrl_c_action_loading` | 单元 | loading=true → `Cancel`，不设 `QUIT_PENDING_SINCE` |
| 2 | `test_determine_ctrl_c_action_idle_first` | 单元 | loading=false + pending=None → `FirstQuit` |
| 3 | `test_determine_ctrl_c_action_idle_double` | 单元 | loading=false + pending=recent → `Quit` |
| 4 | `test_determine_ctrl_c_action_idle_expired` | 单元 | loading=false + pending=old → `FirstQuit` |
| 5 | `test_cancel_consumer_sends_session_cancel` | 集成 | 发送 () 到 CANCEL_TX → client.cancel_current 被调用 |
| 6 | 手动验证 | E2E | TUI 中 loading 时 Ctrl+C → 显示"已发送取消请求" + agent 停止 |

## 4. 实现步骤（TDD 顺序）

| 步骤 | 内容 | 验证方式 |
|------|------|----------|
| Step 1 | `atoms.rs` 新增 `CANCEL_TX` | `cargo check -p peri-tui` |
| Step 2 | `submit_consumer.rs` 新增 `spawn_cancel_consumer` | `cargo check -p peri-tui` |
| Step 3 | `entry.rs` 创建 CANCEL channel + spawn cancel_consumer | `cargo check -p peri-tui` |
| Step 4 | `event_handlers.rs` 提取 `determine_ctrl_c_action` 纯函数 | `cargo test -p peri-tui --lib` |
| Step 5 | `event_handlers.rs` 新增单元测试 | `cargo test -p peri-tui --lib -- test_determine_ctrl_c_action` |
| Step 6 | `event_handlers.rs` 实现 loading cancel 分支 | `cargo test -p peri-tui --lib` |
| Step 7 | 全量回归 | `cargo build -p peri-tui && cargo test -p peri-tui --lib` |

## 5. 边界条件

| 场景 | 行为 |
|------|------|
| Loading + Ctrl+C（首次） | 发送 cancel，提示"已发送取消请求"，不设 QUIT_PENDING |
| Loading + Ctrl+C（连续） | 每次发送 cancel（幂等安全） |
| Loading + Ctrl+C 后 idle | Agent cancel 完成后 ACP_STATE.is_loading→false，此时 Ctrl+C 走空闲逻辑 |
| 空闲 + Ctrl+C → 1s 内 Ctrl+C | 退出（行为不变） |
| 空闲 + Ctrl+C → 1s 后 Ctrl+C | 重置 timer（行为不变） |
| CANCEL_TX 未初始化（acp_client=None） | `tx.send(())` 被跳过，无副作用 |
| cancel_current 失败（网络/服务端错误） | warn 日志，不影响用户体验 |

## 6. 回滚方案

如果 CANCEL_TX 模式有问题，备选方案是在 `event_handlers` 注册时传入 `AcpTuiClient` clone：

```rust
pub fn register_global_handlers(hooks: &mut Hooks, exit: Handler<'static, ()>, client: Option<AcpTuiClient>) {
    let client_clone = client.map(|c| c.clone());
    hooks.use_event_handler(..., move |event| {
        // 闭包内直接调用 client_clone.cancel_current()
    });
}
```

但这会使 event_handlers 签名变重，且与现有 atom-based channel 模式不一致。首选 CANCEL_TX 方案。

---

*最后更新：2026-07-07*
