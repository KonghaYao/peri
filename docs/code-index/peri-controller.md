# peri-controller 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-11。
> 依据：`docs/standards/architecture-contracts.md`、manifest、源码与契约测试；无 crate 级 CLAUDE.md。

## 架构速览

Controller 是控制面宿主：定位 Runtime / Resources、转发会话操作、发布协议化前事件。
取消策略与业务终态归 Agent，登记和销毁编排归 Runtime；Controller 不持有第二份 session 表。
Runtime、Resources 和装配端口只在消费 `self` 的 builder 中替换，发布后按只读字段使用。
`events_rx` 保留排空队列所需的 mutex；Runtime 自己持有其登记和事件序列的锁。

Langfuse 是同一事件分支上的旁路消费者。bridge 负责 v1/v2 转换、句柄配对与诊断计数，
`LangfuseTracer` 持有单轮观测状态，`SubagentRegistry` 是子 agent 归属与生命周期的唯一 owner。
子模块通过同一 owner 的借用实现注册、缓存和收尾，不另建注册表。

依赖方向为 Controller → Runtime / Resources / peri-acp-types；manifest 中的 peri-agent、
peri-model 和 langfuse-client 是现行 Langfuse 适配依赖，不能由索引推断已经解耦。

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 转发取消三元组 | `peri-controller/src/controller.rs` | `Controller::cancel`:339 | 原样交给 Runtime；未知 session 包装为 CancelFailed，策略和幂等判定归句柄实现（ARC-CANCEL-001） |
| 发布业务事件 | `peri-controller/src/controller.rs` | `publish_event`:438、`publish`:426、`publish_message` | Runtime 补打身份后先投弹出队列再广播；无法补打时使用发射方身份，不 panic |
| 订阅与排空事件 | `peri-controller/src/controller.rs` | `subscribe`:465、`pop_events`:472、`Subscription::recv`:131、`try_recv`:142 | 队列有界满丢弃，广播 Lagged 可恢复；退订只 drop receiver，无额外簿记 |
| 注册与定位会话 | `peri-controller/src/controller.rs` | `register_session`:327、`run_session`:311、`session_ids`:350、`contains_session`:355 | register_or_replace 归 Runtime；Controller 只转发，不解释执行结果 |
| 等待、销毁或注入会话 | `peri-controller/src/controller.rs` | `join_session`:364、`destroy_session`:385、`submit_input`:406 | 捕获 Runtime Arc 后调用；销毁返回的已补打事件经 publish 按顺序双投递 |
| 注入部署端口 | `peri-controller/src/controller.rs` | `Controller::new`:198、`with_runtime`:216、`with_resources`:223、`with_mcp_pool`、`with_cron_scheduler`、`with_tool_search`、`with_lsp_servers` | builder 消费 self 后赋值；对应 pick 方法克隆句柄/配置，不引入共享可写配置 |
| 调整启动参数 | `peri-controller/src/controller.rs` | `AgentRef`:49、`LiteParams`:70 | 仅承载定义引用、cwd、初始消息和工具；消费与执行归 Agent |
| 配置与创建 Langfuse 批处理 | `peri-controller/src/langfuse/session.rs` + `langfuse-client/src/{config,batcher}.rs` | `LangfuseSession::new`；`Batcher::try_new` | 生产构造在 spawn 前拒绝零容量/零间隔/容量超限，沿既有 Option 路径返回 None 并记录安全诊断；重试参数只归 LangfuseClient，Batcher legacy max_retries 不覆盖；`session_test.rs` 覆盖非法配置 |
| 关闭部署 Langfuse | `peri-controller/src/langfuse/session.rs` | `LangfuseSession::new_owned`（:50）；`LangfuseShutdownOwner::shutdown`（:43）；`LangfuseSession::shutdown`（:63） | fresh deployment 得到不可克隆的关闭权限；只转发唯一 Batcher join，安全区分 HTTP/worker 失败，turn-facing SessionLike 仍只提供 flush（ARC-HOST-SHUTDOWN-001） |
| 修改 Langfuse 事件入口 | `peri-controller/src/langfuse/bridge.rs` | `LangfuseBridge`:34、`process_event`:92 | 保留统一事件分发与 tracer 锁，trait 入口先持有该 bridge 的 stage 表锁 |
| 修改 v1 事件转换 | `peri-controller/src/langfuse/bridge/v1_conversion.rs` | `UnifiedLangfuseEvent::from_executor_event` | 无映射事件返回 None；v1 LLM 使用 MAIN_AGENT_KEY，工具优先保留 source_agent_id |
| 修改 v2 事件转换 | `peri-controller/src/langfuse/bridge/v2_conversion.rs` | `from_render_event`、`from_observe_event` | 保留 agent identity、request_id、usage 和 compact 语义数据，不修改事件来源 |
| 维护 bridge stage/middleware 配对 | `peri-controller/src/langfuse/bridge/lifecycle.rs` | `start_stage`、`finish_stage`、`finish_middleware` | stage 按 agent 匹配，找不到时领取 tracer 重放句柄；保留 tracer 锁释放/重取边界 |
| 查看 bridge 收到的子 agent 事件 | `peri-controller/src/langfuse/bridge/lifecycle.rs` | `SubagentTelemetry::on_start`、`on_stop` | 单锁集合与计数只描述此 bridge 收到的事件，不决定观测 parent 或关闭 |
| 调整 turn 开始与终止 | `peri-controller/src/langfuse/tracer/turn.rs` | `on_turn_start`:15、`on_turn_end`:84 | stage → generation → 主工具批次 → 子 agent → error → agent-run 同步入队，最后返回 spawn 的 flush 句柄 |
| 修复遗留 stage/generation | `peri-controller/src/langfuse/tracer/turn_fallback.rs` | `close_stage_parents`、`close_abandoned_generations`、`GenerationFallbackStatus::for_outcome` | 先补 parent，再按终态或稳定失败分类补 child；保留未解析 owner 的诊断元数据 |
| 修改错误遥测 | `peri-controller/src/langfuse/tracer/turn_error.rs` | `emit_error_turn`、`failure_error_class`、`failure_output` | 未采样 fatal 先创建合成 parent，再发 ErrorTurn；只写稳定错误分类和允许的 HTTP 状态 |
| 修改边界错误 | `peri-controller/src/error.rs` | `ControllerError`、`SubscriptionError` | Controller 错误保留 Runtime 来源；广播错误区分 Lagged 和 Closed |

## 子 agent 注册与生命周期

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 唯一状态 owner | `peri-controller/src/langfuse/tracer/registry.rs` | `SubagentRegistry`:55、`ActiveSubagent`:29；持有 by_agent_id、invocations、pending_starts、gate_cache |
| 内容归属 | `peri-controller/src/langfuse/tracer/registry.rs` | `ownership`:116、`observation_id_of`:130、`is_main_agent`:96；事件侧 agent_id 查表，未注入主身份才启用既有 fallback |
| 内部数据与诊断 | `peri-controller/src/langfuse/tracer/registry/types.rs` | `SubagentStatus`、`IncompleteReason`、`GateEvent`、`SubagentStartOutcome`、`ClosedSubagent`；registry 根 re-export 内部路径 |
| 有界乱序缓存 | `peri-controller/src/langfuse/tracer/registry/gate.rs` | `try_gate`:10、`take_gated_events`:45；容量 64，满时逐出旧事件并拒绝新事件，重放保留原次序 |
| 父调用与子 Start 关联 | `peri-controller/src/langfuse/tracer/registry/registration.rs` | `register_invocation`:14、`on_subagent_start`:51、`try_join`；按同 parent 的未绑定 invocation FIFO join，冻结 parent 后取出重放事件 |
| Stop/ToolEnded 回收 | `peri-controller/src/langfuse/tracer/registry/closure.rs` | `on_invocation_tool_end`:12、`on_subagent_stop`:53、`close_subagent`；双信号齐备才回收，保留各路径的 flush/remove 顺序 |
| turn 结束兜底 | `peri-controller/src/langfuse/tracer/registry/closure.rs` | `cleanup_turn_end`:155、`finish_observation`；统一投影 close 并设置 private observation_closed，已有 Incomplete 诊断也必须收尾，已 closed 不再关闭 |
| tracer 创建/重放/关闭 | `peri-controller/src/langfuse/tracer/subagent_events.rs` | `handle_join_outcome`、`emit_subagent_obs_start`、`emit_subagent_close`；先开 parent 再重放，关闭时先补 stage 再 flush batch |

## 其他观测子系统

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| Tracer 状态与构造 | `peri-controller/src/langfuse/tracer/mod.rs` | `LangfuseTracer`:52、`new`:93、`new_with_turn_id`:124 |
| LLM Generation | `peri-controller/src/langfuse/tracer/llm_events.rs`、`peri-controller/src/langfuse/tracer/generation.rs` | `on_llm_start`、`on_llm_end`、`on_llm_retrying`、`GenerationTracker` |
| 工具事件与批次 | `peri-controller/src/langfuse/tracer/tool_events.rs`、`peri-controller/src/langfuse/tracer/tool_batch.rs` | `on_tool_start`、`on_tool_end`、`emit_tools_flush`、`ToolBatch` |
| Stage/Workflow/Compact/Middleware | `peri-controller/src/langfuse/tracer/span_events.rs` | `on_stage_start`、`on_stage_end`、`on_compact_end`、`on_workflow_end`、`on_middleware_end` |
| Stage 身份与父链 | `peri-controller/src/langfuse/tracer/stages.rs` | `StageHandle`、`StageSpans`、`MAIN_AGENT_KEY` |
| 中间件 Span 与 Compact Span | `peri-controller/src/langfuse/tracer/middleware.rs`、`peri-controller/src/langfuse/tracer/compact.rs` | `MiddlewareTracer`、`CompactSpan`、`CompactEndInfo` |
| 采样与基础设施 | `peri-controller/src/langfuse/tracer/sampling.rs`、`peri-controller/src/langfuse/tracer/event_builder.rs`、`peri-controller/src/langfuse/tracer/usage.rs` | `SamplingDecider`、`try_add_or_warn_via_session`、`build_usage_details` |
| 背压丢弃遥测 | `peri-controller/src/langfuse/drop_telemetry.rs` | `LangfuseDropRegistry::record`、`snapshot` |
| session 抽象和配置 | `peri-controller/src/langfuse/session.rs`、`peri-controller/src/langfuse/session_like.rs`、`peri-controller/src/langfuse/config.rs` | `LangfuseSession`、`LangfuseSessionLike`、`LangfuseConfig` |

## 回归与跨模块契约

- ARC-CANCEL-001：Controller → Runtime → SessionHandle 原样定位转发；见 [`architecture-contracts.md`](../standards/architecture-contracts.md) 和 [`peri-runtime` 索引](peri-runtime.md)。
- ARC-EVENT-001：`publish_event` / `publish` 是协议化前双投递出口；Langfuse 旁路不参与业务执行，不建立第二条事件投递链。
- 控制面测试：`peri-controller/src/controller_test.rs` 覆盖取消、事件双投递、会话销毁与端口注入。
- 异常关闭回归：`peri-controller/src/langfuse/tracer/registry_lifecycle_test.rs` 的重复 Start/Stop、已 Closed 后重复 Stop；`registry_test.rs` 还验证异常 turn-end 实际发送一次 observation update。
- 观测顺序回归：`peri-controller/src/langfuse/bridge_test.rs` 的双 producer/乱序矩阵，以及 `tracer/tracer_test.rs` 的缺少 LlmCallEnd、未采样错误 parent-first 和错误脱敏。
- bridge 原有内联测试入口迁到 `peri-controller/src/langfuse/bridge/lifecycle_test.rs`，保留 `bridge::tests` 模块路径。
- 验证：`cargo test -p peri-controller`、`cargo test -p peri-controller --doc`、`cargo clippy -p peri-controller --all-targets -- -D warnings`；集成测试使用 `FakeLangfuseSession`，不向真实服务发请求。
