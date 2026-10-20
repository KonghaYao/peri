# P1：子 Agent 无界收尾

状态：A′ 已实现；第五轮复审的主会话 HookStopped panic 与旧 Stop 污染新 run 已修复并补回归；独立验收仍待复核。真实 MCP wire、TUI 手测与自有重启 fixture 未覆盖。日期：2026-10-20。

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

- [x] 永久 Working 无通知、每秒 activity/任务新增：D 不重置，计算退出但 Shell 仍 Working（`permanent_shell...`、`activity_and_new_tasks...` 连续 5 次每秒注入）。
- [x] 无任务正常退出；D 内完成由旧 child 正常处理；D 后终态与 Stop 恰一次（`child_without_active_tasks...`、`settled_sync_child...`、`background_child_...`）。
- [x] active=0 但消息未消费仍交接；Receive 提交跨 D 后才登记义务；Reason 跨 D 的 Act 完成配对（`terminal_result_without_consumption...`、`receive_commit_crossing_deadline...`、`admitted_reason...`）。
- [x] MaxIterations 保留原错误与残留交接；HookStop 不追加模型提醒；cancel 先于 D 生效（`max_iterations_keeps_reason...`、`terminal_result_without_consumption...`、`cancelled_child...`）。
- [x] sync/bg × 新建；真实 `SessionFactory::resume_subagent`/claim 复用 owner 的原队列与 manager（`test_resume_factory_reuses_handoff_owner_queue_and_manager`、ACP live resume）。
- [x] 父 `sync-*` 注册与 child 终态竞争：无父宿主、父已 closing、正常注册三分支；边界 1ms 前后覆盖调度方向，精确同刻调度无法由 tokio 定时器注入。
- [x] callback 拒绝→拒绝→成功后 close 成功率；持续失败保留 delivery 记录（`handoff_sync_child...`、`parent_close_retains...`）。
- [x] 无父投递宿主/缺 owner/closing 显式失败，不制造幽灵任务。
- [x] handoff append/status write 失败不误报交接或可 resume；执行错误 + 交接写失败返回 `HandoffFailed` 并保留原错误/冻结 receipt（`execution_error_with_handoff_write_failure...`、`handoff_persistence_failure...`）。
- [x] late result 后显式 resume 复用原 MQ/manager、新执行身份。
- [x] 不 resume 关闭：终态按原 ID 落库、其他输入记录为 `child_retained_input_unprocessed`；真实 barrier 失败保留 owner/binding 并返回 Incomplete（`test_close_with_unconfirmed_history_flush_keeps_owner`）。
- [x] D 时 pending 列表为空但仍有保留消息：wrapper 仍交接，不因空列表 abort。
- [x] 旧 ContinueCurrentRun 消息被抑制；A→B→C 结果仅归直接父；root 可在真实三层 owner 树查询/取消/关闭（`test_root_queries_cancels_and_closes_nested_owner_tree`）。
- [~] 重启仅加载历史：沿用既有 cold-resume/history 回归（全库通过），未新增独立重启 fixture；不承诺未落库 MQ 恢复。
- [x] 旧 Stop 晚到：绑定旧执行身份的 Stop 在真实 resume 与新执行中被抑制，同执行 Stop 仍生效（`test_resume_suppresses_stop_intent_from_previous_execution`、`stale_stop_intent_does_not_stop_new_execution`、`deadline_left_stop_intent_does_not_stop_resumed_execution`）。
- [~] 旧 cancel 晚到：由 permit 在排空后释放保证，未单独注入迟到 cancel。
- [x] 真实受控阻塞进程组：D 后父/子进程仍存活，正式取消接口杀死整个进程组并回收输出管道（`real_blocking_shell_tree_survives_deadline_then_cancel_cleans_tree`）。
- [~] ACP 根关闭 + 真实 MCP producer barrier、TUI 与真实 MCP wire：owner/root close 已覆盖；ACP 全库通过，但未构造“late result + 真实 MCP producer”与 TUI 手测。

时间用例使用 paused-time 且实际运行非零目标用例。真实 wire/进程验收不可由静态检查替代。

## 独立验收 B1–B6 修复

| 项 | 修复 | 回归证据 |
| --- | --- | --- |
| B1 Receive 跨 D 误报 Settled | Receive 无条件返回已提交且 disposition=Process 的消息身份；loop 消费 HookStopIntent 时先登记该批 Process 身份，再返回携带 Act 身份的 `HookStopped`；Receive 后判定 D 到期则登记 Process 身份并返回 `SettlementDue`。两条路径都不新增 Reason | `receive_commit_crossing_deadline_hands_off_unprocessed_result`（真实 append barrier；D 在 append 期间届满，断言 HandedOff、tasks 为空、receipt 含 delivery ID） |
| B2 执行错误 + 交接写失败只留日志 | 新增 `ChildRunOutcome::HandoffFailed { cause, failure }`；保留原错误、`HandoffCommitError`、冻结 receipt 与 flush 附加失败 | `execution_error_with_handoff_write_failure_keeps_both_causes`；经真实 `run_sync_subagent` 的 `run_sync_handoff_failure_preserves_error_and_receipt` 断言父级同时看到原 model 错误、handoff_persistence_failed、flush 失败与 receipt，且 resume 被拒 |
| B3 成功回执不清旧失败 | `confirmed` 成功时 `settle_sync_delivery` 清除旧记录；失败继续记录 | `handoff_sync_child...` 拒绝×2→成功后 `sync_delivery_errors` 为空，并显式 `close_subagent_session_scope` 成功；`parent_close...` 持续失败保留记录 |
| B4 close 吞 flush 失败 | typed `FlushError::{WriterClosed, Persistence}`；真实持久化失败返回 Incomplete、保留 owner/binding；failure/pending receipt 计入未结义务，acquire 拒绝未确认交接 | `test_close_with_unconfirmed_history_flush_keeps_owner`、`test_unconfirmed_failure_and_pending_receipt_count_as_obligations` |
| B5 background 排空失败不阻断准入 | `drain_failure` 进入 `permit.finish(Err)`，与 sync 一致 | `background_drain_failure_blocks_resume` |
| B6 文件超 1000 行 | 查询/取消 handler 拆到 `session_tasks.rs`；`session_lifecycle.rs` 945 行；本轮新增 factory resume 用例拆到 `resume_owner_reuse_test.rs` | `wc -l` 全部 ≤1000；peri-acp 全库通过 |
| 第四轮主阻塞：已准入 Act tool result | `LoopResult::SettlementDue` 携带 `unprocessed_tool_calls`；runner 在非空时交接，receipt 记录 tool call ID；父结果 success=false | `admitted_act_tool_result_hands_off_instead_of_settled`（直接 run_child_until_terminal 断言 HandedOff）、`admitted_act_tool_result_parent_result_is_failure`（sync wrapper 父级 success=false） |
| 第三轮 #1 HandoffFailed 降级 | flush 失败不再把 `HandoffFailed` 替换为 `Failed { residual: None }`；`has_unsettled_obligations` 计入 failure/pending receipt；close 先关资源、再拒绝未确认交接；取消认领先写 Cancelled 再报告资源错误 | 上述真实 run_sync 用例；`close_lifecycle`/`resume_dispatch` 回归 |
| 第三轮 #2 resume 测试超限 | 新用例拆到 `resume_owner_reuse_test.rs`（61 行），`resume_cases_test.rs` 990 行 | `wc -l` |
| 第三轮 #3 Windows 可移植 | 进程树测试模块 `#[cfg(unix)]`，Windows 不编译/不运行该 fixture | 配置检查；本机无法执行 Windows |
| 建议 #8 缺能力分类 | 新增 `AgentError::ChildHandoffCapabilityUnavailable`；`MissingCapability` 不再归类为 persistence 失败 | `test_missing_handoff_capability_is_not_classified_as_persistence_failure` |
| 第四轮次要 #1 取消认领分类 | 仅 settlement/handoff unconfirmed 允许写 Cancelled；history persistence/resource 失败保持 Active | `close_failure_classification_allows_only_unconfirmed_settlement` + `resume_dispatch`/`close_lifecycle` |
| 第四轮次要 #2 缺能力文案 | 父级输出按 typed label 显示 capability 或 persistence | capability 分类用例断言 `public_message` |
| 第四轮次要 #3 flush 附加 | `ChildHandoffFailure.flush_error` 与 `SubagentFailure.handoff_flush_failure` 保留 flush 失败 | `run_sync_handoff_failure_preserves_error_and_receipt` 断言两者同时可见 |
| 第四轮文档 | 更新进程组测试名、删除重复行、修正 B2 表述、统一 design/spec 口径 | `git diff --check`；人工核对 |

补充覆盖：连续每秒 activity、D 边界前后 1ms 的完成/cancel 两种调度方向、真实 factory/claim resume、worker 释放 LLM/StageContext（Weak）、root 三层 owner 查询/取消/关闭、真实进程组（父 `sh` + 子 `sleep`）在 D 后存活并由正式取消清理整个进程组与输出管道。精确同刻调度无法由 tokio 定时器确定性注入，使用 ±1ms 边界夹逼并在报告注明。

## 本轮收口证据

1. HookStop：旧行为只返回 Completed、丢弃 Act 身份，空任务/空 MQ 时误报 Settled。`HookStopped` 现在携带身份，runner 无条件交接非空列表，window 到期路径也透传列表。故障注入恢复旧语义后，`post_tool_batch_stop_preserves_act_receipt` 实际失败为 Settled；恢复修复后通过。`hook_stop_after_act_hands_off_tool_result_and_parent_fails` 经 PostToolBatch middleware 与 sync wrapper 断言父 success=false、timed_out=false、模型仅调用一次。
2. max_iterations：旧行为保留错误但 residual=None，或交接 receipt 不含 Act 身份。身份由 AsyncContext 单一持有，Reason 成功才清除；runner 错误分支保留残留身份。故障注入丢弃身份后，`budget_exhaustion_preserves_act_receipt` 实际失败为 MaxIterationsExceeded(1)、residual=None；恢复修复后通过。
3. close 分类：原测试仅检查分类函数，不能证明 meta 状态。`cancel_running_claim_finalizes_only_after_resource_close_is_confirmed` 走真实 ResumeClaim/close worker：独立 history Store flush 失败而 meta Store 可写时仍保持 Active；资源关闭但执行结算未确认时写 Cancelled。此项补测试，不改生产分类规则。
4. canonical 交接：原测试未锁定 tool call 字段。上述两个直接 runner 用例断言实际执行一个工具、无其他义务，并按 canonical_message_id 查落库提醒，核对 receipt、正文、metadata、父级 notice 的 call-1 一致。此项补测试，不改 reminder 格式。
5. 遗留改动：保留五个文件的有效实现；修正 LoopState 与 AsyncContext 重复保存身份、HookStopped 分支的移动问题；测试从工具直接注入停止消息改为 after_tools_batch 窄能力投递。新增 fixture 的字段/导出路径编译错误、重复设置 host 与取消 token 断言错误已修正，最终结果见下。

## 第五轮复审收口

1. 主会话 HookStopped panic：`classify_loop_terminal` 旧逻辑只识别 `Completed`，`HookStopped` 落入 `unreachable!`。现在分类器把 `HookStopped` 映射为 Done/EndTurn/无 failure，MQ steering continuation 与 `Completed` 同条件。故障注入恢复旧分类后，`main_executor_hook_stop_finishes_done_without_panic` 实际以 `unreachable!` panic 失败；恢复修复后通过。`terminal_mapping_tests::hook_stopped_maps_to_success_no_failure` 覆盖纯分类。
2. 旧 HookStopIntent 污染新 run：新增 `MessageActivation::StopCurrentRun` / `MessagePolicy::stop_current_run`，`enqueue_stop_intent` 绑定当前执行；Receive 只把非 `Suppressed` 的 Stop 置为 `stop_requested`，旧执行 Stop 与旧 ContinueCurrentRun 一样显式抑制且不写 transcript。故障注入恢复“只按来源判断”后，`stale_stop_intent_does_not_stop_new_execution` 与真实 resume 用例均失败（后者 LLM 调用 0 次、等待 120 秒后失败）；恢复修复后均通过。`deadline_left_stop_intent_does_not_stop_resumed_execution` 覆盖 D 先到期留下 Stop、下一执行继续推理、同执行 Stop 仍返回 HookStopped。
3. 文档：design 的“旧 Stop 在 owner 开放下一次准入前排空”改为与实际一致的执行绑定/抑制描述；`mod.rs` 的 Completed 注释同步为 HookStopped 及主/子分流。

## 第五轮实际验证

- `test --locked -p peri-agent --lib`：1211 passed，0 failed，3 ignored。
- `test --locked -p peri-acp-types --lib`：661 passed，0 failed；`--doc`：6 passed，0 failed，2 ignored。
- `test --locked -p peri-acp --lib`：首轮 1038 passed、1 failed（`test_update_config_切换provider后cfg_provider更新`，`Failed to persist config`）；单测复跑通过，全量复跑 1039 passed、0 failed、1 ignored。该失败不涉及本次路径，按环境/并行持久化竞争记录，不宣称一次全绿。
- 定向：child_runner 34、run_sync 9、handoff 27、resource_owner 11、session::subagent 154、agent::stages 208、executor_helpers 60，均 0 failed。
- `build --locked -p peri-agent -p peri-acp`、`clippy --locked -p peri-agent -p peri-acp --lib -- -D warnings`：退出码 0。
- `peri-agent --doc`：15 passed，0 failed。
- `wc -l`：分支相对 main 的变更 Rust 文件最大 `agent/stages/mod.rs` 999 行，其余 ≤979；全部 ≤1000。

## 实现记录

- `412386e90`：阶段 1，定义 outcome/receipt、一次性窗口及目标契约。
- `ca20a81ec`：阶段 2，父 TaskManager 保留轻量 owner，原 MQ/manager 复用、执行 permit、可信子树查询/取消与根关闭接入。
- `da969a92c`：阶段 3，冻结交接快照、flush + 幂等 append、区分缺能力/写失败；同步状态提交失败显式返回，先提交再 Stop，事件排空失败阻止 owner resume。
- 后续提交：接入共享 D、loop/Receive 准入门、typed outcome 四条路径、wrapper retry 移交、owner close 消息结算、binding 释放、旧消息抑制及验收测试。

剩余风险：真实 MCP wire producer barrier、ACP 根关闭叠加 late result、TUI 手测与自有重启 fixture 未覆盖；精确同刻调度用 ±1ms 边界夹逼而非同一 tick 注入；`close_subagent_session_scope` 的 owner.close 会取消运行中 Shell（沿用显式关闭策略），与截止交接的“不取消 Shell”是不同触发路径。

## 实际验证

所有命令在指定 worktree 内执行，统一前缀 `./scripts/cargo-rmcp-patched.sh`。以下为本轮重跑结果；定向集合存在重叠，不相加作为独立用例数。

- `test --locked -p peri-agent --lib -- child_runner`：34 passed，0 failed（含 background/nested/deadline/process 子模块）。
- `test --locked -p peri-agent --lib -- run_sync`：9 passed，0 failed。
- `test --locked -p peri-agent --lib -- handoff`：27 passed，0 failed；精确 `agent::async_tasks::handoff::tests`：6 passed，0 failed。
- `test --locked -p peri-agent --lib -- resource_owner`：11 passed，0 failed。
- `test --locked -p peri-agent --lib -- session::subagent`：153 passed，0 failed。
- `test --locked -p peri-agent --lib`：1203 passed，0 failed，3 ignored。
- `test --locked -p peri-acp-types --lib`：660 passed，0 failed。
- `build --locked -p peri-agent -p peri-acp`：退出码 0。
- `clippy --locked -p peri-agent -p peri-acp --lib -- -D warnings`：退出码 0。
- `test --locked -p peri-agent --doc`：15 passed，0 failed。
- `test --locked -p peri-acp --lib`：1039 passed，0 failed，1 ignored。
- `wc -l`：分支相对 main 的变更 Rust 文件均 ≤1000；最大 `agent/stages/mod.rs` 993 行。新增修改的 `loop_iteration_test.rs` 956、`claim_preflight_test.rs` 482 行亦满足上限。
- 故障注入：HookStop 和预算身份丢失各复现 1 个失败，恢复修复后均通过；这两次预期失败不计入最终回归结果。初轮 fixture 编译和断言问题见上文，已修正。
- 未覆盖：真实 MCP wire producer barrier 叠加 ACP 根关闭、TUI 手测、自有进程重启用例。真实进程组（父 `sh` + 子 `sleep`）与输出管道清理已由 `real_blocking_shell_tree_survives_deadline_then_cancel_cleans_tree` 覆盖；Windows 仅做 `#[cfg(unix)]` 门控，本机无法执行 Windows。
