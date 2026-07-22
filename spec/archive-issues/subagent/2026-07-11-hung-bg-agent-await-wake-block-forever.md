# hung bg agent 导致 run_react_loop await_wake 永久阻塞


> 归档于 2026-07-20，原路径 spec/issues/2026-07-11-hung-bg-agent-await-wake-block-forever.md
**状态**：Fixed
**优先级**：中
**类型**：Bug
**创建日期**：2026-07-11

## 问题描述

当 bg agent 因任何原因挂死（hung）而永不完成时，`run_react_loop` 会在 `await_wake` 中永久阻塞，整个 session 无法继续。

此问题以前被 `woken_once` 标志**部分掩盖**——`woken_once` 限制 agent 只进入一次 `await_wake`，一定程度上减少了暴露窗口。但即使用 `woken_once`，如果第一个 bg agent 就 hung 了，agent 同样会在**第一次** `await_wake` 中堵死。`2026-07-11` 修复多 bg 同轮 bug 时移除了 `woken_once`（见 `spec/issues/2026-07-11-bg-multi-agent-loading-freeze-last-callback-lost.md`），使得此问题在更多场景下可触发。

## 症状详情

| 场景 | 期望行为 | 实际行为 |
|------|---------|---------|
| 3 个 bg agent，第 2 个 hung | 处理 bg-1 和 bg-3 的 callback，bg-2 超时后报错或取消 | ❌ 处理完 bg-1 后，agent 在第 2 轮 `await_wake` 永久阻塞——bg-3 的 callback 也永远不会被处理 |
| 1 个 bg agent hung | session 在超时后恢复，hung agent 被取消 | ❌ agent 在第一次 `await_wake` 永久阻塞 |

**关键点**：`await_wake` 阻塞的根源是 `idle_should_wait` probe（`bg_registry.active_count() > 0`）返回 true——有活跃 bg agent 计数——但那个 agent 永远不会完成（不再产生 wake）。`Notify` 机制依赖 "完成 → push_defer → wake"，hung agent 永远不触发这个链。

## 根因分析

`run_react_loop` 中 `await_wake` 是**纯等待**——没有超时机制，没有 bg agent 健康检查。防御链为：

```
queue empty → idle_should_wait (active_count>0) → await_wake → ???
```

`???` 处依赖外部 push_defer 触发 wake。如果所有活跃 bg agent 都 hung，wake 永不发生 → 永久阻塞。

## 涉及文件

- `peri-agent/src/agent/stages/mod.rs:670-711` —— `await_wake` + `tokio::select!` 目前只有 cancel token 作为退出条件
- `peri-middlewares/src/subagent/spawner.rs` —— bg agent spawn + complete 流程
- `peri-middlewares/src/subagent/background.rs` —— `BackgroundTaskRegistry`，`active_count()` 的实现
- `peri-acp/src/session/executor.rs:691-693` —— `idle_should_wait` probe 定义

## 可选的修复方向

1. **bg agent 级超时**：为每个 bg agent 设置最大执行时间，超时后自动 cancel + push_defer 错误结果
2. **await_wake 超时**：`tokio::select!` 中加入 `tokio::time::sleep(MAX_IDLE_DURATION)` 分支，超时后检查 hung agent 并 cancel
3. **BgRegistry 健康检查**：`idle_should_wait` probe 改为 `active_count()` + `last_heartbeat()` 双重检查

## 与已有 issue 的关系

| Issue | 关系 |
|-------|------|
| `2026-07-11-bg-multi-agent-loading-freeze-last-callback-lost.md` | 该修复移除了 `woken_once`，使本 issue 在更多场景下可触发 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-11 | — | Open | agent | 创建 |

## 调查记录

**调查日期**：2026-07-11
**方式**：3 个 bg agent 并行调查（ultra-batch）

### 1. 根因链路

从 `await_wake` 到 `Notify` wake 的完整调用链：

**正常路径**：
```
run_react_loop                    stages/mod.rs:662   inbox.await_wake() 阻塞
    ↓
[bg agent tokio::spawn 正常完成]     spawner.rs:253
    ↓
on_bg_complete(&result)           spawner.rs:338      回调触发（先于 registry.complete）
    ↓
router.route_bg_result(result)    executor.rs:1073    注入到 AcpAgentConfig.on_bg_complete
    ↓
AsyncRouter::route_bg_result      async_router.rs:50  转换为 BaseMessage::human
    ↓
inbox.push_defer(source, msg)     async_router.rs:59  → inbox.rs:146-148
    ↓                                          queue.push(QueuedMessage::defer(...))
    ↓                                          wake.notify_one()  ← 唤醒 await_wake
bg_registry.complete(&task_id)    spawner.rs:352      active_count 从 >0 变为 0
```

**挂死路径**：
```
run_react_loop                    stages/mod.rs:662   inbox.await_wake() 阻塞
    ↓
[bg agent 的 run_react_loop 未返回]  spawner.rs:253      永不完成
    ↓
on_bg_complete 永不被调用           spawner.rs:338      永不触发
    ↓
push_defer / wake.notify_one()    —                    永不发生
    ↓
registry.complete()               —                    active_count 永保持 >0
    ↓
idle_should_wait probe            executor.rs:693     始终返回 true
    ↓
await_wake                        inbox.rs:78-89      永久阻塞
```

**退出条件（挂死时唯一解脱）**：
```
tokio::select!                    stages/mod.rs:661
    ├── inbox.await_wake()        ← hung 时永不返回
    └── cancel_fut                ← 用户主动 cancel 时触发 → LoopResult::Interrupted
```

### 2. active_count() 生命周期

**定义**：`background.rs:147-153` —— `tasks.lock().values().filter(|t| t.status == Running).count()`

| 事件 | 文件:行号 | 效果 |
|------|-----------|------|
| 注册 task（status=Running） | `spawner.rs:371` | `active_count` ↑ |
| `complete()`（正常/错误） | `spawner.rs:289, 352` | `active_count` ↓ |
| 显式 `cancel()` | `background.rs:288-323` | `active_count` ↓ |
| bg agent hung（永不完成） | — | `active_count` **永不递减** |

**挂死场景**：当 `run_react_loop`（`spawner.rs:253`）卡在 LLM 调用、工具执行或中间件 hook 时，tokio task 永生不能执行到 `on_bg_complete` 和 `registry.complete()`。

**关键陷阱**：`registry.complete()` 不触发 wake。wake 完全依赖 `on_bg_complete` → `push_defer` → `wake.notify_one()`，而这在 `complete()` 之前被调用（`spawner.rs:338` 先于 `352`）。

### 3. 现有防御机制（全为零）

| 机制 | 位置 | 评估 |
|------|------|------|
| cancel_fut | `stages/mod.rs:659-660` | 需用户主动操作，且作用在所有会话而非单个 hung agent |
| max_iterations(500) | `stages/mod.rs:521` | 不消耗 iteration（在 `continue` 前 block），永不触达 |
| is_cancelled 每轮检查 | `stages/mod.rs:523` | 在迭代开始处检查，hung 在 await_wake 中不会到下一轮 |
| AbortHandle | `spawner.rs:367` | 仅在显式 `registry.cancel()` 时使用，无自动触发 |
| JoinHandle | `spawner.rs:241` | 未被 await，tokio task panic 不会传播，也不会调用 complete() |

**结论：没有任何超时、心跳或自动 cancel 机制。唯一防御是用户手动 cancel。**

此外 `SessionInbox::await_wake`（`inbox.rs:78-89`）本身是纯 `loop { wake.notified().await }`，无 cancel token 注入：

```rust
pub async fn await_wake(&self) {
    if self.queue.has_wake_up() { return; }
    loop {
        self.wake.notified().await;   // ← 无 cancel token，永远等
        if self.queue.has_wake_up() { return; }
    }
}
```

### 4. woken_once 移除前后对比

**移除前**（有 `woken_once` 守卫）：每 session 最多进入 1 次 `await_wake`，后续 bg agent 结果被丢弃。

**移除后**（当前代码）：可无限次进入，任何一个 bg agent hung 即永久阻塞。

| 场景 | 移除前 | 移除后 |
|------|--------|--------|
| 单 bg hung | 1 次阻塞后退出 | 永久阻塞 |
| 多 bg 同轮，全部正常 | 只等第一个，其他丢失 | ✅ 全部等，正常工作 |
| 多 bg 同轮，第 3 个 hung | 等完第 1 个即退出，第 3 个 lost | 等前 2 个正常，第 3 个永久阻塞 |
| 串行 bg，第 2 个 hung | 第 1 轮等，第 2 轮已退出 | 第 1 轮等完，第 2 轮永久阻塞 |

### 5. 修复方案评估

| 维度 | 方向 1：bg 超时 | 方向 2：await_wake 超时 | 方向 3：心跳检查 |
|------|:---:|:---:|:---:|
| 可行性 | ★★★ 高 | ★★☆ 中 | ★★☆ 中 |
| 安全性 | ★★☆ 中偏高 | ★★☆ 中 | ★★☆ 中偏高 |
| 覆盖度 | ★★☆ 中 | ★★☆ 中 | ★★★ 高 |
| 副作用 | ★★☆ 中 | ★★☆ 中 | ★★★ 低 |
| 改动量 | ~40 行 | ~60 行 | ~80+ 行 |

**推荐：方向 1（主力） + 方向 2 轻量版（兜底）**

| 层级 | 方案 | 阈值 | 作用 |
|------|------|------|------|
| L1 主力 | bg agent 级超时 | 600s | 主动 kill hung agent，触发 complete + wake |
| L2 兜底 | await_wake idle 超时 | 180s | 防止 L1 未生效时 session 永久阻塞 |

**理由**：
- 方向 1 是最直接的根本修复，完全遵循现有 "complete 驱动" 模式
- 方向 3 架构优雅但过度工程化——心跳机制引入持续状态维护成本，对 tokio task 级别故障 `tokio::time::timeout` 更 Rust-idiomatic
- 方向 1+2 组合双重保障：L1 在 bg 层面解决问题，L2 在 session 层面兜底
- 与代码库现有模式契合：工具调用 300s、Bash 120s、MCP 120s 均用 `tokio::time::timeout`

### 6. 推荐实现步骤

**Step 1**：`spawner.rs` 添加 bg agent 超时（~35 行新增）

- 新增 `const MAX_BG_AGENT_DURATION: Duration = Duration::from_secs(600);`
- 包裹 `run_react_loop`：`tokio::time::timeout(MAX_BG_AGENT_DURATION, run_react_loop(...)).await`
- 新增 timeout 分支：cancel token → abort join handle → 构造 error `BackgroundTaskResult` → `on_bg_complete` → `bg_registry.complete()` → `deregister_runtime`

**Step 2**：`stages/mod.rs` 添加 idle 超时（~12 行新增）

- 新增 `const MAX_AWAIT_WAKE_DURATION: Duration = Duration::from_secs(180);`
- `tokio::select!` 加 `biased` 修饰 + `sleep` 分支，超时后 `return LoopResult::Completed`

**Step 3**：回归测试

- 单 bg hung → 600s auto-cancel，session 恢复
- 3 个 bg，第 2 个 hung → bg-1 正常完成，bg-2 超时 auto-cancel，bg-3 正常完成
- idle timeout → 180s 无任何 bg 完成，loop 退出不阻塞

### 7. 影响面

**高风险**：
- `inbox.rs:83-89` — `await_wake` 无超时（**严重**）
- `stages/mod.rs:661` — `tokio::select!` 仅 cancel 可退出（**严重**）
- `spawner.rs:241-294` — 若 `registry.complete()` 之前 panic，任务永久 Running（**中**）

**与已有修复的兼容性**：与 `woken_once` 移除修复（commit `52a42ff4`）无冲突——两者正交。

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
