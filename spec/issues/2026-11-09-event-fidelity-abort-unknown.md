# 设计债：事件失真与强制 abort 的不确定出口缺少收敛入口

状态：open（设计债）。本轮只记录证据与后续任务，不改生产代码、不清任何 flag。
日期：2026-11-09；基线：`2b6de62c9`。

## 1. 范围

两个独立问题，共同点是「保守地承认不确定」，但缺少从不确定回到可核对状态的入口：

1. Agent 事件总线在 render/state 通道拥塞时丢弃事件；`BusHealth` 只留下 sticky
   `dirty`，当前没有自动失真修复契约。
2. 宿主 `Drop` 直接 abort 任务，不等待异步排空；`session/close` 读到
   `CloseSettlement::Unknown` 只返回错误，没有显式证据收敛入口。

本文不宣称 canonical 数据丢失、永久转圈或 P0。历史读取与重新进入仍可修复观察；
「未知副作用不得改写为成功、失败或已取消」的保守处理正确，应保留。

## 2. 已核实证据

### 2.1 事件丢弃与 sticky dirty

- `peri-acp-types/src/event_v2/bus.rs`：`EventBus::emit_render` / `emit_state` 在
  `try_send` 返回 `Full` 时给出 `EmitOutcome::Dropped`；`BusHealth::record` 置
  `dirty = true` 并累计计数。后续成功发送不会清除 `dirty`。
- `peri-acp/src/host/prompt.rs` 的新轮次准入只记录「事件视图仍需重同步」警告，
  不清除丢失记录，也不触发重同步。
- 既有用例 `peri-acp-types/src/event_v2/bus_test.rs` 的
  `full_channels_report_dropped_and_sticky_health` 与
  `closed_channels_report_closed_and_record_loss` 直接复现 drop 与 sticky 计数。

### 2.2 强制 abort 与 Unknown 关闭

- `peri-acp/src/host/lifecycle.rs` 的 `Drop for AcpHostHandle` 调用 `task.abort()`；
  只有显式 shutdown 拥有排空保证。
- `peri-acp/src/host/requests/session_close.rs` 在会话仍为 closing 且未 delete 时读取
  `close_settlement`；`CloseSettlement::Unknown` 直接返回 `-32010`，没有查询、重试
  或人工确认的收敛路径。
- `peri-resources/src/sessions/sqlite_store/session_data/lifecycle.rs` 的
  `read_close_settlement` 在 thread 不存在时给出 `Unknown`；用例
  `close_intent_survives_reopen_and_finishes_without_execution_owner` 断言 missing
  thread 为 `Unknown`。

本轮实测（本机 macOS，基线 `2b6de62c9` + 本文档提交）：

- `nextest -p peri-acp-types -E 'test(event_v2::bus::tests)'`：16/16 通过。
- `nextest -p peri-resources -E 'test(close_intent_survives_reopen_and_finishes_without_execution_owner)'`：
  1/1 通过。
- 上述用例只证明 drop 计数、sticky dirty 与 `Unknown` 分类存在，不证明用户可见视图
  必定失真或同进程重开必定阻塞。

### 2.3 尚未证明

- TUI 在丢弃后的可观察差异、历史重读恢复视图所需条件，以及其他消费者是否依赖
  丢失事件。
- 同进程 abort 后重新 load/close 的实际阻塞时长与可达出口；不同 Store 后端是否
  一致。
- 丢失事件是否改变模型可见的 canonical 历史（当前证据只覆盖观察面）。

## 3. 需要后续 task 的证据

1. 事件失真复现：构造 render/state 容量受限、消费者暂停的受控用例，记录 TUI
   可观察差异；确认历史重读或重新 load 是否能修复视图，形成「失真 → 修复」契约测试。
2. 修复入口裁决：确定 `dirty` 的消费方与触发方式（显式 resync、消费者重建或其他），
   并给出不丢 canonical 的前提。
3. 强 abort 复现：构造 `AcpHostHandle` Drop 时有在途模型/工具任务的用例，记录同进程
   close/load 的返回码、等待时间与最终状态。
4. `Unknown` 收敛：定义可查询、可重试或可人工确认的证据入口；`Unknown` 不得映射为
   success，也不得伪造 `Finished`。
5. 可观测性：日志/状态保留 session、turn、通道与累计丢弃数，避免只有布尔 `dirty`。

## 4. 非目标

- 不把「事件丢失」升级为 canonical 数据丢失；历史与 Store 仍是权威。
- 不把「不确定」改成成功、失败或已取消。
- 不在本任务中改生产代码、flag、表结构或索引。
