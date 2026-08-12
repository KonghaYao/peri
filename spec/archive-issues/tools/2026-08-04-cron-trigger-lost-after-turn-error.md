> 归档于 2026-08-11，原路径 spec/issues/2026-08-04-cron-trigger-lost-after-turn-error.md

# Cron 触发在 turn 结束后丢失（CronOwner bridge 生命周期绑定在 turn 级 V2Session 上）

**状态**：Fixed
**优先级**：高
**创建日期**：2026-08-04

## 问题描述

Cron 任务在 agent 运行中、大模型 retry 失败后完全无法注入：turn 异常结束后，cron 触发没有任何接收方，任务静默丢失（`tick` 只打 warn 日志）。根因是 cron 注入管道（`CronOwner` + bridge task + `scheduler.subscribe()`）的生命周期绑定在**每次 turn 新建的 V2Session** 上，turn 一结束（无论正常 Completed 还是 retry 失败 Error）管道即死；而 idle 期没有替代消费方，导致后续所有 cron 触发被 `retain` 清理掉。

设计文档（`docs/design/peri-agent-message-store-v2.md`）写明 AsyncOwners（SessionInbox + CronOwner）应为 **session 级、跨 turn 存活**，实现与设计存在偏差。

## 症状详情

| 现象 | 数据证据 |
|------|---------|
| retry 失败后 cron 任务完全没触发 | tick 循环健康（每 1 秒跑），但 `extra_trigger_txs` 的 sender 全部失效被 `retain` 移除，`tx.send()` 失败只打 `warn!("cron tick: extra trigger sender closed, removing")` |
| retry 期间触发的 cron 消息滞留在 queue | retry 期间（agent 运行中）bridge 还活着，消息能 push 进 session 级 `v2_message_queue`；但 retry 失败 → turn Error 结束，无人消费，直到用户下次手动输入才被 `drain_all` 消费（表现为"延迟出现"或"看似丢失"） |
| 报错后 agent 还能正常继续用 | `LoopResult::Error` 只结束当前 turn（`executor_helpers.rs` Phase 9），session 未销毁；但新 turn 重建 bridge 前，cron 触发持续丢失 |

## 复现条件

- **复现频率**：必现（turn 结束后 cron 到点即触发失败；retry 失败只是最明显的暴露窗口）
- **触发步骤**：
  1. agent 运行中（LLM 调用进行中），大模型 retry 多次后耗尽失败
  2. turn 以 `LoopResult::Error` 结束，TUI 显示错误
  3. cron 任务到点触发 → 无接收方，触发丢失
  4. （用户下一条消息发出后新 turn 重建 bridge，cron 恢复——恢复前所有触发已丢）
- **环境**：TUI 主进程（peri-tui 内嵌 peri-acp，同进程共享 `Arc<Mutex<CronScheduler>>`）

## 根因分析

### 1. CronOwner bridge 生命周期 = 单次 turn 的 V2Session 生命周期

`peri-acp/src/agent/builder.rs:896-959` 每次 turn 的 `build_stage_context` 都新建 V2Session 并挂载 cron 管道：

- `sched.subscribe()` 注册新 sender（builder.rs:922）
- spawn bridge task：`CronTrigger → prompt_tx`（builder.rs:927-951）
- `CronOwner::start(prompt_rx, inbox, cancel)`（builder.rs:953-954）
- `session.set_async_owners(...)` 挂到本次新建的 V2Session（builder.rs:959）

`AcpSession.v2_session` 从未被赋值（`peri-acp/src/session/mod.rs:86,201,239`），V2Session 随 turn 结束 drop → `CronOwner::Drop` → `handle.abort()`（`peri-agent/src/agent/session/cron_owner.rs:146-150`）→ bridge 的 `trigger_rx` 关闭 → bridge 退出（builder.rs:944）→ scheduler 对应 sender 变死。

### 2. 下一次 tick 清理死 sender，触发彻底无接收方

`peri-middlewares/src/cron/mod.rs:142-152`：

```rust
self.extra_trigger_txs.retain(|tx| {
    if tx.send(trigger.clone()).is_err() {
        warn!("cron tick: extra trigger sender closed, removing");
        false
    } else { true }
});
```

### 3. idle 期无替代消费方

- `run_react_loop` 的 idle `await_wake` 只在 `idle_should_wait` 为 true 时启用，probe 仅检查 bg subagent `active_count() > 0`（builder.rs:839-844）——cron 不满足，loop 直接 `return Completed`
- TUI 侧 primary `trigger_tx` 在 `CronState::new()` 被直接丢弃（`peri-tui/src/app/cron_state.rs:13` 的 `unbounded_channel().0`），TUI 无 CronTrigger 消费代码

### 4. retry 失败放大路径

retry 期间 cron 触发成功入队（bridge 活）→ retry 耗尽 `run_reason` 直接 `return Err`（`peri-agent/src/agent/stages/reason.rs:278`）→ `LoopResult::Error` → turn 结束（`executor_helpers.rs:579-601`）→ bridge 死 → 后续 cron 触发全部丢失；入队的消息滞留 queue 直到下次用户输入。

## 涉及文件

- `peri-acp/src/agent/builder.rs:912-960` —— cron 管道挂载点（turn 级，应提升到 session 级）
- `peri-agent/src/agent/session/cron_owner.rs:87-127` —— CronOwner `start`（生命周期随宿主 Session）
- `peri-agent/src/agent/session/inbox.rs:122-186` —— `InboxHandle`（session 级，跨 turn 存活，是正确范例）
- `peri-middlewares/src/cron/mod.rs:66-75,122-158` —— `subscribe()` / `tick()` / sender 清理
- `peri-acp/src/session/mod.rs:86-95,448-467` —— `AcpSession`（`session_inbox_for` lazy-init 是 session 级正确范例；`v2_session` 字段未使用）
- `peri-tui/src/app/cron_state.rs:12-31` —— TUI 侧 tick 循环（健康，无需改）；primary tx 被丢弃
- `peri-acp/src/agent/builder.rs:839-844` —— `idle_should_wait` 只认 bg subagent
- `peri-acp/src/session/executor_helpers.rs:546,575-601` —— `run_react_loop` / `LoopResult::Error` 处理

## 修复方向（用户已确认：Cron 是 session 级别的）

1. **bridge 提升到 session 级**：仿照 `session_inbox_for`（`peri-acp/src/session/mod.rs:448-467`）的 lazy-init 模式，在 `AcpSession` 上持有 CronOwner/bridge（subscribe 一次、跨 turn 存活）；`build_stage_context` 不再每次重建，而是复用 session 级实例。需注意 `set_async_owners` 的 "already set" 保护（`peri-agent/src/session/mod.rs:229-231`）与现有 turn 级挂载的兼容。
2. **idle 消费**（bridge 存活后的必要条件）：idle 时需有人消费 queue——`idle_should_wait` 覆盖 cron 场景，或 cron 触发时通知 TUI 发起新 turn。
3. 修复后补测试：turn Error 结束后 cron 触发仍能注入；idle 期间 cron 触发不被丢弃。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-04 | — | Open | agent | 创建（用户报告 retry 失败后 cron 无法注入；静态分析定位根因） |
| 2026-08-04 | Open | Fixed | agent | 修复合入 commit `a22a3820`：SessionCronBridge 提升到 session 级（`SessionManager::cron_bridge_for` lazy-init）；修复方向 1、3 完成，方向 2（idle 立即开新 turn）明确 out of scope 留作后续增强；回归测试 2 条 + retain 测试；独立 code review APPROVED（0 critical / 0 major，3 minor 非阻塞） |
