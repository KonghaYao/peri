# peri-agent 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-11（模块职责拆分与 compact/历史恢复修复合并）
> 依据：peri-agent/CLAUDE.md、docs/standards/architecture-contracts.md、源码

## 架构速览

- 数据流：`MessageQueue → Receive → Compact → Reason → Act → MessageQueue`
- 循环入口：`src/agent/stages/mod.rs:612` 的 `run_react_loop(StageContext, max_iterations) -> LoopResult`；Receive 是正常队列耗尽退出判定点与 keepgoing 队列语义入口，cancel 与 stage error/interruption 也可在其他控制流位置结束循环
- 稳定不变量：`FrozenContext` 会话内不可漂移（ARC-FROZEN-001）；`BaseTool::is_direct()` 是工具可见性事实源（ARC-TOOLS-001）；`CompactConfig` 是 compact 阈值唯一事实源；中间件链序蓝本 `production_blueprint`（ARC-MIDDLEWARE-001）

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改用户待发送、立即发送与停止恢复 | `src/session/user_input_mailbox.rs` + `src/agent/stages/receive.rs` + `src/session/exec/executor_helpers/v2_execute.rs` | `UserInputMailbox::{enqueue,dispatch,take_back,reserve_run,attach_attempt,finish_attempt,stop_attempt}`；`run_receive` | 跨 turn 唯一 owner 保留待发区与幂等回执，selected 交接 MQ；Stop 只回收未领取项，自然成功后释放普通项，Delivered 走本轮 render FIFO；契约 ARC-BOUNDARY-001 / ARC-CANCEL-001 / ARC-EVENT-001 |
| 改 compact 失败后的历史恢复 | `src/session/transcript.rs` + `src/session/exec/executor_helpers/{v2_execute,intercept}.rs` + `peri-acp/src/host/prompt.rs` | `CompactionCommitState`；Phase 8 flush；`intercept_immediate_command`；`finish_prompt_turn` | 取消/模型错误仍返回可信 canonical snapshot；writer 失败或 compact commit uncertain 停止使用热状态、保留已提交磁盘内容供冷恢复，禁止按新增 ID 回滚已提交摘要；ARC-COMPACT-001 |
| 改 Full 后预算恢复判定 | `src/agent/stages/compact_progress.rs` + `src/agent/stages/reason.rs` | `CompactBudgetRecovery::{record_full_applied,begin_request,observe_response}` | 无新 Human/Tool 工作时，两次成功 Full 后的对应实际请求 usage 仍高压则返回 `CompactBudgetUnrecovered`；Reminder/AI 不重置次数，缺失/零/过期 usage 不作证据；cancel 优先；ARC-COMPACT-001 |
| 改 compact 触发阈值 | `peri-acp-types/src/compact.rs`（`CompactConfig` 事实源，`apply_env_overrides` 实现在 :325；`peri-agent/src/agent/compact_v2/config.rs` 仅 re-export；`peri-acp/src/host/compact_config.rs` 是配置加载调用方） | `CompactConfig` 字段：`auto_compact_threshold`（默认 0.95）、`micro_compact_threshold`（默认 0.75）、`smart_compact_enabled`（deprecated、默认 false，但运行时仍尊重 true） | budget < `micro_compact_threshold` 跳过；达到该阈值后默认走 Micro，显式启用 deprecated Smart 时走 Smart；Micro 收益不足且 budget ≥ `auto_compact_threshold` 时升级 Full；force=true 直接 Full。注意：调低 Full 阈值时 micro 阈值必须更低，否则先走 Skip |
| 改 compact 策略选择 | `src/agent/compact_v2/mod.rs` + `src/agent/stages/compact.rs` + `src/agent/token.rs` | `determine_compact_action(budget, config)`（mod.rs:102，Skip/Micro/Smart 选择）；`run_compact`（mod.rs:125 编排）；阶段入口 `stages/compact.rs::run_compact`；`TokenTracker::pressure_sample_key` | Micro 计划收益不足且 `budget_pct >= auto_compact_threshold`、`reclaim_target > 0` 时先提交 Micro 再尝试 Full；自动 Compact 以有效 provider usage generation + tool-growth generation 标识压力样本，同一样本只尝试一次，新 usage 或工具增长可重新评估；LLM 缺失由 Full 执行阶段报 `CompactNoLlm`；cache-aware 仅是 Micro 分支的提前跳过条件；`planner.rs::CompactPolicy::force_full_threshold` 无消费点（遗留） |
| 改事件契约、通道和协议转换 | `peri-acp-types/src/event_v2/{types,bus,executor_mapping}.rs`；`src/agent/events_v2.rs` | EventBus / 三层事件 / `*_event_to_executor` | 事实源及纯契约测试在 types crate，Agent 仅 re-export；`src/agent/events_v2_test.rs` 保留公共路径和 prelude 类型 identity 检查；消费者顺序与收尾仍归各 forwarder（ARC-EVENT-001） |
| 改子 Agent 事件转发与关闭排空 | `src/agent/subagent_event_forwarder.rs` | `spawn_subagent_event_forwarder` | observe → render → state 的 biased 优先级保持；observe Closed 只停用自身分支，三通道全部关闭且排空才退出；保留 child source 注入与生命周期事件去重；`subagent_event_forwarder_test.rs` 覆盖 producer 先关闭后的缓冲交付（ARC-EVENT-001） |
| 改 Workflow agent 装配与收尾 | `src/agent/workflow/agent.rs` | `WorkflowAgentExecutor::execute`、`await_workflow_forwarder` | public Context/API 与 frozen/tool 注入入口保留；loop → drop EventBus → await forwarder → 最终统计/结果 → telemetry terminal；编排回归在 `agent/agent_test.rs` |
| 改 Workflow agent 观测与进度 | `src/agent/workflow/agent/observation.rs` | `WorkflowObservation::{handler,report_model,snapshot}` | 单一锁持有 tool count、output usage 与最后模型；事件先累计/发送 progress，再调用 Langfuse；有效模型早报保持 None 计数；`observation_test.rs` 覆盖进度先后与关闭排空后统计 |
| 改 Workflow agent 返回值与遥测终态 | `src/agent/workflow/agent/result.rs` | `project_run_result`、`completed_result`、`ProjectedResult::telemetry_outcome` | forwarder failure 优先于 loop 终态；schema 只校验输出，成功仍返回 JSON string；无 usage 时保留字节长度 token 估算，缺模型回退有效模型；`result_test.rs` 覆盖 wire、schema、失败优先和取消 |
| 改 compact 展示事件 | `src/agent/stages/compact.rs` + `peri-acp-types/src/event_v2/{types,executor_mapping}.rs` + `src/session/exec/{compact_pipeline,events}.rs` | `ObserveEvent::CompactStarted` / `MessagesCompacted`；`observe_event_to_executor`；`emit_compact_started` / `emit_compact_completed` | 自动 compact 将 strategy、受影响消息数、估算节省 token、files/skills 映射到 `ExecutorEvent::CompactCompleted`；手动 `/compact` 从 pipeline 发送同一展示载荷；下游经 ACP 单路径投递，契约 ARC-EVENT-001 |
| 改 Micro/Full 执行细节 | `src/agent/compact_v2/{micro,projection,full}.rs` + `src/session/{transcript.rs,exec/executor_helpers/v2_execute.rs}` | `micro_compact`；`render_llm_view`；`full_compact_inner`；`re_inject_v2`；`MessageTranscript::{with_own_payloads,with_ancestor_payloads}` | Micro 不原地截断 transcript，而是持久化 message-level projection directive，后续模型视图由 `render_llm_view` 投影工具结果；普通 root Agent 与独立复制后的 ACP fork 跨 turn 历史属于可压缩 own region；显式继承上下文才使用只读 ancestor boundary；子会话以版本化 InheritedContext 冻结 payload 与 flags，load/resume 保持 ancestor/own 边界，普通 setter 与 lifecycle 拒绝改写祖先；Full 仅按本轮新 `excluded` transition 统计 affected messages，文件 re-inject 只从 Full 前可见 `Read` 来源收集，新 `Read` 同路径仍可注入更新内容；精确载荷污染风险见 `spec/issues/2026-09-09-p0-micro-compact-edit-write-context-corruption.md`，compact churn 调查见 `spec/issues/2026-09-10-p0-full-micro-compact-churn.md` |
| 改 Goal 自动接续与状态事件 | `peri-middlewares/src/goal_middleware.rs` + `src/agent/stages/act.rs` + `src/session/exec/stage_builder.rs` + `peri-acp-types/src/goal.rs` + `peri-acp-types/src/event_v2/types.rs` | `GoalMiddleware::after_agent`；`GoalController::increment_continuation`；`emit_goal_snapshot`；`StateEvent::GoalSnapshot` | 仅 active Goal 且无既有 `block_continue` 时记录一次主动接续并设置 `goal_active`；Act 每轮结束（含错误返回）发 session Goal 快照，下游经 ACP 单路径投影；契约 ARC-EVENT-001 / ARC-MIDDLEWARE-001 |
| 改循环退出 / keepgoing 判定 | `src/session/exec/executor.rs` + `src/agent/stages/mod.rs`（Receive 分支） | `executor.rs:130 is_keepgoing(&MessageContent)`；`run_session_loop`（executor.rs:221）；`run_react_loop` 正常退出判断（stages/mod.rs:647 `consumed_count == 0 && !has_tool_calls`）；判空底层 `peri-acp-types/src/messages/content.rs::is_empty`（:399） | 空字符串 / 空 blocks / 空 raw 内容须用 `MessageContent::is_empty()` 判空且禁止 trim 替代（纯空白字符串不算空）；空历史 + 空内容 prompt 时短路 `push_done`；keepgoing 不注入 recall；cancel 与 stage error/interruption 可在 Receive 正常退出点之外终止；契约 ARC-KEEPGOING-001 |
| 改 turn fatal failure 分类/传递 | `src/session/exec/executor_helpers/v2_execute.rs` + `executor_helpers.rs` + `executor_helpers/collect.rs`；契约 DTO 在 `peri-acp-types/src/session.rs` | `classify_loop_terminal`；`internal_failure_terminal`；`ExecutionFailure::from_agent_error`；`ExecOutcome.failure` → `PromptResult.failure` | transcript flush 后只采样一次 cancel；forwarder JoinError 保留到 Phase 9，在提取 transcript/recall/compaction 后覆盖为 Internal terminal；单一终态同时决定 Prompt stop reason、`TurnEnded`、fatal failure 与 cascade；LLM/provider failure 保留脱敏限长原意和可选 HTTP status，其他内部错误使用安全文案；Completed 为已提交成功，其他非成功结果中 cancel 优先；契约 ARC-EVENT-001 / ARC-CANCEL-001 |
| 加工具（direct/deferred） | trait 事实源 `peri-acp-types/src/tools.rs`；注册面 = middleware 的 `collect_tools()`；组装 `src/session/exec/stage_builder/tools.rs::build_session_tool_view` | `BaseTool::is_direct()`（默认 **false** = deferred）；Reason publication 在 `src/agent/stages/reason.rs`，专用 hook runner 在 `middleware_runner.rs::run_before_reason_catalog`；Dynamic MCP projection holder 由 `StageBuildInput::dynamic_mcp_projection` 从 session owner 透传 | 每 turn 先应用 middleware disabled 与 agent allow/disallow filter 构造 session-local 视图；动态 refresh 后按 working map swap → `before_reason_catalog` → `before_model` → pin 发布，ToolSearch 在专用 hook 内重绑 Search index 与 Execute resolver；Discover/resource 的 projection lease 跨 stage build 复用并由 session close 释放；不得使用静态核心白名单或等待下一 turn；契约 ARC-TOOLS-001 |
| 改 PTC effective-target dispatch | `src/agent/stages/tool_dispatch/{effective_dispatcher,execution}.rs` + `peri-acp-types/src/tools.rs` | `StageEffectiveToolDispatcher::dispatch`；`collect_tool_results` | canonical `RunPtcCode` 是 deferred-only，经 `SearchExtraTools → ExecuteExtraTool` 进入执行；从当前 pinned catalog canonical resolve，policy/HITL/event/tool card 投影 effective target，并复用 timeout/cancel；嵌套调用不写 transcript 或重复执行外层 batch hook/失败计数；模型 assistant raw wrapper call 仅保留协议配对；direct tools 不受影响；旧 `run_code` 仅作搜索迁移关键词，不可执行 |
| 改 cancel 链路 | `src/agent/stages/mod.rs` + `src/session/exec/executor_helpers/v2_execute.rs` + `peri-acp-types/src/session.rs` | `run_stage`（stage-local `AgentError::Interrupted` 规范化）；`build_and_execute_agent_v2` / `classify_loop_terminal`；`cancel_cascade_agents` / `cancel_all_agents`；`CancelRequest` 在 `peri-acp-types/src/identity.rs` | stage 仍成对发射 `StageEnded(Error)`，loop 终态统一为 Interrupted；按 (session_id, turn_id, attempt_id) 三元组定位；幂等判定与终态归 Agent 层；clear_queue 默认 false；契约 ARC-CANCEL-001 |
| /compact 命令路径 | `src/session/exec/compact_pipeline.rs` | `run_compact(force=true)` → Full + re-inject | 编排：validate_inputs → resolve_auxiliary_model → run_v2_compact_with_cancel → assemble_compact_messages；取消返回 Cancelled |
| 改 LLM 调用链路 | `src/agent/stages/reason.rs` + `src/agent/model_bridge.rs` | `run_reason`；`AgentModelBridge::build_request`；model_bridge 流式事件 v2 直发 | Reason：snapshot → LlmCallStart → before_model → generate（与 cancel 竞争）→ after_model → LlmCallEnd；bridge 每个 ModelRequest 同步读取一次当前 middleware prompt contribution，与 frozen base request-local 组合且不累加；事件契约 ARC-EVENT-001 |
| 改工具执行分发 | `src/agent/stages/act.rs` + `src/agent/stages/tool_dispatch.rs` + `tool_dispatch/execution.rs` | `run_act`；`dispatch_tools`；`collect_tool_results` | 外层一次 staging/commit 后计入含解析失败结果的 tool-growth，再执行 after_tools_batch；PTC 内部调用不重复结算；私有执行管线保持审批 → yield → 并发完成即发 ToolEnded → after_tool → 后处理顺序，after_tool 看不到本轮待提交消息；ToolStarted 与实际执行均使用审批后参数，transcript 保留模型原始调用用于配对 |
| 改 middleware 状态能力 / 消息修改 | `src/middleware/{capabilities,state}.rs` + `src/agent/agent_context.rs` + `src/agent/stages/middleware_runner.rs` | `BeforeAgentState` / `InputBatchState::input_message_ids` / `BeforeToolState` / `AfterToolState` / `AfterAgentState`；`MiddlewareState::replace_message`；`AgentContext::from_stage` / `reconcile_to_transcript`；`run_before_agent` | hook 不再暴露 cwd/step setter、store/thread 或无法回写的 token/context 快照；首次 Receive 的本批用户 ID 只提供给 before_agent，空批次不重读历史；替换按稳定 MessageId 查找，不增删/重排，before_agent 成功或 Err 后均 reconcile；StateView 无可变 queue/catalog；队列和目录分别由 QueueState/CatalogState 提供，before_model 保留消息追加，其他 hook 无输入替换能力 |

## 子系统

### RCRA 阶段（src/agent/stages/）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 阶段循环入口/StageContext | stages/mod.rs | `run_react_loop`；`run_stage`；`StageContext::builder()`；`append_messages_to_transcript`。`run_stage` 先成对发射 `StageEnded`，再将 stage-local `AgentError::Interrupted` 规范化为 `LoopResult::Interrupted`；其他错误保持 `LoopResult::Error` |
| Receive（排空队列 + 退出判定） | stages/receive.rs | `run_receive`；`drain_all` + `consumed_count` |
| Compact（预算检查 + 触发压缩） | stages/compact.rs | `run_compact`；PreCompact/PostCompact hook |
| Reason（LLM 推理） | stages/reason.rs | `run_reason`；只恢复已提交 projection（与自动 compact 开关独立），无 directive 使用 canonical；验证 Full 后真实 usage |
| Act（工具执行或回答） | stages/act.rs | `run_act`；emit TurnCompleted |
| 工具批次提交 | stages/tool_dispatch.rs | `dispatch_tools`（:73）；ID/target 解析、原子转录、batch hook 与错误收敛 |
| 共享调用执行 | stages/tool_dispatch/execution.rs | `collect_tool_results`（:51）；审批/并发/结算，参数复用 `tools::normalize_params` |
| PTC 有效调用适配 | stages/tool_dispatch/effective_dispatcher.rs | `StageEffectiveToolDispatcher::dispatch`（:34）；同一 pinned catalog，内层事件 ID 关联外层调用 |
| 阶段中间件 runner | stages/middleware_runner.rs + agent_context.rs | `run_before_agent` 结束后（含 Err）drain recall 并将稳定 ID replacement reconcile；`run_before_model`/`run_after_model` 保留追加消息双写路径 |

### Compact v2（src/agent/compact_v2/）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 策略选择 + 触发编排 | compact_v2/mod.rs | `determine_compact_action`（:102）；`run_compact`（:125）；`CompactResult` |
| 压力计算与计划 | compact_v2/planner.rs + projection.rs | `plan_micro` 只规划 visible own history；`estimate_projection_chars` 跳过 excluded，已隐藏历史无重复收益 |
| Micro 执行（按 round 截断） | compact_v2/micro.rs | `micro_compact` |
| Smart 执行（废弃中，恒 false） | compact_v2/smart.rs | `smart_compact` |
| Full 执行 + re-inject | compact_v2/full.rs | `re_inject_v2`、`extract_file_info`、`extract_skill_names` |
| 配置 re-export | compact_v2/config.rs | `CompactConfig`（事实源 peri-acp-types）、`CONTINUATION_HINT` |
| 摘要 prompt 模板 | compact_v2/descriptions/ | summary_system_prompt.md / summary_user_prompt.md |

### 会话与执行（src/session/）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 执行编排、keepgoing、短路 | session/exec/executor.rs | `is_keepgoing`（:130）；`run_session_loop`（:221）；空历史短路 push_done；辅助构建拆至 executor/（context / agent_build / prediction 子模块） |
| v2 装配与循环驱动 | session/exec/executor_helpers/v2_execute.rs | `build_and_execute_agent_v2`；`V2ExecuteRequest.frozen_session` → `StageBuildRequest.frozen_session` 单一 snapshot；根 executor_helpers.rs 声明并 re-export intercept / event_pump / collect / bg_fork 子流程 |
| /compact 命令执行体 | session/exec/compact_pipeline.rs | `run_compact(force=true)` |
| Stage 装配顺序与公开输入 | session/exec/stage_builder.rs | `StageBuildInput` / `build_stage_context`；保留主 Session → turn/EventBus → 父身份/host → collect_tools/catalog → StageContext 的顺序 |
| 模型缓存与生产链投影 | session/exec/stage_builder/agent.rs | `build_agent` / `TurnAssembly` / `project_assembly`；retry handler 先于模型工厂更新；生产 chain 包装一次，bridge provider 与 StageContext clone 同一 `Arc<MiddlewareChain>`；空 CLAUDE/skills 保留 `Some("")` 冻结缺席语义 |
| 主 Session 与后台 owner | session/exec/stage_builder/session_setup.rs | `build_session`；同一 `FrozenSessionData` 构造 `SessionStore.frozen`，激活 persistence；session 级 cron bridge 与 print 级 CronOwner 分支、取消优先级不变 |
| 父身份与子任务宿主 | session/exec/stage_builder/subagent_setup.rs | `attach_subagent_host` / `SubagentDependencies`；借用原 owner，移动后台事件发送端并注入同一冻结数据；必须早于 middleware `collect_tools` |
| 工具视图与目录注册 | session/exec/stage_builder/tools.rs | `build_session_tool_view` / `register_tool_catalog`；disabled 剔除后 merge 当前链工具，同名有状态工具覆盖本地条目，不写宿主共享表；动态 catalog 注册失败沿 `StageBuildError` 返回 |
| Stage 可选依赖 | session/exec/stage_builder/dependencies.rs | `configure_stage` / `StageDependencies`；按原顺序注入 goal/error/compact/idle/hook；inbox handle 优先 session 级 inbox，再回退 async owner |
| 子 Agent 创建入口与新 thread 注入 | session/subagent/factory.rs + factory/spawn.rs | `SessionFactory::spawn_subagent` / `spawn_subagent_impl`；公开入口不变，spawn 在新 thread 执行前持久化所选父 canonical payload/flags 快照；identity 和 fork prompt 仍属 child own |
| 子 Agent 恢复与状态 claim | session/subagent/factory/resume.rs + factory/claim.rs | `resume_subagent_impl` / `ResumeClaim`；同一 worker 顺序完成 active 与终态写入；准备取消恢复旧状态（sync 返回原线程中断结果），sync 执行 Drop 写 cancelled，bg 注册成功后移交；继承快照/own payload/flags 的恢复与交叠校验全部受 claim 保护；保持 thread 身份与 own 尾部 tool-call 截断 |
| 子 Agent 冻结派生与共享装配 | session/subagent/factory/context.rs | `inherited_frozen_context` / `derive_cancel_token` / `build_subagent_session_v2`；父值优先与 Cascade/Independent 派生一致，按 ancestor → own → flags 装载，再绑定 persistence；消息注入归调用流程 |
| 后台任务管理（bg shell，易失不持久化） | agent/async_tasks/ | `TaskManager`（manager.rs:28，per-session 聚合）；`BackgroundTaskRegistry`（registry.rs:105）；shell 执行 `shell_command` / `kill_process_group` / `parse_timeout`（shell.rs:109/:25/:201）；根 async_tasks.rs 仅 re-export |
| 中间件链装配 | session/factory.rs | `production_blueprint`（链序事实源，装配实现在 peri-middlewares/src/assembly.rs） |
| 消息队列 | session/queue.rs | MessageQueue 入队/排空 |
| Transcript 标记与 canonical 历史 | session/transcript.rs + transcript_test.rs | `persisted_payloads` 保留 message/reminder 类型和 ID；`visible_model_messages` 单独生成模型投影，`visible_messages` 跳过 reminder 与 excluded；`test_compaction_reload_preserves_canonical_reminder_once` 经真实 compact 提交与 SQLite 重载验证 reminder 顺序、身份和单次投影 |
| Transcript 持久化任务 | session/transcript/persistence.rs + transcript.rs | `run_writer`；FIFO Append batching / barrier / sticky failure / Shutdown；失败态撤销批处理 deadline，只等待新操作且不重试失败批次；`with_persistence` 只绑定并启动唯一 writer，compaction store→memory 提交仍归 `MessageTranscript` |
| Turn/会话状态 | session/turn.rs、session/runtime.rs | TurnId、AgentRuntime |

### 工具系统

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 调用解析与参数归一化 | `src/tools/invocation.rs` | `DirectToolInvocationResolver::resolve/resolve_target`；同一 Arc 多 key 去重、不同 target 的歧义拒绝；`normalize_params` 是唯一实现，同时声明 alias/canonical 的 schema 不改写字段 |
| 工具 trait 事实源 | `peri-acp-types/src/tools.rs` | `BaseTool`（:146）；`is_direct()` 默认 false（:199） |
| 工具注册面 | middleware `collect_tools()`（`peri-agent/src/middleware/trait.rs:60`，13 处实现） | 新工具由中间件提供；包装层透传 is_direct |
| deferred 搜索/执行代理 | `peri-middlewares/src/tool_search/` | `middleware.rs`（基于 local tool view 构建索引并刷新元工具描述）、`search_tool.rs`、`execute_tool.rs`、`tool_index.rs`、`core_tools.rs`（调用解析与 direct 描述 helper） |
| 链装配 | `peri-middlewares/src/assembly.rs` + `assembly/workflow.rs` | 根 `ChainSlot::ToolSearch`；workflow 工厂 `build_tool_resolver` 注入 `ExecuteExtraToolResolver` |

## 跨模块契约（指向 architecture-contracts.md，不复制正文）

- ARC-COMPACT-001：visible own 收益、真实 usage、durable compact 恢复与 inherited provenance
- ARC-BOUNDARY-001：TUI 交互主路径经 ACP，不得直驱 Agent 运行时
- ARC-CANCEL-001：cancel 三元组定位，Agent 持有终态判定
- ARC-EVENT-001：事件链路单事实源 Agent 发射 → ACP 映射 → TUI 消费；禁止 v1 中间态
- ARC-FROZEN-001：frozen 数据会话内不可漂移，SubAgent 复用
- ARC-TOOLS-001：`is_direct()` 自声明可见性
- ARC-KEEPGOING-001：空白 prompt = 继续跑 loop
- ARC-MIDDLEWARE-001：中间件链序是行为契约，链序蓝本 `production_blueprint`
- ARC-MIDDLEWARE-CAPABILITY-001：阶段能力接口 `middleware/capabilities.rs`；执行适配与回写入口 `agent/stages/middleware_runner.rs`
