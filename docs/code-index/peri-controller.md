# peri-controller 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-08-17（langfuse tracer / bridge 拆分事件域子模块）
> 依据：docs/standards/architecture-contracts.md（ARC-CANCEL-001 / ARC-EVENT-001）、源码（无 crate 级 CLAUDE.md，架构速览取自 lib.rs / controller.rs 模块注释）

## 架构速览

- 职责：控制面宿主（docs/design/architecture.md §6）——控制面五步 lite params → pick Resources → pick Runtime → run Session → pop events；无业务执行权，只定位与转发（cancel 语义、终态判定均归 Agent 层）
- 数据流：ACP → `Controller`（协议化前分支）→ Runtime 查映射 → `SessionHandle`；事件经 `publish_event`/`publish` 双投递（弹出队列 `pop_events` + 订阅广播 `subscribe`，Langfuse bridge 旁路消费同一分支，不参与业务链路）
- 稳定不变量：cancel 按 (session_id, turn_id, attempt_id) 三元组定位并转发，不解释取消语义（ARC-CANCEL-001）；事件统一出口为 `publish_event`（stamp 补打 + 双投递，ARC-EVENT-001）；缺省 Runtime 空实例、Resources 未注入，由部署装配点经 `with_*` 注入
- 依赖方向（§0）：Controller → Runtime / Controller → Resources / 契约层 peri-acp-types；不依赖 peri-acp；peri-agent / peri-model / langfuse-client 为 langfuse 过渡依赖（L4 移除）

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| cancel 转发（三元组定位） | `src/controller.rs` | `Controller::cancel(&CancelRequest)`（:336）→ `Runtime::cancel` → `SessionHandle::cancel` | 定位依据请求携带的三元组（`CancelRequest.identity`，事实源 `peri-acp-types/src/identity.rs:262`）；未注册 session 包 context 为 `ControllerError::CancelFailed`；幂等判定与终态归 Agent 层，本层只定位与转发（契约 ARC-CANCEL-001） |
| 事件发布（业务事件出口） | `src/controller.rs` | `publish_event(session_id, &UnstampedEvent, ExecutorEvent)`（:436）；`publish(EventEnvelope)`（:424）；私有 `publish_message`（:453） | 先经 `Runtime::stamp` 补打 session_id/session_seq（未注册 session 降级为发射方身份直接投递，不 panic），再双投递：弹出队列有界满丢弃（Critical 类）+ 订阅广播慢消费者 lagging（Broadcast 类） |
| 事件订阅（协议化前分支） | `src/controller.rs` | `subscribe()`（:463）→ `Subscription::recv`（:131）/ `try_recv`（:142）/ `unsubscribe`（:154） | 显式订阅句柄；`Lagged(skipped)` 可恢复（可继续 recv）、`Closed` 终态；退订 = drop 接收端或显式 unsubscribe，无 Controller 侧簿记 |
| pop events（控制面第五步） | `src/controller.rs` | `pop_events()`（:470） | 按投递序 `try_recv` 排干弹出队列全部在途事件（`try_recv` 直到 Empty） |
| session 定位/枚举 | `src/controller.rs` | `register_session`（:324）、`run_session`（:308）、`session_ids`（:348）、`contains_session`（:353） | 注册 = 注册或替换句柄（不递增 epoch/不重置 seq）；run 只发起不解释，错误包 `RunFailed`；枚举无顺序保证（Runtime 簿记为 HashMap） |
| 会话生命周期面 | `src/controller.rs` | `join_session`（:362）、`destroy_session`（:383）、`submit_input`（:404） | join 带 deadline（true=期内结束/false=超时）；destroy 经 Runtime 七步编排，drain 出的补打事件经 `publish` 双投递并作为返回值；submit_input 透传 `SessionHandle::submit_input`，错误包 `InjectFailed` |
| 装配注入（pick 目标源） | `src/controller.rs` | `with_runtime`（:216）/ `with_resources`（:223）/ `with_mcp_pool`（:230）/ `with_cron_scheduler`（:240）/ `with_tool_search`（:250）/ `with_lsp_servers`（:259）与对应 `pick_*`（:265-:280） | 缺省：Runtime 空实例、Resources None、端口 None、lsp 空 vec；宿主装配点构造具体实现后 upcast 注入 |
| lite params 会话启动参数 | `src/controller.rs` | `LiteParams::new`（:87）、`with_initial_messages`（:104）、`with_tools`（:110）；`AgentRef`（:49） | 仅承载最小启动参数集（session 标识/agent 引用/cwd/初始输入/初始消息/工具集）；初始消息与工具集为透传声明，消费方在 Agent 层 session 工厂（L5） |
| Langfuse 观测（旁路消费者） | `src/langfuse/bridge.rs` + `src/langfuse/tracer/mod.rs` | `LangfuseBridge::process_event`（bridge.rs:105）；`LangfuseTracer`（tracer/mod.rs:49，`new` :90） | 事件在协议化前分支给 bridge，不承担 Controller 职责；`UnifiedLangfuseEvent`（bridge/unified_event.rs:19，bridge.rs:21 re-export）为 v1/v2 事件并集，无映射事件返回 None |
| 改边界错误类型 | `src/error.rs` | `ControllerError`（:9）、`SubscriptionError`（:33） | 仅对边界可判定条件类型化（RunFailed/CancelFailed/JoinFailed/DestroyFailed/InjectFailed 均包 Runtime context 为 `#[source]`）；层内细节错误归 anyhow，不逐层类型化 |

## 子系统

### 控制面宿主（src/）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| Controller 宿主（控制面五步） | controller.rs | `Controller::new`（:198）；`EVENT_CHANNEL_CAPACITY = 1024`（:42，弹出队列与订阅广播共用） |
| 事件订阅句柄 | controller.rs | `Subscription`（:125，Broadcast 交付类） |
| 边界错误 | error.rs | `ControllerError`（:9：RunFailed/CancelFailed/JoinFailed/DestroyFailed/InjectFailed，均包 Runtime context）；`SubscriptionError`（:33：Lagged/Closed） |
| crate 出口 | lib.rs | re-export `AgentRef/Controller/LiteParams/Subscription`、`ControllerError/SubscriptionError`（:23-24） |
| 测试入口（ARC-CANCEL-001 验证） | controller_test.rs | `cancel_forwards_triple_to_handle`（:215）、`cancel_unknown_session_typed_error`（:237）、`publish_pop_and_subscribe_events`（:258）、`bypass_consumer_subscribes_same_branch`（:298）、`destroy_session_orchestrates_phases_and_publishes_drained`（:390）；MockHandle（:27）实现 `SessionHandle` 记录调用序列，`temp_store`（:121）注入 ThreadStore |

### Langfuse 观测（src/langfuse/）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 事件路由 / 统一枚举 | bridge.rs + bridge/unified_event.rs | `UnifiedLangfuseEvent`（unified_event.rs:19）；`from_executor_event`（unified_event.rs:167）/ `from_render_event`（:350）/ `from_observe_event`（:395）；`LangfuseBridge`（bridge.rs:31）、`process_event`（bridge.rs:105） |
| 单轮追踪器 Facade | tracer/mod.rs + tracer/{llm_events,tool_events,span_events,subagent_events}.rs | `LangfuseTracer`（mod.rs:49）；turn：`on_turn_start`（mod.rs:142）/ `on_turn_end`（mod.rs:210）；LLM：`on_llm_start`（llm_events.rs:15）/ `on_llm_retrying`（llm_events.rs:239）；工具：`on_tool_start`（tool_events.rs:17）/ `on_tool_end`（tool_events.rs:82）；Span：`on_compact_start`（span_events.rs:14）/ `on_compact_end`（span_events.rs:29）/ `on_stage_start`（span_events.rs:117）/ `on_stage_end`（span_events.rs:138）；subagent：`on_subagent_start`（subagent_events.rs:102）/ `on_subagent_stop`（subagent_events.rs:122） |
| ReAct 阶段 Span 管理 | tracer/stages.rs | `StageHandle`（:23）、`StageSpans`（:47）、`MAIN_AGENT_KEY`（:20） |
| 工具批次管理 | tracer/tool_batch.rs | `ToolBatch`（:52） |
| SubAgent 归属注册表 | tracer/registry.rs | `SubagentRegistry`（:210，agent_id 查表归属，替代旧 LIFO 栈） |
| LLM Generation 生命周期 | tracer/generation.rs | `GenerationTracker`（:43） |
| 中间件链追踪 | tracer/middleware.rs | `MiddlewareTracer`（:33） |
| Compact Span | tracer/compact.rs | `CompactSpan`（:38）、`CompactEndInfo`（:15） |
| 采样决策器 | tracer/sampling.rs | `SamplingDecider`（:15） |
| 基础设施 | tracer/event_builder.rs / tracer/usage.rs | `now_rfc3339`（event_builder.rs:19）、`new_uuid`（:24）、`try_add_or_warn_via_session`（:58）；TokenUsage 转换 |
| 背压丢弃遥测 | drop_telemetry.rs | `LangfuseDropRegistry`（:65）、`record`（:84）、`snapshot`（:115） |
| 会话抽象 | session.rs / session_like.rs / fake_session.rs | `LangfuseSession`（session.rs:19）；trait `LangfuseSessionLike`（session_like.rs:8）；`FakeLangfuseSession`（fake_session.rs:10，测试注入） |
| 配置 | config.rs | `LangfuseConfig`（:3）、`from_env`（:46）、`load_with_settings`（:90） |

## 跨模块契约（指向 architecture-contracts.md，不复制正文）

- ARC-CANCEL-001：cancel 链路 Controller →(定位转发) Runtime →(查映射) Agent 句柄；`CancelRequest` 事实源 `peri-acp-types::identity`（:262，含 clear_queue/policy，clear_queue 默认 false）；幂等判定与 turn 终态归 Agent 层，上层只定位与转发
- ARC-EVENT-001：事件链路单事实源 Agent 发射 → ACP 映射 → TUI 消费；Controller `publish_event`/`publish` 是协议化前分支（stamp 补打 + 弹出队列/订阅广播双投递），Langfuse bridge 以同一分支旁路消费，禁止恢复第二套事件投递
