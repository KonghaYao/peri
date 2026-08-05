# Refactor Plan: 核心流程 bug 批量修复 —— 异常路径收尾、持久化/遥测收尾、bg 生命周期

**状态**：Open（plan 阶段）
**优先级**：高
**创建日期**：2026-08-05
**来源**：3 个并行 code-reviewer 全仓核心流程审查（peri-agent 主循环 / peri-acp 链路 / peri-tui+peri-middlewares）+ 1 个对抗性 plan review（devil's advocate）

## Problem Statement

2026-08-05 对核心流程（`peri-tui → peri-acp → peri-agent::run_react_loop`）做批量静态审查，发现主流程正常路径健康（工具事件配对完整、cancel 五处检查、subagent 双通道身份键闭合），但存在 12 个可确认缺陷，集中在三个系统性缺口：

1. **异常/取消路径的收尾缺失**：主循环对成功路径的收尾（`TurnCompleted`、`StageEnded`、持久化 flush、注册清理）覆盖完整，但所有 `Err`/`Interrupted` 路径的收尾都缺失或不对称。
2. **持久化与遥测收尾**：transcript writer 在 drop 时 abort 丢弃积压（每次 turn 最后一批消息丢落库）；Langfuse 事件在慢 flush 期间被静默丢弃。
3. **bg 任务生命周期协议不统一**：注册晚于 spawn（幽灵任务）、取消只 abort 不清理（泄漏/孤儿进程）、跨 turn 存活状态串扰。

对抗性 plan review 推翻了 4 个初始修复方案（P1-1 标志无置位时机、P2-1 tokio 无先注册后 spawn API、P2-2 async 收尾无法进 RAII、P0-2 只修 1 个调用点没修根因），砍掉 2 项（P4-3 无行为 bug、P4-1 与"取消不留痕"设计意图冲突），本计划为对抗后修订版。

## Solution

按"影响 × 成本"分 5 步实施，每步独立可验证：

- **Step 1 纯 bug 批**：caps 协商一次性消费、bg expect 双 panic、reason 死分支、StageEnded 不对称（4 项，无决策依赖）
- **Step 2 持久化根因**：`MessageTranscript::Drop` 改优雅关闭（Shutdown 模式），覆盖全部 6 个 `run_react_loop` 调用方
- **Step 3 bg 生命周期**：spawn 包装任务门控注册、取消走 token+超时 abort+同步收尾
- **Step 4 TUI 状态机**：auto-compact 重放判定（先锁测试）、cancel phase 双保险
- **Step 5 遥测/事件收尾**：删 bg-task-completed 死路径、batcher 丢弃汇总、TurnCompleted 补发

## Slices（对抗后修订）

### Step 1 — 纯 bug、无决策依赖

**S1.1** `caps 协商值只消费一次` → `2026-08-05-caps-negotiated-once-broken-second-session.md`
**S1.2** `bg.rs 双 expect 可经公开 RPC panic` → `2026-08-05-bg-command-expect-panic-via-rpc.md`
**S1.3** `reason.rs match 死分支（cancel 误报 LlmFailure）` → `2026-08-05-cancel-misreported-as-llm-failure.md`
**S1.4** `run_stage Err 不 emit StageEnded / compact cancel 无配对` → `2026-08-05-stage-ended-missing-on-error-path.md`

### Step 2 — 持久化根因（方案已重设计）

**S2.1** `MessageTranscript::Drop 改 Shutdown 模式优雅关闭`（替代原"executor 加 flush"方案——原方案只覆盖 6 个调用方中的 1 个，bg/fork 路径依然丢）→ `2026-08-05-transcript-drop-loses-final-messages.md`

### Step 3 — bg 生命周期（方案已重设计）

**S3.1** `先注册后 spawn 不可行（tokio AbortHandle 只能来自 JoinHandle）` → 改为 spawn 包装任务 + 注册结果 oneshot 门控 → `2026-08-05-bg-task-over-limit-still-runs.md`
**S3.2** `Abort 仅 handle.abort() 跳过全部收尾` → 改为 `token.cancel()` + 超时 abort 兜底 + 同步收尾 guard → `2026-08-05-bg-cancel-abort-skips-cleanup.md`

### Step 4 — TUI 状态机（先锁测试再改）

**S4.1** `auto-compact 误触发 session/load 重放`：`compact_command_pending` 标志方案被推翻（`/compact` 以 AgentText 提交、`CompactCompleted` 无 trigger 字段，标志无处置位）→ 待选方案：服务端透传 `CompactTrigger` / TUI 流事件清除标志；**先提交场景 B 的 `rx.try_recv()==Err(Empty)` 断言（现有测试必红）** → `2026-08-05-auto-compact-triggers-spurious-session-reload.md`
**S4.2** `cancel_consumer 直接写 atom 与 bridge phase 派生不同步`：保留直接写兜底 + 注入事件双保险；先 cancel RPC（带超时）再复位 → `2026-08-05-cancel-consumer-loading-phase-desync.md`

### Step 5 — 遥测/事件收尾

**S5.1** `BackgroundTaskCompleted 全链死路`：补映射无效（TUI handler 只打日志），改删 Path A 直推 + 评估变体废弃 → `2026-08-05-background-task-completed-event-dead-path.md`
**S5.2** `langfuse batcher 慢 flush 期间 DropNew 丢事件`：优先"丢弃汇总计数+日志"，通道分离为可选 → `2026-08-05-langfuse-batcher-drops-during-slow-flush.md`
**S5.3** `run_after_agent 失败无 TurnCompleted`（唯一保留的异常路径收尾项，低风险）→ `2026-08-05-after-agent-failure-missing-turn-completed.md`

## Decision Document

- **P0-2 → S2.1 改 Drop 层修复**：`MessageTranscript::Drop` 的 `abort()` 是根因（`transcript.rs:761-765`），参照 `langfuse-client/src/batcher.rs:215-225` 的 Shutdown 模式（注释明确"不调用 abort()：abort 会立即取消任务，导致缓冲区中的事件丢失"）。writer 持有 store 独立 Arc、detached 安全。executor 显式 flush 降级为双保险。
- **P1-1 → S4.1 方案二选一**：服务端透传 `CompactTrigger`（`CompactStarted` 已携带 Auto/Manual，`peri-agent/src/agent/events.rs:154-157`，需 event_sink 补映射）或 TUI 侧"任何流事件到达时清除标志"。先写红测试锁定，再选方案。
- **P2-1 → S3.1 包装任务**：`AbortHandle::new_pair` 无法与 `tokio::spawn` 关联；spawn 一个先 await 注册结果 oneshot 的包装，注册失败直接 return，成功才跑 `run_react_loop`。必须覆盖 `register_runtime` deregister 与已 emit `SubagentStarted` 的配对 Stop。
- **P2-2 → S3.2 组合方案**：async 收尾无法在 Drop 中 await（RAII 方案被推翻）；`BackgroundTask` 需新增 cancel_token 字段；`token.cancel()` + 超时后 abort 兜底 + 同步收尾 guard，async 收尾在兜底路径丢失并记日志。实施前必须验证 `run_react_loop` 所有 await 点响应 cancel（工具执行中、HITL 等待中）。
- **P3-2 → S5.1 删而非补**：补映射是死路（TUI `handle_background_task_completed` 只打日志，`agent.rs:19-41`）；真实通知走 registry 通道（`system.rs:265`）。删 `executor.rs:1148-1181` Path A 直推，注意 `variant_coverage_test.rs`。
- **P0-3 → S1.1 保留双 fallback**：`consume_pending_caps` 未协商 → `unwrap_or_default()`（全 false）与 `ensure_session_caps` 未协商 → `all_enabled()` 语义不同，修复必须保留各自 fallback，否则 TUI/stdio 一侧行为翻转。
- **P0-2 关联**：`registry.complete` 对已 remove 条目返回 false 是有意设计（防幽灵完成事件），**不是**泄漏，S3.2 不得改动该行为。

## Testing Decisions

- **先写红测试**（S4.1 必做）：`acp_events_test.rs:1708-1741` 场景 B 补 `rx.try_recv() == Err(Empty)` 断言——现有测试已证明会红，同时是 bug 锁定与修复验收。
- **mock 注入**（S1.3/S3.1/S3.2）：cancel 竞态窗口与注册竞态无法自然触发，需 mock LLM 直接返回 `Err(Interrupted)`（`reason_test.rs` 已有 harness）、mock registry 强制 `register_with_kind` 失败。
- **持久化**（S2.1）：补"drop 后最后一批消息已落库"测试（`transcript_test.rs` 现有 Barrier 测试缺 drop 不丢覆盖）；writer detached 时序需 poll store。
- **协议**（S1.1）：stdio 集成测试断言"第 2+ 个 session/new 拿到协商值"（`stdio_test.rs` 有框架）。
- **回归**：每步跑目标 crate `--lib` 测试 + `cargo clippy --workspace --all-targets -- -D warnings`；S5.1 关注 `variant_coverage_test.rs`。

## Out of Scope（挂起，需产品决策，不建 issue）

- **P4-1 审批 cancel 留痕**（`tool_dispatch.rs:354-423`）：cancel 时提交 error 结果 = 改变"取消不留痕"语义（`transcript.rs:525` "丢弃暂存数据"），下一轮 LLM 会看到 "interrupted by user" 的 tool_result——特性 or 污染需决策。
- **P4-3 LoopResult 双语义统一**：6 个生产调用方均已各自归一化，无行为 bug；统一会改变用户可见字符串（"Background sub-agent failed: interrupted"）并可能挂 e2e。**建议砍掉**，如需做降级为文档化 TODO。
- **P1-3 排队输入语义**（`turn.rs:141-142`）：取消时丢弃输入但气泡残留 vs 保留队列——产品决策；"保留"选项需配套 drain，否则 agent 不再运行时排队输入永久悬挂。
- **#8 add_estimated_tool_tokens 未接线**（`token.rs:58-66`）：注释声称的功能从未调用，compact 预算低估；需先确认 P0-5 文档意图再定是否接线。

## 关联 issue

- 本批 12 个分项 issue（见 Slices 引用）
- 交叉引用（不重复建）：`2026-08-05-cancel-bg-task-workflow-kind-ineffective.md`（Workflow 类型 Kill(None) 未打通，与 S3.2 同区域互补）、`2026-08-05-loading-stuck-after-transport-close.md`（is_loading 无看门狗，与 S4.2 兜底语义需协调）、`2026-08-05-stale-turn-interrupted-overwrites-new-turn.md`（turn 代际防护，与挂起项 P1-3 同区域）、`2026-07-25-control-boundary-events-can-be-dropped.md`（EventBus 饱和丢事件，与 S5.2 不同层）

## Devflow 声明（auto-devflow）

- **模式**：max（floor）——本批涉及 concurrency/async lifecycle（safety floor）、持久化数据、跨 crate 边界（peri-agent/peri-acp/peri-tui/peri-middlewares）。
- 逐项执行时按项降级：S1.3/S1.4/S5.3 可降 normal/lite（单点、低风险）；S3.x/S4.x/S2.1 保持 max。
- 每项遵循：explore → plan → plan review（max）→ 单 coder 切片 → code-reviewer → verification。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（三 agent 审查 + 对抗 review 后定稿） |
| 2026-08-05 | Open | Fixed | agent | 12/12 分项全部实施完成（5 切片 + 1 收尾）；整合审查有条件通过，2 个中等发现已修复；编译期发现 S2.1 flush 的 guard 跨 await Send 回归并修复（persist_tx_handle + flush_via_tx）；clippy --workspace --all-targets -D warnings 全绿；分项验证状态待真机 E2E |
| 2026-08-05 | Fixed | Fixed | agent | S4.1 方案 A 根治完成（服务端透传 CompactTrigger，8 文件 + 2 测试，详见分项 issue 修复 #2）；E2E 目标场景真机验证：compact-command / clear-chat / bg-task-area / basic-question 4/4 通过，thread-switch 1 项 judge 误判（1514 个历史线程环境假设失效，快照证实线程切换功能正常）；单测 peri-agent 646 / peri-acp 415 / peri-tui 895 / peri-middlewares 1099 全绿 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
