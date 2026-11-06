# P1：Stop 持久化阻塞同连接后续帧

**状态**：已确认，待修复。
**优先级**：P1。
**类型**：回归（性能 / 响应性）。
**创建日期**：2026-11-06。
**来源**：用户提供的性能扫描报告（`peri-performance-scan-20261106`）。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `fbb1d8ac0`）。

## 确认证据

- TUI 的 Stop 是通知，不是请求：
  `peri-tui/src/acp_client/client/requests.rs:349-351` 最后一步
  `send_notification("session/cancel", params)`。
- 收包循环内联等待通知处理：
  `peri-acp/src/host/server_loop.rs:62-64` 的 `Notification` 分支直接
  `self.dispatch_notification(method, params).await`，没有 spawn；同一连接下一条
  `recv()` 必须等它返回。对比 Request 分支的 `spawn_*`（`server_loop.rs:34-60`）。
- 锁外持久化仍在收包循环调用栈里逐 thread 等待：
  `peri-acp/src/host/server_loop.rs:988-994` 在释放 sessions 锁后内联
  `persist_notification_effects(...).await`；`peri-acp/src/host/notify.rs:232-246`
  对 `stop_targets` 逐项 `set_work_state(Stopped)`，无超时、无合并、无队列。
- 底层写入的真实阻塞面：本机 SQLite
  `peri-resources/src/sessions/sqlite_store/session_rows.rs:139-146,157-161`
  （`pool.acquire()` + 无条件 `UPDATE`）；远端
  `peri-resources/src/sessions/remote/session_write.rs:369-385,409-429,444-460`
  每次调用独立铸造 identity 并提交；单次远程预算为
  `peri-resources/src/sessions/remote/connection.rs:22-23` 的 20s，
  `remote/mutation.rs:35,333-353` 逐调用包住。N 个目标的串行总时长按调用结构
  是逐项之和，不是整条 Stop 一个 20s。
- 无活跃 token 的合法 Stop 仍收集写入目标：
  `peri-acp/src/host/notify.rs:80-95` 在 `cancel_token` 检查之前 push 本会话与
  cascade children，因此新增的写等待可独立触发。
- 扫描机制实验（临时 WAL SQLite，非 Peri 端到端）：并发 SELECT 0.337ms 返回，
  `UPDATE` 等待写锁 831.695ms 后完成。只能证明写锁机制，不构成生产延迟测量。

## 根因链

1. 用户按 Stop，TUI 发送 `session/cancel` 通知（观察：`requests.rs:349-351`）。
2. `ServerLoop` 收包循环内联处理该通知；内存取消（token、级联 child、
   continuation 代际）先完成，随后在**同一个调用栈**里逐 thread 写
   `work_state = stopped`（观察：`server_loop.rs:62-64,988-994`、
   `notify.rs:232-246`）。
3. 存储写入受 SQLite 写锁或远端调用预算支配；同会话与级联 child 逐个串行等待
   （观察：`session_rows.rs:139-161`、`session_write.rs:369-460`）。
4. 用户可见后果：该 Stop 持久化完成前，同一连接的新请求/通知不会被分派；
   DB 争用时表现为 Stop 之后新 prompt、`session/list` 等一并延迟（推断自第 1–3
   步的调用结构；未在真实终端做前后延迟测量）。
5. 附加约束：任何把持久化移出收包循环的修复都必须保证迟到的 `stopped` 不覆盖
   下一轮准入写入的 `working`。当前两边都没有进程内代次守卫：准入写
   `peri-acp/src/host/prompt_dispatch.rs:355-373` 不读 `work_state`、不校验代次；
   Stop 写（`notify.rs:232-246`）不比对任何代次；SQL 也无条件
   （`session_rows.rs:157-161`）。现有 `continuation_epoch` 只覆盖内部续跑准入
   （`prompt_dispatch.rs:204-315`），用户新 prompt 为 `None`。

## 影响面

- 用户影响：存储争用/远端慢时，Stop 会连带阻塞同连接后续交互；Stop 之后立即
  继续对话可能表现为卡顿。
- 受影响模块：`peri-acp` 通知分派与 Stop 持久化、`peri-resources` 写入路径的
  调用结构（不修改存储实现）。
- 边界：已分派任务与其他物理连接不受影响；受影响的只是同连接新收到的帧。
- 是否可规避：等待持久化完成；或换新连接继续操作。

## 修复方案

- 取消即时生效保持不变；`work_state`/占用快照持久化移出收包循环，交给
  host-owned task（新增 `HostTaskKind::NotificationEffect`），收包循环不再等待。
- 顺序与代次：
  - 取消通知在 sessions 锁内捕获每个目标的**意图代次**
    （`SessionState::work_intent_epoch`）。
  - 持久化任务按 thread 取得该会话的 **prompt 锁**（与 prompt 准入同一把，
    `PromptLocks`），在锁内复核代次；代次已变说明已有更新的 turn 准入，跳过
    这条过时写入。
  - `dispatch_prompt_turn` 在 `Working` 写入成功后推进该会话的
    `work_intent_epoch`（仍持 prompt 锁），使已排队的迟到 Stop 失效。
  - 占用快照失效写 `NULL` 使用同一守卫：新准入后的 turn 会写自己的新快照，
    不应被旧失效覆盖。
- 明确不做：不改数据库 schema、不加表/列；不改 `session/cancel` 协议；不改变
  Stop 的内存取消语义与级联范围；不重构 `SessionResources` 门面。

回归测试：

- 新增 wire 级用例（真实 `ServerLoop` + mpsc transport + 阻塞
  `set_work_state` 的 `SessionResources` 包装）：Stop 写入被门控期间，同连接
  另一请求仍能拿到响应；释放后 `stopped` 正确落库。
- 新增代次用例：捕获 Stop 目标代次后模拟一次新准入（推进代次并写 `working`），
  执行持久化，断言 `work_state` 保持 `working`。
- 更新受影响的既有断言：`host/recovery_parent_binding_test.rs`、
  `host/activation_test.rs`（`stop_targets` / `context_usage_invalidations`
  类型变化）。

## 验收标准

1. `set_work_state(Stopped)` 被阻塞时，同一连接上的下一条请求仍在 1s 内收到
   响应；写入释放后停止意图落库。
2. 取消之后发生的新 turn 准入不会被迟到的 `stopped` 覆盖。
3. `cargo test -p peri-acp --lib` 的相关用例与 `bash scripts/check-changed.sh`
   通过。

## 验证记录

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::notify_persistence`
  → 2 passed（wire 级响应性 + 代次守卫）。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::requests::tests::activation_tests`
  → 27 passed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::requests::tests::recovery_tests`
  → 64 passed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::requests::tests::user_input_tests`
  → 12 passed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib`
  → 1206 passed, 1 ignored, 0 failed。
- `bash scripts/check-changed.sh` → 通过（反向依赖闭包 `cargo check`）。
- `./scripts/cargo-rmcp-patched.sh fmt --all -- --check` → 无 diff。
- `git diff --check` → 无输出。
