> 归档于 2026-08-11，原路径 spec/issues/2026-08-05-transcript-drop-loses-final-messages.md

# agent 正常结束时 transcript 最后一批消息丢失落库（Drop abort writer）

**状态**：Fixed
**优先级**：高
**创建日期**：2026-08-05
**关联计划**：`2026-08-05-core-flow-bugfix-plan.md` S2.1

## 问题描述

transcript 绑定持久化后（`builder.rs:924-930` 激活 `with_persistence`），writer 用 ≤100ms 窗口批量落库（`APPEND_BATCH_WINDOW`，`transcript.rs:202-235`），未到期不落库。`run_react_loop` 结束后 executor 只读内存 transcript 构造 `AgentState`，**从不调用 `flush_persistence()`**；函数返回时 `V2AgentOutput` drop → `MessageTranscript::drop` → `abort()` writer task（`transcript.rs:754-765`），`pending_appends` 和通道中未处理的消息被直接丢弃。

**每次带 thread_store 的 turn 正常结束**，最后一条 AI 消息（最终回答）写入到 loop 退出、Phase 8、函数返回的间隔是毫秒级，几乎总是落在 100ms 窗口内 → 最终回答/最后一批工具结果未落库。会话恢复（`with_thread_context`/`load_messages`）后历史不完整，TUI 显示与持久化不一致。

## 症状详情

- `run_react_loop` 有 **6 个生产调用方**：主路径 executor、bg 子任务（`execute_bg.rs:233`、`spawner.rs:308`）、fork（`execute_fork.rs:190`）、define（`define.rs:555`）、workflow（`workflow_agent.rs:549`）——bg 子线程的 transcript 在任务结束 drop 时同样丢最后一批
- writer 的"通道关闭时 flush 剩余"分支（`None` 分支）永远走不到：abort 不走 `None` 分支

## 复现条件

- **复现频率**：必现（任何一次带 thread_store 的 turn 正常结束）
- **触发步骤**：
  1. 运行一轮对话（有持久化）
  2. 结束 turn，立即检查 SQLite 历史
  3. 最后一条 AI 消息缺失
- **环境**：带 `with_persistence` 的会话

## 涉及文件

- `peri-agent/src/session/transcript.rs:754-765` —— `shutdown_persistence` + `Drop` 用 `abort()`
- `peri-agent/src/session/transcript.rs:202-235` —— 100ms 批量窗口
- `peri-acp/src/session/executor_helpers.rs:573-601` —— Phase 8 后无 flush 直接返回

## 修复方向（对抗 review 重设计）

原方案"executor Phase 8 后显式 flush"只覆盖 6 个调用方中的 1 个，**改 Drop 层修复根因**：

- `MessageTranscript::Drop` 改为优雅关闭：`try_send(Shutdown)` 让 writer flush 积压后自行退出（参照 `langfuse-client/src/batcher.rs:215-225` 的做法，注释明确"不调用 abort()：abort 会立即取消任务，导致缓冲区中的事件丢失"）
- writer 持有 store 的独立 Arc（`transcript.rs:132`），不依赖 transcript 内部状态（`flush_appends` 只用 store/tid/pending），detached 收尾安全——一个改动覆盖全部 6 个调用方及未来新增
- executor 显式 flush 作为双保险（可选）；若坚持调用点方案，需覆盖 cancel 提前返回路径与 `close_session`（`mod.rs:258-267`）

## 测试决策

- 补"drop 后最后一批消息已落库"测试：`transcript_test.rs` 现有 Barrier 测试缺 drop 不丢覆盖；writer detached 时序需 poll store

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（peri-agent 审查发现；对抗 review 改为 Drop 层修复） |
| 2026-08-05 | Open | Fixed | agent | 修复：Drop abort 改优雅关闭（Shutdown 信号 flush 积压后退出），覆盖全部 6 个 run_react_loop 调用方；executor Phase 8 加显式 flush 双保险；补 drop 不丢消息测试 |
| 2026-08-11 | Fixed | Fixed | agent | 终态确认归档：loop 退出前 flush_persistence + drop 前 flush 剩余，修复记录与测试见正文 |

## 修复记录

### 修复 #1（2026-08-05）

- **操作人**：agent（Slice 2 / S2.1 编码切片）
- **用户原意**：transcript 正常结束时最后一批消息（含最终回答）必须落库，不能因 Drop abort writer 直接丢弃 `pending_appends` 与通道中未处理的消息
- **修复内容**：
  - `peri-agent/src/session/transcript.rs`：`PersistOp` 新增 `Shutdown` 变体；writer 循环新增 `Some(PersistOp::Shutdown) | None` 优雅关闭分支（先 flush 剩余积压再退出，注意必须位于 `Some(other)` 通配分支之前）；`shutdown_persistence()` 由 `abort()` 改为发送 `Shutdown`（参照 `langfuse-client/src/batcher.rs` 的 Shutdown 模式——不 abort，abort 会取消任务导致缓冲区事件丢失）；Drop 保持调用 `shutdown_persistence()` 但语义变为优雅关闭；writer 持有 store 独立 Arc，detached 收尾安全
  - `peri-acp/src/session/executor_helpers.rs`：Phase 8 构造 AgentState 前加显式 `flush_persistence().await` 双保险（失败仅 warn 不阻断内存路径；guard 仅作方法接收者，函数级 `#[allow(clippy::await_holding_lock)]` 注明安全原因）
  - `peri-agent/src/session/transcript_test.rs`：新增 `test_drop_flushes_pending_appends_to_store`（drop 后最后一批消息落库 + 顺序一致，轮询 store 验证）、`test_drop_without_persistence_is_noop`、`test_shutdown_persistence_flushes_pending_and_writer_exits`
- **验证状态**：待验证（`cargo build -p peri-agent -p peri-acp` 通过；`cargo test -p peri-agent --lib` 643 passed；`cargo test -p peri-acp --lib` 415 passed；`cargo clippy -p peri-agent -p peri-acp --lib -- -D warnings` 通过）
