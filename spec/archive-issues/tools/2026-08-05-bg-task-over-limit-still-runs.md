> 归档于 2026-08-11，原路径 spec/issues/2026-08-05-bg-task-over-limit-still-runs.md

# bg 任务"检查-注册"竞态：超限任务报错但仍实际运行（幽灵任务）

**状态**：Fixed
**优先级**：中
**创建日期**：2026-08-05
**关联计划**：`2026-08-05-core-flow-bugfix-plan.md` S3.1

## 问题描述

并发上限检查（`active_count() >= 3`）与 `register_with_kind` 之间存在多个 await 点（create_thread、middleware 装配等），不是原子操作。两个并发 bg 任务同时通过检查后，后者的 `register_with_kind` 返回 `KindConcurrentLimit`，但 `tokio::spawn` 的任务已经启动：

- `SubagentStarted` 已在注册前发出（`execute_bg.rs:155-160`）
- 任务继续跑完，`on_bg_complete` 仍被调用（`execute_bg.rs:311/382`）→ Defer 注入主 agent MQ
- `SubagentStopped` 照发（`bg_event_sender`），`registry.complete` 因条目不存在返回 false 不推 Completed 事件

用户看到 "Failed to register background task" 错误，但任务实际执行并在完成后通知主 agent——工具结果与真实行为不一致，主 agent 上下文被误导。

## 症状详情

- 可达性校准（对抗 review）：`register_with_kind` 是 **per-kind 上限**（agent=3, shell=5, workflow=3），预检是 total≥3。当 total<3 时任一 kind 计数 ≤ total < 3 ≤ 上限，注册**永远不会失败**——失败只发生在并发竞态（两个任务同时通过 total 预检后串行注册撞上 kind 上限）。低概率竞态，但预检到注册隔了 `build_agent_from_def` 的多个 await，窗口不小
- **同构缺陷**：`spawner.rs:139-141`（检查）→ `:280`（spawn）→ `:460-462`（注册）
- **遗漏的 double 泄漏**：`register_runtime`（active_agents 注册，`execute_bg.rs:95-101`）在 spawn 前已执行，注册失败时无人 deregister；已 emit 的 `SubagentStarted` + lifecycle hook 无配对 Stop → subagent_depth 错乱

## 复现条件

- **复现频率**：偶发（并发竞态）
- **触发步骤**：LLM 单条消息并行调用两个 `Agent(run_in_background:true)`（v2 支持并行工具调用），或 bg 任务 + cron/workflow 并发，当前活跃数恰为 2

## 涉及文件

- `peri-middlewares/src/subagent/tool/execute_bg.rs:47-51`（检查）、`:199`（spawn）、`:408-411`（注册）
- `peri-middlewares/src/subagent/spawner.rs:139-141`、`:280`、`:460-462`

## 修复方向（对抗 review 重设计）

"先注册成功再 spawn"在 tokio 无干净 API（`AbortHandle` 只能来自 `JoinHandle`，`AbortHandle::new_pair` 无法与 `tokio::spawn` 关联），改为：

- **spawn 包装任务**：spawn 一个先 await 注册结果 oneshot 的包装，注册失败直接 return（不跑 `run_react_loop`），成功才继续——或注册失败分支 abort join_handle
- **必须覆盖**：`register_runtime` 失败时 deregister（或改为注册成功后再执行 register_runtime）；已 emit `SubagentStarted` 的配对 Stop 补发
- 测试：注册失败路径依赖并发竞态，需 mock registry 强制 `register_with_kind` 失败（注入点）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（peri-tui/中间件审查发现；对抗 review 校准可达性并推翻原方案） |
| 2026-08-05 | Open | Fixed | agent | 修复：spawn 包装任务 + 注册结果 oneshot 门控（execute_bg.rs / spawner.rs），注册失败直接 return；SubagentStarted/SubagentStart/v2 Start 与 register_runtime 移至注册成功之后，失败路径零事件零注册 |

## 修复记录

### 修复 #1（2026-08-05）

**方案**：`tokio::spawn` 一个包装任务，闭包第一步 await `oneshot::channel` 的注册结果；调用方先 `register_with_kind`（此时包装任务已 spawn 但阻塞在 oneshot），成功则 `register_runtime` + `send(Ok)` 放行，失败则 `send(Err)` + `return Err`——包装任务直接 return，不跑 `run_react_loop`、不 emit 任何事件。

**配套改动**：
- `SubagentStarted` + `SubagentStart` hook + v2 `SubagentStart` 从 spawn 前移至闭包内注册成功后（对齐 fork 路径语义），注册失败零事件 → 无"已 emit Started 无配对 Stop"的 depth 错乱
- `register_runtime`（active_agents）移至注册成功分支（先注册运行时再放行任务），失败路径零注册 → 无需 deregister，消除 double 泄漏
- `BgCancelHandle::Abort` 由 `AbortHandle` 改为 `JoinHandle<()>`（支撑 S3.2 超时等待语义）
- 覆盖测试：`test_bg_register_failure_does_not_execute_task`（multi_thread + barrier 确定性制造注册竞态：4 并发 invoke 1 成功 3 失败；断言失败任务零 LLM 调用、零事件、零 register_runtime 注册、registry 无幽灵条目）

**验证状态**：已验证（L1 复验 2026-08-05：`cargo test -p peri-middlewares --lib subagent` 135 通过，含 test_bg_register_failure_does_not_execute_task 与 test_p0_2_background_defined_skill_preload_once_after_parent_cancel；注册失败零事件零注册语义保持）

