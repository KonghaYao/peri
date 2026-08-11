> 归档于 2026-08-11，原路径 spec/issues/2026-08-05-bg-cancel-abort-skips-cleanup.md

# bg agent 取消（Abort）跳过全部收尾：active_agents 泄漏 + 子进程孤儿化

**状态**：Fixed
**优先级**：高
**创建日期**：2026-08-05
**关联计划**：`2026-08-05-core-flow-bugfix-plan.md` S3.2
**关联 issue**：`2026-08-05-cancel-bg-task-workflow-kind-ineffective.md`（Workflow 类型 Kill(None) 未打通，同区域互补，不重复）

## 问题描述

对 Agent 类 bg 任务，取消仅 `handle.abort()`（`background.rs:321-323`）。spawn 闭包中 `run_react_loop` 之后的收尾（`emit_subagent_stop_bg`、`bg_registry.complete`、`deregister_runtime`、`thread_store.update_thread_status`、`fire_stop_hooks`）全部被跳过：

- `register_runtime`/`deregister_runtime` 对应的 active_agents 条目永久泄漏（同步路径有 `DeregisterGuard`，bg 路径没有）
- abort 不触发子任务的 `CancellationToken`——bg agent 内已启动的 Bash 子进程（若 Bash 工具未设 `kill_on_drop`）不会随 task abort 终止，成为孤儿进程
- thread_store 中 child thread 状态停留 running
- `subagent_depth` 不配对（TUI `handle_bg_task_cancelled` 只更新面板，不配对 depth）
- 另：cancel 是 `remove` 而非置 Cancelled 语义——abort 生效前任务继续跑，transcript 继续写 hidden thread（取消后幽灵写入）；UI 上任务瞬间消失但实际还在跑

## 症状详情

- 对抗 review 修正：`registry.complete` 被跳过**不是**泄漏——`complete` 对已 remove 条目返回 false 不推事件是有意设计（`background.rs:231-232` 注释"否则会产生幽灵完成事件"），修复时不得改动该行为
- `BackgroundTask` 结构里没有 cancel_token（只有 `Abort` 句柄），"Abort 前先 `token.cancel()`"需要改结构存 token

## 复现条件

- **复现频率**：必现（取消 Agent 类 bg 任务）
- **触发步骤**：bg 任务运行中取消（bg 面板 / session/cancel-bg-task）

## 涉及文件

- `peri-middlewares/src/subagent/background.rs:319-323` —— remove + abort
- `peri-middlewares/src/subagent/tool/execute_bg.rs` —— spawn 闭包收尾序列

## 修复方向（对抗 review 重设计）

RAII guard 方案被推翻（async 收尾无法在 Drop 中 await），改为组合方案：

1. `BackgroundTask` 新增 cancel_token 字段
2. 取消时 `token.cancel()`（让工具层取消链生效）→ **超时兜底**：验证 `run_react_loop` 所有 await 点响应 cancel（工具执行中、HITL 审批等待中是否吃 cancel？）后，超时再 abort——否则"取消后任务继续跑"比 abort 更糟
3. 同步收尾 guard（deregister_runtime 等同步操作可进 guard）；async 收尾（update_thread_status、fire_stop_hooks）在 abort 兜底路径丢失并记日志
4. 评估 remove 语义改"标记 Cancelled"以消除取消后幽灵写入

## 测试决策

- mock registry + deregister 断言；验证取消后 active_agents 归零、thread 状态不再 running

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（peri-tui/中间件审查发现；对抗 review 推翻 RAII 方案并修正误报） |
| 2026-08-05 | Open | Fixed | agent | 修复：BackgroundTask 增 cancel_token 字段；cancel() 的 Abort 分支先 token.cancel()（任务响应取消链走完整收尾）→ 超时（3s）abort 兜底；任务内新增 BgCleanupGuard 同步收尾（deregister + SubagentStopped 补发配对） |

## 修复记录

### 修复 #1（2026-08-05）

**方案**：`BackgroundTask` 新增 `cancel_token` 字段（Agent 类任务）；`cancel()` 的 `Abort` 分支序列改为：
1. `token.cancel()`——工具层取消链生效，`run_react_loop` 在下一个响应 await 点（reason LLM select / 工具执行 select / idle select / 循环顶 is_cancelled）返回 Interrupted，走完整收尾（SubagentStopped / hooks / thread status / complete / deregister）
2. 异步超时兜底：`tokio::time::timeout(3s, &mut JoinHandle)` 等待任务自然结束（保留 async 收尾），超时再 `abort()`——否则"取消后任务继续跑"比 abort 更糟
3. 无 tokio runtime 上下文时（防御分支）直接 abort

**配套改动**：
- 任务闭包内新增 `BgCleanupGuard`（Drop 时同步执行 deregister_runtime + 补发 SubagentStopped），正常路径显式 emit 后 `disarm_stop()`；abort/panic 兜底路径同步收尾不丢失；`registry.complete` 对已移除条目返回 false 不推事件的有意设计**未改动**
- `BgCancelHandle::Abort` 由 `AbortHandle` 改为 `JoinHandle<()>`（可 await 等待）
- 覆盖测试：`test_cancel_abort_token_cancels_task_first`（token 先于 abort）、`test_cancel_abort_grace_timeout_fallback`（超时 abort 兜底）、`test_bg_cancel_trigger_token_and_cleanup`（集成：取消后 SubagentStopped 配对、active_agents deregister、registry 无幽灵 Completed 事件）

**残余风险（记录，未跨 crate 修）**：
- `run_react_loop` 中 **HITL 审批等待不响应 cancel**（`HumanInTheLoopMiddleware::broker_approve` / `batch_broker_approve` 的 `broker.request` 仅 300s 超时，无 `CancellationToken` 竞争）——取消时任务可挂至审批返回，3s 后 abort 兜底，async 收尾（thread status / stop hooks）丢失（同步 guard 仍执行）
- abort 兜底路径丢失的收尾：`update_thread_status`（thread 停留 running）、`fire_subagent_stop_hooks`（生命周期 hook）、`on_bg_complete`（Defer 不注入主 agent MQ）
- remove 语义（"标记 Cancelled"）未改：token.cancel() 生效后幽灵写入窗口极小（任务在下一个 await 点退出），hidden thread 写入可接受；status 枚举/complete/TUI 映射改动面大，留待后续
- bg shell 子进程孤儿化不在本修复范围（Shell 路径走 Pid 进程组 kill，既有行为）

**验证状态**：已验证（L1 复验 2026-08-05：`cargo test -p peri-agent --lib async_tasks` 35 通过，含 test_cancel_abort_token_cancels_task_first / test_cancel_abort_grace_timeout_fallback；peri-middlewares subagent 135 通过含 test_bg_cancel_trigger_token_and_cleanup。残余风险记录见上，HITL 审批等待不响应 cancel 等随 L5/Runtime 处理）

