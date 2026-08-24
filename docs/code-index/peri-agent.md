# peri-agent 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-08-25（完整 frozen snapshot 贯穿 production stage）
> 依据：peri-agent/CLAUDE.md、docs/standards/architecture-contracts.md、源码

## 架构速览

- 数据流：`MessageQueue → Receive → Compact → Reason → Act → MessageQueue`
- 循环入口：`src/agent/stages/mod.rs:612` 的 `run_react_loop(StageContext, max_iterations) -> LoopResult`；Receive 是唯一退出口 + keepgoing 判定点
- 稳定不变量：`FrozenContext` 会话内不可漂移（ARC-FROZEN-001）；`BaseTool::is_direct()` 是工具可见性事实源（ARC-TOOLS-001）；`CompactConfig` 是 compact 阈值唯一事实源；中间件链序蓝本 `production_blueprint`（ARC-MIDDLEWARE-001）

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改 compact 触发阈值 | `peri-acp-types/src/compact.rs`（`CompactConfig` 事实源，`apply_env_overrides` 实现在 :325；`peri-agent/src/agent/compact_v2/config.rs` 仅 re-export；`peri-acp/src/host/compact_config.rs` 是配置加载调用方） | `CompactConfig` 字段：`auto_compact_threshold`（默认 0.95）、`micro_compact_threshold`（默认 0.75）、`smart_compact_enabled`（废弃恒 false） | budget < 0.75 跳过；≥ 0.75 走 Micro；Micro 收益不足且 budget ≥ auto_compact_threshold 时升级 Full；force=true 直接 Full。注意：调低 Full 阈值时 micro_compact_threshold 必须更低，否则先走 Skip |
| 改 compact 策略选择 | `src/agent/compact_v2/mod.rs` + `src/agent/stages/compact.rs` | `determine_compact_action(budget, config)`（mod.rs:102，Skip/Micro/Smart 选择）；`run_compact`（mod.rs:125 编排，Full 升级判定在 :233）；阶段入口 `stages/compact.rs::run_compact` | Full 升级判定以代码为准：`budget >= config.auto_compact_threshold` + `llm.is_none()` 守卫 + cache-aware 跳过；`planner.rs::CompactPolicy::force_full_threshold` 无消费点（遗留） |
| 改 Micro/Full 执行细节 | `src/agent/compact_v2/micro.rs` / `full.rs` | `micro_compact`；`re_inject_v2`、`extract_file_info`、`extract_skill_names` | Micro 按 round 截断（`micro_excluded_tools` 黑名单）；Full 走 `peri_model::Model` 摘要 + re-inject |
| 改循环退出 / keepgoing 判定 | `src/session/exec/executor.rs` + `src/agent/stages/mod.rs`（Receive 分支） | `executor.rs:130 is_keepgoing(&MessageContent)`；`run_session_loop`（executor.rs:221）；`run_react_loop` 退出判断（stages/mod.rs:647 `consumed_count == 0 && !has_tool_calls`）；判空底层 `peri-acp-types/src/messages/content.rs::is_empty`（:399） | 空白 prompt 须用 `MessageContent::is_empty()` 判空（禁止 trim 替代）；空历史 + 空白 prompt 时短路 `push_done`；keepgoing 不注入 recall；契约 ARC-KEEPGOING-001 |
| 改 turn fatal failure 分类/传递 | `src/session/exec/executor_helpers/v2_execute.rs` + `executor_helpers.rs` + `executor_helpers/collect.rs`；契约 DTO 在 `peri-acp-types/src/session.rs` | `classify_loop_terminal`；`ExecOutcome.failure` → `PromptResult.failure` | transcript flush 后只采样一次 cancel；单一纯分类器同时决定 Prompt stop reason、`TurnEnded`、fatal failure 与 cascade。Completed 为已提交成功；其他非成功结果中 cancel 优先；契约 ARC-EVENT-001 / ARC-CANCEL-001 |
| 加工具（direct/deferred） | trait 事实源 `peri-acp-types/src/tools.rs`；注册面 = middleware 的 `collect_tools()`；组装 `src/session/exec/stage_builder.rs::build_session_tool_view` | `BaseTool::is_direct()`（默认 **false** = deferred）；LLM 侧过滤点 `src/agent/stages/reason.rs` | 每 turn 先应用 middleware disabled 与 agent allow/disallow filter 构造 session-local 视图；`is_direct()=true` 直接进入 LLM tools，false 经 ToolSearch；元工具 direct 描述和 deferred resolver 也绑定同一视图，不得使用静态核心白名单；契约 ARC-TOOLS-001 |
| 改 PTC effective-target dispatch | `src/agent/stages/tool_dispatch.rs` + `peri-acp-types/src/tools.rs` | `StageEffectiveToolDispatcher::dispatch`；`collect_tool_results` | canonical `RunPtcCode` 是 deferred-only，经 `SearchExtraTools → ExecuteExtraTool` 进入执行；从当前 runtime tool snapshot canonical resolve，policy/HITL/event/tool card 投影 effective target，并复用 timeout/cancel；模型 assistant raw wrapper call 仅保留协议配对；direct tools 不受影响；旧 `run_code` 仅作搜索迁移关键词，不可执行 |
| 改 cancel 链路 | `src/agent/stages/mod.rs` + `src/session/exec/executor_helpers/v2_execute.rs` + `peri-acp-types/src/session.rs` | `run_stage`（stage-local `AgentError::Interrupted` 规范化）；`build_and_execute_agent_v2` / `classify_loop_terminal`；`cancel_cascade_agents` / `cancel_all_agents`；`CancelRequest` 在 `peri-acp-types/src/identity.rs` | stage 仍成对发射 `StageEnded(Error)`，loop 终态统一为 Interrupted；按 (session_id, turn_id, attempt_id) 三元组定位；幂等判定与终态归 Agent 层；clear_queue 默认 false；契约 ARC-CANCEL-001 |
| /compact 命令路径 | `src/session/exec/compact_pipeline.rs` | `run_compact(force=true)` → Full + re-inject | 编排：validate_inputs → resolve_auxiliary_model → run_v2_compact_with_cancel → assemble_compact_messages；取消返回 Cancelled |
| 改 LLM 调用链路 | `src/agent/stages/reason.rs` + `src/agent/model_bridge.rs` | `run_reason`；model_bridge 流式事件 v2 直发 | Reason：snapshot → LlmCallStart → before_model → generate（与 cancel 竞争）→ after_model → LlmCallEnd；事件契约 ARC-EVENT-001 |
| 改工具执行分发 | `src/agent/stages/act.rs` + `src/agent/stages/tool_dispatch.rs` | `run_act`；`dispatch_tools`（并发执行 + 写 transcript） | 有 tool_calls → 并发执行；无 → 最终回答 emit TextChunk + StateSnapshot |

## 子系统

### RCRA 阶段（src/agent/stages/）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 阶段循环入口/StageContext | stages/mod.rs | `run_react_loop`；`run_stage`；`StageContext::builder()`；`append_messages_to_transcript`。`run_stage` 先成对发射 `StageEnded`，再将 stage-local `AgentError::Interrupted` 规范化为 `LoopResult::Interrupted`；其他错误保持 `LoopResult::Error` |
| Receive（排空队列 + 退出判定） | stages/receive.rs | `run_receive`；`drain_all` + `consumed_count` |
| Compact（预算检查 + 触发压缩） | stages/compact.rs | `run_compact`；PreCompact/PostCompact hook |
| Reason（LLM 推理） | stages/reason.rs | `run_reason`；`is_direct` 过滤（:138） |
| Act（工具执行或回答） | stages/act.rs | `run_act`；emit TurnCompleted |
| 工具并发分发 | stages/tool_dispatch.rs | `dispatch_tools` |
| 阶段中间件 runner | stages/middleware_runner.rs | `run_before_compact`/`run_before_model`/`run_after_model` 等 |

### Compact v2（src/agent/compact_v2/）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 策略选择 + 触发编排 | compact_v2/mod.rs | `determine_compact_action`（:102）；`run_compact`（:125）；`CompactResult` |
| 压力计算与计划 | compact_v2/planner.rs | `plan_micro`、`ContextPressure`、`CompactPolicy`（force_full_threshold 无消费点） |
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
| 工具视图与主 Session 组装 | session/exec/stage_builder.rs | `build_session_tool_view`；`build_stage_context` 以同一 `FrozenSessionData` 构造 middleware projection、主 `SessionStore.frozen` 与 `SubagentHost`；空 CLAUDE/skills 保留 `Some("")` 冻结缺席语义，禁止 late-file 回读 |
| 子 Agent 创建（spawner/fork/bg/build_agent 收敛） | session/subagent/ | `SessionFactory::spawn_subagent`（factory.rs:36）；`SubagentSpawnConfig`/`SubagentChainAssembler`（types.rs:137/:87）；根 subagent.rs 仅 re-export（directives / factory / run_sync / background / v2_bridge / lifecycle / util） |
| 后台任务管理（bg shell，易失不持久化） | agent/async_tasks/ | `TaskManager`（manager.rs:28，per-session 聚合）；`BackgroundTaskRegistry`（registry.rs:105）；shell 执行 `shell_command` / `kill_process_group` / `parse_timeout`（shell.rs:109/:25/:201）；根 async_tasks.rs 仅 re-export |
| 中间件链装配 | session/factory.rs | `production_blueprint`（链序事实源，装配实现在 peri-middlewares/src/assembly.rs） |
| 消息队列 | session/queue.rs | MessageQueue 入队/排空 |
| Transcript 标记 API | session/transcript.rs | `visible_messages()`；excluded 标记过滤 |
| Turn/会话状态 | session/turn.rs、session/runtime.rs | TurnId、AgentRuntime |

### 工具系统

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 工具 trait 事实源 | `peri-acp-types/src/tools.rs` | `BaseTool`（:146）；`is_direct()` 默认 false（:199） |
| 工具注册面 | middleware `collect_tools()`（`peri-agent/src/middleware/trait.rs:60`，13 处实现） | 新工具由中间件提供；包装层透传 is_direct |
| deferred 搜索/执行代理 | `peri-middlewares/src/tool_search/` | `middleware.rs`（基于 local tool view 构建索引并刷新元工具描述）、`search_tool.rs`、`execute_tool.rs`、`tool_index.rs`、`core_tools.rs`（调用解析与 direct 描述 helper） |
| 链装配 | `peri-middlewares/src/assembly.rs` | `ChainSlot::ToolSearch`（:514）；`ExecuteExtraToolResolver` 注入（:874） |

## 跨模块契约（指向 architecture-contracts.md，不复制正文）

- ARC-BOUNDARY-001：TUI 交互主路径经 ACP，不得直驱 Agent 运行时
- ARC-CANCEL-001：cancel 三元组定位，Agent 持有终态判定
- ARC-EVENT-001：事件链路单事实源 Agent 发射 → ACP 映射 → TUI 消费；禁止 v1 中间态
- ARC-FROZEN-001：frozen 数据会话内不可漂移，SubAgent 复用
- ARC-TOOLS-001：`is_direct()` 自声明可见性
- ARC-KEEPGOING-001：空白 prompt = 继续跑 loop
- ARC-MIDDLEWARE-001：中间件链序是行为契约，链序蓝本 `production_blueprint`
