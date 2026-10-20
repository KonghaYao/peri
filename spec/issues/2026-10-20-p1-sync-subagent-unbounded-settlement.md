# P1：子 Agent 无界收尾

状态：A′ 已实现并通过定向与全库回归；真实进程/TUI 手测未覆盖。日期：2026-10-20。

## 故障与证据

同步 coder 已产出最终文本，后台 Shell 仍 Working，sync 任务持续 11 小时未终结。现场时间线：2026-10-19 21:56:54 发起 Shell，21:57:32 前台超时 promote，22:03:17 child 最终回复，22:05:24 sync wrapper 转后台，22:07:33 loop 写 bounded handoff；之后没有 child Stop。特殊文件/FIFO 阻塞只是现场推断，未核实系统调用；修复覆盖任何永久 Working，不依赖此推断。不操作现场进程。

核查基线：`1247c7683e220e367c9f14396e06cbd9798a6649`。

- `session/subagent/child_runner.rs` 在 Completed 后等待 task/queue/activity，没有 deadline。
- `session/subagent/run_sync.rs` 的 120 秒只结束调用方等待，worker 继续运行；父投递失败另有无界重试。
- `agent/async_tasks/handoff.rs` 的旧写入以 0 混淆无任务、无能力及写失败。
- pool 持有 manager/MQ 不等于 worker 退出后具备可寻址取消、关闭及消息结算。

## 已批准契约

本任务以《Peri 子 Agent 无界收尾修复方案》（2026-10-20，A′，第 1–11 节）为验收契约。替代原 spec 的“timer → Completed”、仅限三个文件及 121 秒仍自动继续旧 child 的旧提案。

1. 首次 RCRA 推进结束且有未结任务/消息义务时启动唯一绝对 D，复用 `HANDOFF_MAX_WAIT=120s`；activity、loop 重入、短暂空闲不重置。
2. cancel 之后 Receive 之前、Receive 之后语义准入前检查 D；已准入 Reason/Act 完成配对，不中断其 future，不以 deadline 取消 Shell。
3. typed outcome 区分 Settled、HandedOff、Interrupted、Failed；receipt 包含 child/execution、接收方、原因、任务和消息引用、canonical ID。
4. HandedOff 映射 child Error、父 success=false、timed_out=false；保留文本及 pending，不伪造成功/Running。
5. 轻量 owner 接管原 manager/MQ、可信 scope 控制及 canonical 端口，仲裁 handoff/resume/close；不持有 LLM/StageContext/旧事件总线。
6. 冻结快照后 flush + idempotent append，再非 Active 状态提交、旧事件排空及一次 Stop/父结果；提交失败显式报告并保留资源，不无限等待。
7. 父结果 retry 交 registry 单一 worker，wrapper 释放完整 session；sync/bg × spawn/resume 同契约。
8. 父/根按 child/execution/task 在可信树内查询、取消、关闭。显式 close 非模型结算保留 MQ，失败返回 Incomplete，不删除唯一 owner。
9. 截止后需要显式 resume 才能再次推理；结果仍投直接 child。重启只加载已保存历史，不恢复旧执行或未落库 MQ。
10. 不新增 DB 表、持久状态枚举、持久 ACK 账本；不扩张 late result 即时 durable，不改 Shell promote/执行期限。

## 实施顺序

1. 定义 typed outcome、receipt、一次性 window，更新契约。
2. 轻量 owner 与 handoff/resume/close 准入。
3. 可靠提交、非 Active 状态与旧事件收尾。
4. child runner/loop 接入共享 D 与安全准入。
5. sync/background/spawn/resume、父结果及展示贯穿。
6. 移除 wrapper 自有投递等待、完成 close 消息结算与回归。

## 验收清单

- [x] 永久 Working 无通知、activity/任务新增：D 不重置，计算退出但 Shell 仍 Working（`permanent_shell_hands_off_at_deadline_without_notifications`、`activity_and_new_tasks_do_not_reset_deadline`）。
- [x] 无任务正常退出；D 内完成由旧 child 正常处理；D 后终态与 Stop 恰一次（`child_without_active_tasks_settles_immediately`、`settled_sync_child...`、`background_child_...`）。
- [x] active=0 但消息未消费仍交接；Receive 跨 D 保留历史；Reason 跨 D 的 Act 完成配对，不开下一轮（`terminal_result_without_consumption...`、`receive_across_deadline...`、`admitted_reason_completes_act_after_deadline`）。
- [x] MaxIterations 保留原错误与残留交接；HookStop 不追加模型提醒；cancel 先于 D 生效（`max_iterations_keeps_reason...`、`terminal_result_without_consumption...`、`cancelled_child...`）。
- [x] sync/bg × 新建；resume 复用 owner/MQ 并由显式 prompt 驱动（`late_result_after_deadline_requires_explicit_resume`、现有 resume 回归）。
- [x] 父 `sync-*` 注册与 child 终态竞争：无父宿主、父已 closing、正常注册三条分支均返回 typed 结果；同刻调度本身未做确定性注入（`handoff_without_parent_owner...`、`handoff_with_closing_parent...`、`handoff_sync_child...`）。
- [x] callback 拒绝后由 registry 单一 retry worker 接管，wrapper 释放完整 session（`handoff_sync_child_returns_pending_and_registry_retries_delivery`）。
- [x] 无父投递宿主/缺 owner/closing 显式失败，不制造幽灵任务（`handoff_without_parent_owner...`、`missing_owner...`、`test_close_rejects_active_execution...`）。
- [x] handoff append/status write 失败不误报交接或可 resume（`handoff_persistence_failure...`、`subagent_terminal_write_failure...`）。
- [x] late result 后显式 resume 复用原 MQ/manager、新执行身份（`late_result_after_deadline_requires_explicit_resume`）。
- [x] 不 resume 关闭：终态按原 ID 落库、其他输入记录为 `child_retained_input_unprocessed`、失败保留 owner 并返回 Incomplete（`test_close_settles_terminal...`、`parent_close_retains...`）。
- [x] D 时 pending 列表为空但仍有保留消息：wrapper 仍交接，不因空列表 abort（`handoff_with_empty_task_list_keeps_retained_messages`）。
- [x] 旧 ContinueCurrentRun 消息被抑制；A→B→C 结果仅归直接父（`old_bound_control_message...`、`nested_a_b_c...`）。
- [~] 重启仅加载历史：沿用既有 cold-resume/history 回归（全库通过），未新增独立重启 fixture；不承诺未落库 MQ 恢复。
- [ ] 真实受控阻塞进程在 D 后仍存活、正式取消接口确认进程树与输出管道清理：未实现真实进程 fixture。本任务未改动 Shell promote/执行期限，不能由单元测试替代该证据。

时间用例使用 paused-time 且实际运行非零目标用例。真实 wire/进程验收不可由静态检查替代。

## 实现记录

- `412386e90`：阶段 1，定义 outcome/receipt、一次性窗口及目标契约。
- `ca20a81ec`：阶段 2，父 TaskManager 保留轻量 owner，原 MQ/manager 复用、执行 permit、可信子树查询/取消与根关闭接入。
- `da969a92c`：阶段 3，冻结交接快照、flush + 幂等 append、区分缺能力/写失败；同步状态提交失败显式返回，先提交再 Stop，事件排空失败阻止 owner resume。
- 后续提交：接入共享 D、loop/Receive 准入门、typed outcome 四条路径、wrapper retry 移交、owner close 消息结算、binding 释放、旧消息抑制及验收测试。

剩余风险：同刻调度未穷举；真实进程与 TUI 手测未执行；`close_subagent_session_scope` 的 owner.close 会取消运行中 Shell（沿用显式关闭策略），与截止交接的“不取消 Shell”是不同触发路径。

## 实际验证

所有命令在指定 worktree 内执行，统一前缀 `./scripts/cargo-rmcp-patched.sh`。

- `test --locked -p peri-agent --lib -- session::subagent::child_runner::tests`：21 passed，0 failed（含 background/nested 子模块；paused-time）。
- `test --locked -p peri-agent --lib -- session::subagent::run_sync::tests`：7 passed，0 failed。
- `test --locked -p peri-agent --lib -- agent::async_tasks::handoff::tests`：6 passed，0 failed。
- `test --locked -p peri-agent --lib -- session::subagent::resource_owner::tests`：7 passed，0 failed。
- `test --locked -p peri-agent --lib -- session::subagent::`：129 passed，0 failed。
- `test --locked -p peri-agent --lib`：1181 passed，0 failed，3 ignored。并行运行时 `agent::stages::receive::tests::committed_delivery_missing_from_hot_state_is_restored_without_reemitting` 曾因共享 git fixture 失败一次，单测与复跑均通过；该用例不涉及本修复路径。
- `test --locked -p peri-acp-types --lib`：660 passed，0 failed。
- `build --locked -p peri-agent -p peri-acp`：通过，退出码 0。
- `clippy --locked -p peri-agent -p peri-acp --lib -- -D warnings`：通过，退出码 0。
- `test --locked -p peri-agent --doc`：15 passed，0 failed。
- `test --locked -p peri-acp-types --doc`：6 passed，0 failed，2 ignored。
- `test --locked -p peri-acp --lib`：1039 passed，0 failed，1 ignored（含 live resume child chain 用例）。
- 未执行：真实 MCP wire、受控阻塞进程与进程树清理、TUI 手测。理由是实现范围未包含这些 fixture；不能由静态检查或单元测试替代。
