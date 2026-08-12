> 归档于 2026-08-11，原路径 spec/issues/2026-08-05-cancel-bg-task-workflow-kind-ineffective.md

# session/cancel-bg-task 对 Workflow 类型任务无效（条目移除但 runner 继续运行，UI 状态矛盾）

**状态**：Fixed
**优先级**：高
**创建日期**：2026-08-05

## 问题描述

`session/cancel-bg-task` 取消 Workflow 类型的 bg 任务时，只从 registry 移除条目并发出 `bg-task-cancelled` 事件，**workflow runner 实际未被终止**：workflow 继续消耗 agent/token，`workflow/list_runs` 仍显示 running，而 TUI 的 bg 任务面板已移除该条目——两端状态矛盾。根因：Workflow 任务注册时固定 `BgCancelHandle::Kill(None)`（`workflow/mod.rs:46`），真正的 `kill_tx` 在 peri-workflow 的 `WorkflowTaskRegistry`（`tool.rs:179,218`），二者从未打通；`cancel()` 命中 `Kill(None)` 分支仅打 `warn!("kill_tx already consumed")`（`background.rs:291-295`）。

> **对抗验证改判（2026-08-05）**：核心机制 100% 成立，但改判为 **P1 潜在缺陷（协议契约错误）**——TUI 从未触发该 RPC（tasks 面板 Enter 仅打日志、workflow 面板 Enter 为 no-op、`cancel_bg_task` 客户端方法无调用方），"UI 两端矛盾"当前无法由 TUI 用户操作复现，只能由外部 RPC 客户端触发；一旦 TUI 接线（修复方向 3）则必然触发。P1 实锤维度：用户以为已取消，workflow 继续烧 token/agent 直到自然结束，且 `budget_total: None`（`tool.rs:170`）无预算兜底。另注意：自然完成后会经 `executor.rs:1056 → background.rs:243-250` 推**幽灵完成事件**（`bg-task-completed` 对已取消条目仍通知）。

来源：cancel 链路三方并行审查（Agent 3 确认，P1）。

## 症状详情

| 现象 | 数据证据 |
|------|---------|
| workflow 继续运行 | `background.rs:288-296` `Kill(None)` 分支只 warn；真 kill 通道是 `workflow/kill_run`（`requests.rs:471-492`），与 cancel-bg-task 无关联 |
| `workflow/list_runs` 仍显示 running | `progress_store`（`requests.rs:434-438`）由 workflow 运行状态驱动，runner 未死则持续 running |
| TUI bg 面板条目已消失 | `bg-task-cancelled` 事件照发（`background.rs:312-315`）→ TUI `handle_bg_task_cancelled`（`system.rs:305-319`）移除条目 |
| 唯一真实 kill 通道未接线 | `workflow/kill_run`（`requests.rs:471-492`）存在，但 TUI workflow 面板 Enter 也是 no-op（`workflow.rs:126-129`） |

## 复现条件

- **复现频率**：对 Workflow 类型任务执行 cancel-bg-task 时必现
- **触发步骤**：
  1. 启动一个 workflow bg 任务（`BgTaskKind::Workflow`）
  2. 调用 `session/cancel-bg-task`（taskId=该 workflow）
  3. 观察：registry 条目移除、`bg-task-cancelled` 事件发出，但 workflow 继续执行直到自然完成
- **环境**：TUI 或 stdio 会话；WorkflowTool 运行中

## 根因分析

1. **注册时 kill 句柄为空**：`register_workflow`（`workflow/mod.rs:36-53`）固定 `BgCancelHandle::Kill(None)`，而真实 `kill_tx` 由 `WorkflowTaskRegistry`（`tool.rs:179,218`）持有，注册路径未把 kill_tx 写入 registry 条目。
2. **cancel 分发对 None 静默**：`BackgroundTaskRegistry::cancel`（`background.rs:281-321`）对 `Kill(None)` 只 warn，随后仍移除条目、发事件——**用户视角"已取消"但实际未取消**。
3. **两条 kill 通道未关联**：`workflow/kill_run`（按 run_id）与 `session/cancel-bg-task`（按 task_id）互相独立，TUI 侧也均未接线（`cancel_bg_task` 客户端方法无调用方；workflow 面板 Enter no-op）。

## 涉及文件

- `peri-middlewares/src/subagent/background.rs:281-321` —— `BackgroundTaskRegistry::cancel`（`Kill(None)` 分支仅 warn；先 remove 条目再发事件；`Kill(Some)` 分支为死代码）
- `peri-middlewares/src/workflow/mod.rs:36-53` —— `register_workflow`（固定 `Kill(None)`；trait 签名 `peri-workflow/src/tool.rs:23-26` 无 kill 通道入参）
- `peri-workflow/src/tool.rs:179,218,224-233` —— `WorkflowTaskRegistry` 真实 kill_tx（run_id 即 task_id 注册）
- `peri-workflow/src/registry.rs:162-176` —— `WorkflowTaskRegistry::kill()`（全仓唯一生产调用点是 `workflow/kill_run`）
- `peri-workflow/src/runner.rs:651-708` —— runner 退出条件（仅 kill_rx / msg_loop 自然完成，不观察 bg registry）
- `peri-tui/src/acp_server/requests.rs:471-492,517-535` —— `workflow/kill_run` 与 `session/cancel-bg-task` 命令（两通道无共享状态）
- `peri-tui/src/kit/panels/tasks.rs:66-80`、`workflow.rs:126-129` —— TUI 面板（Enter 未接线）
- `peri-tui/src/kit/acp_events/system.rs:305-319` —— `handle_bg_task_cancelled`

## 修复方向

1. **打通 kill 通道**（对抗验证修正：`BackgroundTaskRegistry` 不持有 `WorkflowTaskRegistry` 引用，无法直接委托）。可行路径：(a) 修改 `BgTaskRegistry` trait，`register_workflow` 携带 kill 闭包/oneshot Sender；(b) 在 `WorkflowMiddleware::register_workflow` 内同时写两个 registry（`mod.rs:121-143` 已有 `with_bg_registry` 注入点，它是唯一同时持有两个 registry 的地方）；(c) 由 `WorkflowMiddleware` 包装 cancel 语义，在 bg cancel 时转发到 `self.registry().kill()`。
2. **不可取消时如实反馈**：若暂不打通，`cancel()` 对 Workflow 类型应返回明确错误（而非"成功"语义），TUI 提示"请使用 workflow/kill_run"。
3. **TUI 接线**：tasks 面板 Enter 对 Workflow 任务调 `workflow/kill_run`，对 Agent/Shell 任务调 `cancel_bg_task`。
4. 补测试：cancel-bg-task 对 Workflow 类型的行为（现状 `Kill(None)` 无效行为无测试锁定；`background_test.rs` 只覆盖 Abort/Pid）。顺带核查 Node 侧 kill 时是否发 `RunDone killed`（`runner.rs:664` abort msg_loop 后若未发 RunDone，`progress.rs:247-255` 会以 Running 状态永久保留 run）。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（来源：cancel 链路三方并行审查，P1；待对抗验证） |
| 2026-08-05 | Open | Open | agent | 对抗验证：**证实，改判为 P1 潜在缺陷（协议契约错误）**。核心机制 100% 成立（注册侧 Kill(None) 硬编码 + trait 签名无 kill 通道入参 + cancel 静默无效 + runner 不观察 registry + 两 RPC 无共享状态）。最大弱化：TUI 从未触发该 RPC（TUI 端不可复现，外部 RPC 客户端可触发）。P1 实锤：资源失控（无 budget 兜底）+ 协议返回 success 却无效。补充：文件路径笔误已修正（实际在 peri-middlewares/src/subagent/background.rs 等）；Kill(Some) 分支为死代码；幽灵完成事件（自然完成后仍推 bg-task-completed）；kill_run 路径下 progress_store 可能永久保留 Running 状态 |
| 2026-08-05 | Open | Open | agent | 修复 v1（路径 a：BgTaskRegistry::register_workflow 增加 kill 闭包；Kill(None) 报错+保留条目；session not found 改 -32602；TUI tasks/workflow 面板 Enter 接线；+6 测试；middlewares 1084 / workflow 39 / tui 839 passed，clippy 0 新增） |
| 2026-08-05 | Open | Open | agent | 修复验证：**有条件通过**。核心 P1 已修复并经实证（/tmp 独立程序 + 真实 Node runtime：kill 闭包 → 同一条 kill_tx → runner ~100ms 内终止，registry 条目移除，二次 kill NotFound；RPC 层测试探针真实走通全链路）。残余 P2：kill 后 workflow/list_runs 仍永久显示 running——Node 侧 run_done{status:killed} 被 runner.rs:664 的 msg_loop.abort() 丢弃，progress_store.cleanup_completed 保留 Running（3/3 次实证均为 Running），workflow 面板持续 spinning（issue 症状表第二行未覆盖，kill_run 既有路径同样触发）。残余 P3：kill 后仍推 bg-task-completed 幽灵失败通知。待跟进：kill 后主动标记 progress_store 为 Killed（tool.rs 通知任务处已有引用，约 3 行） |
| 2026-08-05 | Open | Open | agent | 修复 v2（残余 P2 幽灵 running）：runner.rs:698-703 kill 分支、done_tx_for_kill.send() 之前补发 `progress_store.apply_event(RunDone{status:"killed"})`（+10 行），复用既有 reducer（progress.rs:207-215）标记 Killed + completed_at；progress.rs/tool.rs/TUI 零改动。MockAgentExecutor 加 delay 字段供 kill 测试维持 Running 窗口。+4 测试（progress 层 3 个：killed 状态/cleanup 保留/no-op；runner 层 1 个 #[ignore] 核心回归）。peri-workflow 42 + 2 ignored（真实 Node runtime 实证通过）、peri-middlewares 1084、peri-tui 841 passed |
| 2026-08-05 | Open | Open | agent | 修复验证（workflow review）：**通过**。kill 标记与方案候选 A 完全一致（标记 → send done_tx → cleanup 时序确定）；单实例 progress_store 贯穿 middleware→tool→runner→list_runs（workflow/mod.rs:86,120,139-151）——kill 标记对 list_runs 可见；核心回归测试防假阳性到位（先轮询 Running 再 kill，修复前必然失败），--ignored 实证通过。新发现 P2：workflow/kill_run 与 kill_agent handler 忽略 sessionId 参数取第一个带 middleware 的 session（requests.rs:454-461,477-484，多 session 时可能 kill 错 session，单 session 不受影响），建议与 list_runs 对齐改 sessions.get()。P3：Node 自然崩溃（非 kill）时 msg_loop failed 收尾不写 progress_store（同源幽灵 running 另一路径）。P3：幽灵完成事件（方案明确本次不实现）。待处理：sessionId 匹配修复；failed 收尾补 RunDone |
| 2026-08-05 | Open | Fixed | agent | L1 复验（2026-08-05）：核心 P1 修复闭环确认——Kill(Some) 闭包打通（`BgTaskRegistry for TaskManager::register_workflow` 携 kill 闭包转发 `WorkflowTaskRegistry::kill`）、cancel() 对 Kill(None) 报错保留条目、kill 后 progress_store 标记 Killed（残余 P2 幽灵 running 修复 v2）；`test_cancel_workflow_invokes_kill_closure` / `test_cancel_with_unavailable_handle_returns_error_and_keeps_entry` 随迁 async_tasks_test.rs 通过。残余待办（不在 L1 范围）：workflow/kill_run 与 kill_agent 的 sessionId 匹配、Node 非 kill 崩溃时 failed 收尾补 RunDone、kill 后幽灵完成事件 |
