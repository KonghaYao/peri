# P1：同步子任务被 cancel 时未上报 SubagentStopped

状态：调查完成，修复中。对应 GitHub issue #291。日期：2026-10-26。
基线：`origin/main` @ `199382c5d`。

## 现象

同步子任务（Agent/Task 工具，`run_in_background=false`）执行中，父 session 收到
`session/cancel` 后，客户端只收到 `turn_cancelled`。该子任务已发出的
`subagent_started` 没有对应的 `subagent_stopped`，下游子任务卡片停留在「运行中」。
后台子任务（`run_in_background=true`）不受影响。

## 证据

执行链是父工具调用 future 内的 inline await：

1. `peri-middlewares/src/subagent/tool/define.rs`：`invoke_output` 直接 await
   `SessionFactory::spawn_subagent`；该工具 `timeout()` 为 `None`。
2. `peri-agent/src/session/subagent/factory/spawn.rs`：`SubagentRunMode::Sync`
   直接 `run_sync_with_registered_handoff(...).await`，没有独立 task。
3. `peri-agent/src/session/subagent/run_sync.rs`：`let mut execution = Box::pin(run(...))`，
   交接前在 wrapper 的 `select!` 与后续分支中 inline poll；只有交接成功后才把
   execution 交给 `spawn_sync_worker`。
4. `peri-agent/src/session/subagent/child_runner.rs`：`run_child_with_handoff_signal`
   inline await `run_react_loop`。
5. 父侧 `peri-agent/src/agent/stages/tool_dispatch/execution.rs`：
   `tokio::select! { biased; _ = cancel.cancelled() => ..., result = invoke_fut => ... }`。
   cancel 分支命中时直接 drop `invoke_fut`。
6. 子 cancel token 由父 session token `child_token()` 派生
   （`factory/context.rs::derive_cancel_token`），父 cancel 会同时取消子 token。

因此 `run_sync_subagent` 的正常收尾序列整体被跳过：

- `flush_session_history`、`settle_explicit_close`（资源/MCP/Shell 清理、close）、
  lifecycle hook 与线程终态持久化；
- v2 `SubagentStop` 与 v1 `SubagentStopped`（`run_sync.rs` 的唯一发射点）；
- forwarder drain 与 `ChildExecutionPermit::finish` 结算。

同步路径没有 bg 的 `BgCleanupGuard` 兜底；`DeregisterGuard` 只注销
`active_agents`，不发射事件。resume 路径另有 claim Drop 写 `Cancelled`，但仍不发
Stopped。

## 既有测试缺口

现有取消用例都是「invoke 之前已 cancel」并直接 await `invoke`，不经过父 dispatch
的 select drop，因此没有覆盖本问题：

- `peri-middlewares/src/subagent/tool/tool_test/invoke_test.rs::test_cancel_token_interrupts_subagent`
- `peri-middlewares/src/subagent/tool/tool_test/integration_v2_test.rs::test_integration_sync_cascade_cancel_returns_interrupted_marker`
- `peri-agent/src/agent/stages/tool_dispatch/execution_test.rs::test_dispatch_concurrent_cancelled`
- bg 取消用例（`bg_register_cancel_test.rs`）只覆盖后台路径。

## 修复方向

在 `run_sync_with_registered_handoff` 中把 execution 作为独立 `tokio::spawn` 任务
持有，父工具调用 future 只 await 其结果。父 future 因 cancel 被 drop 后：

1. 子任务继续存活，观察到已取消的 child token，按既有 Interrupted 收尾；
2. 由既有 tail 完成 flush / close / 状态持久化 / v2+v1 Stop / permit 结算；
3. 父 turn 不再消费子结果（caller 已 drop），不产生额外父结果投递。

交接成功后的 worker 继续 await 同一 JoinHandle；`JoinError` 显式映射为失败，
不静默吞掉。

不在本次范围：后台子任务路径、子任务卡片渲染、数据库表结构。

## 验收

1. 新增回归测试：同步子任务运行中（已发 Started）取消父 token 并 drop 父调用
   future，随后恰好收到一次 `SubagentStopped`（interrupted/error 语义）；修复前用例失败。
2. 子线程终态被持久化，permit/close 走正常收尾；无重复 Stop。
3. 既有 `run_sync_test`、subagent 工具测试与取消相关测试保持通过。
4. `./scripts/check-changed.sh` 通过。

## 实现补充

- caller drop 守卫：`SyncChildCancelGuard` 在 wrapper 被 drop 时取消 child token，
  保留「父调用 drop 必须终止子任务」的既有不变式（生产 cancel 路径为幂等二次取消）；
  成功交接后 disarm，交出的执行不再受父调用 drop 影响。
- 关闭失败不写终态：`settle_explicit_close` 返回 Err 时不写线程终态、不结清 resume
  claim（保留 `CancelRunning` 重试），与后台路径的 terminal withheld 及 P1 收尾契约
  「history persistence/resource 失败保持 Active」一致；Stop 事件仍发射以使客户端收敛。

## 验证记录

- 修复前回归用例超时失败：`dropping the caller future must not swallow SubagentStopped`。
- 修复后：
  - `cancelled_sync_child_emits_stopped_after_caller_future_dropped` 通过；连续 8 次稳定。
  - `session::subagent` 全量 164 passed / 0 failed（连续 3 次）。
  - `running_caller_drop_resource_failure_keeps_close_and_claim_unfinished`、
    `test_resume_running_cancelled_by_dispatch_finalizes_claim` 通过。
  - `./scripts/check-changed.sh` 通过；`fmt --check` 通过。
- 未运行 workspace 全量测试、e2e 与 TUI 手测。

## 剩余风险

1. 取消后的同步子线程终态沿用既有 sync 语义写 `Error`（后台路径写 `Cancelled`）；
   对齐二者属于独立的产品语义变更，本次未做。
2. 关闭失败时仍会发射 Stopped/hook 且线程保持 Active；claim Drop 的 `CancelRunning`
   会再尝试一次 close，资源 owner 可能观察到两次 close 调用。
3. caller drop 现在必然取消子执行；若未来需要 drop 后无条件续跑，应改为显式 disarm。
