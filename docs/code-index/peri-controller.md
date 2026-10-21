# peri-controller 代码索引

Langfuse tracer 的墙钟时间由 [peri-time](peri-time.md) 读取；毫秒精度等遥测
字段格式由 `src/langfuse/tracer/event_builder.rs` 保留。

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-11。
> 依据：`docs/standards/architecture-contracts.md`、manifest、源码与契约测试；无 crate 级 CLAUDE.md。

## 架构速览

Langfuse 配置类型与规则已迁至 [`peri-config::observability`](peri-config.md)：
`resolve(settings, environment)` 是纯解析，默认值/clamp/非法值/batch 语义只保留
一份。Controller `langfuse/config.rs` re-export，session/tracer 消费 typed 配置；
ACP host 从同一配置 snapshot 取 observability，不由 bridge 重新读来源。

Controller 是控制面宿主：定位 Runtime / Resources、转发会话操作、发布协议化前事件。
取消策略与业务终态归 Agent，登记和销毁编排归 Runtime；Controller 不持有第二份 session 表。
Runtime、Resources 和装配端口只在消费 `self` 的 builder 中替换，发布后按只读字段使用。

事件出口只有订阅广播：`publish` / `publish_event` 补打身份后分发给全部订阅者；慢消费者经
`SubscriptionError::Lagged` 显式报告错过条数，无订阅者时不产生丢失告警。4.2.3 的
「控制面弹出队列事件未投递」告警源于一个生产代码从未排空的有界 mpsc 队列，已删除；
回归入口 `tests/controller_event_delivery.rs::publishing_beyond_capacity_without_pull_consumer_logs_no_drop`。

Langfuse 是同一事件分支上的旁路消费者。bridge 负责 v1/v2 转换、句柄配对与诊断计数，
`LangfuseTracer` 持有单轮观测状态，`SubagentRegistry` 是子 agent 归属与生命周期的唯一 owner。
子模块通过同一 owner 的借用实现注册、缓存和收尾，不另建注册表。

tracer 不再持有输入原文：`tracer/input_snapshot.rs` 把已存在的 `Arc` 借用成
`SnapshotSource`，`llm_events.rs::freeze_input` 经 `LangfuseSessionLike::try_freeze`
取得 `InputSource`（小输入内存字节、大输入暂存句柄），冻结计费与落盘由 langfuse-client
worker 完成，桥接入口不做 IO；冻结被拒时在 metadata 显式标记而不是回退旧输入。
借用适配器的 `write_json` 逐元素 `to_value` 后经 `langfuse_client::snapshot::write_stable_json`
写出，冻结字节的键序（对象键字典序）与构建 feature 无关。

依赖方向为 Controller → Runtime / Resources / peri-acp-types；manifest 中的 peri-agent、
peri-model 和 langfuse-client 是现行 Langfuse 适配依赖，不能由索引推断已经解耦。

## 速查表

Langfuse HTTP 重试与批次失败日志由 `langfuse-client/src/client.rs` 和 `langfuse-client/src/batcher/worker.rs` 记录经过客户端归一化的错误原因（HTTP 状态、重试次数等），不输出认证头、事件内容或服务端响应体；日志回归位于独立集成测试 `langfuse-client/tests/logging.rs`，避免并行单测初始化 tracing callsite 时干扰日志捕获。

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 转发取消三元组 | `peri-controller/src/controller.rs` | `Controller::cancel`:296 | 原样交给 Runtime；未知 session 包装为 CancelFailed，策略和幂等判定归句柄实现（ARC-CANCEL-001） |
| 发布业务事件 | `peri-controller/src/controller.rs` | `publish_event`:397、`publish`:383、`publish_message` | Runtime 补打身份后广播到订阅者；无法补打时保留发射方身份（包括 message_id），不 panic |
| 订阅事件 | `peri-controller/src/controller.rs` | `subscribe`:439、`Subscription::recv`:130、`try_recv`:141 | 慢消费者 Lagged 可恢复（错过条数显式）；无订阅者不告警；退订只 drop receiver，无额外簿记 |
| 注册与定位会话 | `peri-controller/src/controller.rs` | `register_session`:284、`run_session`:268、`session_ids`:307、`contains_session`:312 | register_or_replace 归 Runtime；Controller 只转发，不解释执行结果 |
| 等待、销毁或注入会话 | `peri-controller/src/controller.rs` | `join_session`:321、`destroy_session`:341、`submit_input`:362 | 捕获 Runtime Arc 后调用；销毁返回的已补打事件经 publish 按顺序投递到订阅分支 |
| 注入部署端口 | `peri-controller/src/controller.rs` | `Controller::new`、`with_runtime`、`with_mcp_pool`、`with_cron_scheduler`、`with_tool_search` | builder 消费 self 后赋值；对应 pick 方法克隆句柄/配置，不引入共享可写配置 |
| 调整启动参数 | `peri-controller/src/controller.rs` | `AgentRef`:48、`LiteParams`:69 | 仅承载定义引用、cwd、初始消息和工具；消费与执行归 Agent |
| 配置与创建 Langfuse 批处理 | `peri-config/src/observability.rs`；`peri-controller/src/langfuse/{config,session}.rs`；`langfuse-client/src/{config,batcher}.rs` | core `resolve` / `LangfuseConfig`；`LangfuseSession::new`；`Batcher::try_new` | settings/env/default/clamp 规则与 secret-redacted Debug 归 core（新键解析在 `observability_spool.rs`）；Controller 消费 projection，batcher 构造校验沿 Option 降级；客户端独占重试语义 |
| 借用输入快照来源 | `peri-controller/src/langfuse/tracer/input_snapshot.rs` | `llm_input_source`:41、`GenerationInput`:462、`GenerationInput::into_source`:471、`mark_rejected_input`:485 | 只克隆已存在 `Arc<Vec<BaseMessage>>`/`Arc<Vec<ToolDefinition>>`，不构造 JSON 树、不触达文件路径；`retained_bytes` 零大分配遍历并给保守上界（业务侧显式 reserve 可能超出，见模块限度）；`write_json` 逐元素 `to_value` 后经 `langfuse_client::snapshot::write_stable_json` 输出，键序与构建 feature 无关 |
| 输入冻结与交付路径 | `peri-controller/src/langfuse/tracer/llm_events.rs` | `freeze_input`:23、`GenerationInput::into_source`；`tracer/event_builder.rs`：`try_add_prepared_or_warn_via_session` | 输入来源在请求期冻结一次并随事件交付（`try_add_prepared`）；被拒事实写 metadata、不伪造旧 fallback；准备失败不混淆为发送失败 |
| turn-facing 会话能力 | `peri-controller/src/langfuse/session_like.rs` | `try_freeze`、`try_add_prepared`、`memory_budget`、`flush` | 只提供 flush（`Pin<Box<dyn Future … + Send>>`）与预算透传，无 shutdown 权限；预算身份不符由 `TelemetryEvent::try_new` 拒绝 |
| 装配 spool 能力 | `peri-controller/src/langfuse/session.rs` | `LangfuseSession::{new,new_owned}` 第 3 参 `Option<Arc<dyn SpoolStore>>`；`map_shutdown` | 只接受装配层注入的资源端口，controller 不打开文件系统；关闭结果显式映射 Pending/PersistedPending/DeliveryIncomplete |
| 关闭部署 Langfuse | `peri-controller/src/langfuse/session.rs` | `LangfuseSession::new_owned`；`LangfuseShutdownOwner::shutdown`；`LangfuseSession::shutdown` | fresh deployment 得到不可克隆的关闭权限；只转发唯一 Batcher join，报告包含已由 turn 观察的累计 HTTP 失败并区分 worker 失败；turn-facing SessionLike 仍只提供 flush（ARC-HOST-SHUTDOWN-001） |
| 修改 Langfuse 事件入口 | `peri-controller/src/langfuse/bridge.rs` | `LangfuseBridge`:34、`process_event`:92 | 保留统一事件分发与 tracer 锁，trait 入口先持有该 bridge 的 stage 表锁 |
| 修改 v1 事件转换 | `peri-controller/src/langfuse/bridge/v1_conversion.rs` | `UnifiedLangfuseEvent::from_executor_event` | 无映射事件返回 None；v1 LLM 使用 MAIN_AGENT_KEY，工具优先保留 source_agent_id |
| 修改 v2 事件转换 | `peri-controller/src/langfuse/bridge/v2_conversion.rs` | `from_render_event`、`from_observe_event` | 保留 agent identity、request_id、usage 和 compact 语义数据，不修改事件来源 |
| 维护 bridge stage/middleware 配对 | `peri-controller/src/langfuse/bridge/lifecycle.rs` | `start_stage`、`finish_stage`、`finish_middleware` | stage 按 agent 匹配，找不到时领取 tracer 重放句柄；保留 tracer 锁释放/重取边界 |
| 查看 bridge 收到的子 agent 事件 | `peri-controller/src/langfuse/bridge/lifecycle.rs` | `SubagentTelemetry::on_start`、`on_stop` | 单锁集合与计数只描述此 bridge 收到的事件，不决定观测 parent 或关闭 |
| 调整 turn 开始与终止 | `peri-controller/src/langfuse/tracer/turn.rs` | `on_turn_start`:15、`on_turn_end`:84 | stage → generation → 主工具批次 → 子 agent → error → agent-run 同步入队，最后返回 spawn 的 flush 句柄；`on_turn_start` 首步登记 sid→本 turn trace（不受采样影响），`on_turn_end` 首步 compare-and-remove 清理，供指标出口归属 |
| 改指标事件归属（metrics → Langfuse） | `peri-controller/src/langfuse/{metric_sink,turn_traces}.rs` | `LangfuseMetricsSink::record`；`TurnTraceRegistry::{register,clear,resolve}` | 指标按自带 `sid` 取该会话活跃 turn 的 trace 归属（不再各开 root trace）；无活跃登记（未开始/已结束/无 sid）时回退独立 root trace，事件不丢；`clear` 只移除仍指向本 tracer trace 的登记；出口由 ACP host 在 Langfuse 可用时安装（无 Langfuse 不落盘、不发电） |
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
| session 抽象和配置 | `peri-controller/src/langfuse/{session,session_like,config}.rs`；配置事实源 `peri-config/src/observability.rs` | `LangfuseSession`、`LangfuseSessionLike`、core `LangfuseConfig` re-export |

## 回归与跨模块契约

- ARC-CANCEL-001：Controller → Runtime → SessionHandle 原样定位转发；见 [`architecture-contracts.md`](../standards/architecture-contracts.md) 和 [`peri-runtime` 索引](peri-runtime.md)。
- ARC-EVENT-001：`publish_event` / `publish` 是协议化前广播出口；Langfuse 旁路不参与业务执行，不建立第二条事件投递链。
- 控制面测试：`peri-controller/src/controller_test.rs` 覆盖取消、事件广播、会话销毁与端口注入。
- 异常关闭回归：`peri-controller/src/langfuse/tracer/registry_lifecycle_test.rs` 的重复 Start/Stop、已 Closed 后重复 Stop；子 agent 终态与 Workflow 在内存闭合后发送一次完整 Create，不重发同 ID 更新。
- 观测顺序回归：`peri-controller/src/langfuse/bridge_test.rs` 的双 producer/乱序矩阵，以及 `tracer/tracer_test.rs` 的缺少 LlmCallEnd、未采样错误 parent-first 和错误内容保真。
- bridge 原有内联测试入口迁到 `peri-controller/src/langfuse/bridge/lifecycle_test.rs`，保留 `bridge::tests` 模块路径。
- 验证：`cargo test -p peri-controller`、`cargo test -p peri-controller --doc`、`cargo clippy -p peri-controller --all-targets -- -D warnings`；集成测试使用 `FakeLangfuseSession`，不向真实服务发请求。
