# peri-acp 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-08-25（frozen snapshot stage bridge）
> 依据：peri-acp/CLAUDE.md、docs/standards/architecture-contracts.md、docs/design/peri-acp-protocol.md、源码

## 架构速览

- 数据流：`ACP request → transport(mpsc/stdio) → host 部署单元 → dispatch 纯函数 → SessionManager(frozen/caps) → run_prompt → peri-agent run_session_loop → ExecutorEvent → event/forwarder+mapper → SessionUpdate / AcpEvent → client`
- 服务入口：`src/host/mod.rs:248` 的 `run_acp_server(AcpTransport, AcpServerConfig)`（mpsc/TUI 部署单元，`session/prompt` spawn 后台 task 保证 cancel 可响应）；stdio 部署单元 `src/host/stdio/mod.rs:39` 的 `run_acp_stdio(StdioInput)`（Provider、合并配置与 `ConfigSource` 统一按 canonicalized `input.cwd` 冻结，随后整体接入统一 `run_acp_server`）。方法分发：`src/host/requests.rs:22` 的 `handle_request` match（按方法分派到 `host/requests/` 子模块：session_lifecycle / plugin / config_options / mcp_oauth / workflow / rewind）——**stdio 与 TUI 共用 `run_acp_server` + `handle_request`（单一路径，transport 多态）**
- 稳定不变量：`SessionManager` 在每条 session/new、load、resume、fork 路径注册 caps，发送扩展事件前按 session caps 门控；frozen 数据会话内不可漂移（ARC-FROZEN-001）；事件改动须覆盖发射/mapper/forwarder/caps 门控/客户端五层（ARC-EVENT-001）；Hub/Web 投影必须从 canonical event 映射为版本化 allowlist DTO（`event/activity.rs`），禁止复用 TUI 私有 `event_json`；中间件链序事实源在 Agent 层 `production_blueprint`（ARC-MIDDLEWARE-001），ACP 仅构造装配上下文；Langfuse bridge/tracer 实现在 `peri-controller/src/langfuse/`，ACP `event/forwarder.rs` 只保留协议化前分支的接线点（None=禁用），不参与业务链路

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 新增/改会话协议方法 | `src/host/requests.rs`（注册面，`handle_request` :22，按方法分派到 `host/requests/{session_lifecycle,plugin,config_options,mcp_oauth,workflow,rewind}.rs`）；`src/host/mod.rs`（`session/prompt` 单独处理 :455，spawn 后台 task）；stdio 侧部署装配点 `src/host/stdio/mod.rs`（:39 `run_acp_stdio` + `assemble_stdio_config`，业务处理走统一 `run_acp_server`） | handle_* 入口：`session/new`（requests/session_lifecycle.rs:84）、`after_new_response`（:165）、`session/rename`（requests/session_lifecycle.rs:552）、`session/update_config`（requests/config_options.rs:130）、`workflow/*`（requests/workflow.rs:12-93）、`plugin/*`（requests/plugin.rs:83-400）、`session/rewind*`（requests/rewind.rs:13-52）；`session/cancel` 走 notification | `session/new` 成功 response 写入 transport 后才首发 `available_commands_update` 并启动 MCP 预热，确保客户端先建立 sessionId 路由；`session/load` 保持 response 前 replay/通知；`session/prompt` 是唯一 spawn 后台执行的方法；stdio 与 TUI 共用统一 host |
| 改 prompt 执行流程（keepgoing/挂起注入/错误响应） | `src/host/prompt.rs` + `src/host/mod.rs` + `src/session/executor.rs` | `run_prompt`；`prompt_wire_response` / `execution_failure_to_acp_error`；`dispatch_prompt_turn`；`session/executor.rs` **仅 re-export** `peri_agent::session::exec::executor` 的执行入口（ARC-BOUNDARY-001） | 挂起时 prompt 注入 inbox；keepgoing 短路在 Agent 层；重试中的 `LlmRetrying` 是进度事件，不结束 prompt；仅 fatal `PromptResult.failure` 在历史/state/cancel-token 后处理完成后映射为 `session/prompt` JSON-RPC server error（`-32000`，脱敏非空 message，首版无 data），cancel/interrupted/max iterations 仍返回成功 `PromptResponse`；mpsc/stdio 共用统一 host |
| 改事件映射（ExecutorEvent → 协议） | `src/event/mapper.rs` + `src/event/mod.rs` + `src/event/activity.rs` + `src/session/event_sink.rs` + `src/dispatch/session_replay.rs` | `map_event`；`map_agent_activity`；`tool_result_content`；`TransportEventSink::push_event`；`AcpEvent` DTO | ToolEnd live/replay 使用标准 `failed`/`completed`，同时写标准 `ToolCallUpdate.content` 与兼容 `rawOutput`，失败空文本有安全 fallback；SubAgent 来源写入 ACP 标准 `SessionNotification._meta.peri.sourceAgentId`（mpsc/stdio 同构，typed SDK 往返保留）；`BgRegistryEvent` 是私有功能载体：无标准 `SessionUpdate`，TUI 私有事件仍按 `agent_event` cap 门控，Hub/Web 仅经 `map_agent_activity` 输出去正文、哈希 correlation 的 capability-gated allowlist 摘要；`LlmRetrying` 经 `peri/agent_event` 送达 TUI；契约 ARC-EVENT-001 |
| 改事件发射/forwarder | `src/event/forwarder.rs` | `spawn_eventbus_forwarder(handles, on_event, bridge)`（:78） | 消费 v2 EventBus 三通道（render/state/observe），**biased select：render 先于 state**（防 partial 污染）；Langfuse `LangfuseBridge` 在协议化前分支消费（:101）；observe Lagged 容错；映射后经 `on_event(UnstampedEvent, ExecutorEvent)` 送 event_sink |
| 改 Hub/Web 事件投影 | `src/event/activity.rs` | `map_agent_activity(&ExecutorEvent) -> Option<AgentActivityWire>`（:93）；`AgentActivityKind`（:19）/`AgentActivityStatus`（:36） | `peri.agentActivity` 安全摘要面：allowlist 字段 + `safe_label`/`truncate_utf8`/`hash_correlation` 清洗；禁止携带消息/路径/输出/错误正文；cap 未双向协商不投影 |
| 改 provider/模型/配置 | `src/provider/mod.rs` + `config.rs` + `store.rs` | `LlmProvider` enum（mod.rs:23，OpenAi/Anthropic）；`from_config`（:118）/`from_config_for_alias`（:125）/`into_model`（:246）；`PeriConfig`（config.rs:13）；`ConfigSource`（store.rs:78，读写路径唯一事实源，`load_at` :92 / `save` :199） | 模型切换走 `session/set_config_option` 的 `configId="model"` 分支（requests/config_options.rs:62，`handle_set_config_option` :44）；`session/update_config` 校验 providers 非空 + profile→provider 引用；`AgentPool::has_valid_cache`（session/agent_pool.rs:64）按 provider 指纹复用 LLM 实例 |
| 改 transport（新增传输） | `src/transport/mod.rs` + `mpsc.rs` + `stdio.rs` + `router.rs` | `AcpTransport` trait；`mpsc_transport_pair()`；`RequestRouter::{register,dispatch,close,wait_closed}`；`PendingRequest`；`StdioTransport::from_reader_writer` | router 以 owned pending handle 统一线性化 response、caller cancellation 与 terminal close，数字 ID 在正数域回绕并以 owner identity 防 stale handle 误删；终止以稳定 `Transport closed` 结算当前/后续请求，连接静默仍无隐式 timeout。MPSC 任一 pump/channel 关闭终止逻辑 pair，并保留已转发 incoming queue；stdio reader EOF/error 与所有 writer 路径汇入同一 terminal 状态。String response id 仍走 unmatched 转发；legacy `{"type":"cancel"}` 仍只在 stdio pump 精确拦截。契约：ARC-TRANSPORT-001；测试：`router_test.rs`、`mpsc_test.rs`、`stdio_test.rs`。 |
| 改 host 后台任务 / EOF 收口 | `src/host/task_scope.rs` + `src/host/mod.rs` + `src/host/stdio/mod.rs` + `src/session/mod.rs`；跨层 owner 契约 `peri-acp-types/src/ports.rs` | `HostTaskOwner::{begin_shutdown,shutdown}`；`HostTaskSpawner::spawn`；`McpTaskOwnerPort`；`SessionManager::{pre_close_session,session_ids}` | non-Clone owner 与 weak spawner 分离；ACP config 只持 boxed `McpTaskOwnerPort`，不得直接 import middlewares concrete owner。EOF 以 local + manager session ID 并集执行 pre-close/close，cooperative grace 后 abort/drain，LSP/MCP 全在 session 锁外关闭；host 或 MCP service report 超时均返回 `Incomplete` 并保持 Closing。契约 ARC-HOST-SHUTDOWN-001 |
| 改 prompt 组装（system prompt） | `src/prompt/mod.rs` + `prompts/sections/*.md` | `PromptTemplate::render`（:342）；`PromptFeatures::detect`（:42）；`PromptEnv::with_frozen_date`（:104） | render 按 `PromptFeatures` 门控 section（git repo 检测等）；frozen date 在会话创建时注入，禁止中途重读（ARC-FROZEN-001）；`format_available_agents`（:409） |
| 改 HITL/AskUser 交互 | `src/broker/transport_broker.rs`（TUI/stdio 统一 broker，批 3 后无第二实现） | `AcpTransportBroker`、`impl UserInteractionBroker`（`request` 是完整转发 context 的串行化点）；`with_auto_approve` / `with_timeout`；`parse_ask_user_timeout` / `ask_user_timeout` | 同一 broker 实例的转发 Approval/Questions 共享 capacity=1 异步门，多 item Approval 不可被 Questions 插入；AutoApprove 锁前本地返回。审批逐 item 发 `session/request_permission` RPC（仅 allow_once/reject_once 两选项），问题聚合为单个 `elicitation/create` form；传输失败默认 Reject（防误放行）；提问超时兜底：统一构造点读 env `PERI_ASK_USER_TIMEOUT_SECS`（缺失/非法 → 默认 300s，`0` → 不超时） |
| 改命令路由/内置命令 | `src/session/command/mod.rs` + `src/dispatch/commands.rs` + `src/host/prompt.rs` + `src/host/notify.rs` | `register_builtins`（command/mod.rs:124，compact/clear/rewind/LoopPlaceholder）；`register_ui_entries`（commands.rs:73）/`ui_route_entries`（:38）；`stdio_filters_command`；`send_available_commands_update` | 注册顺序 = 内置 → 本地 skills → 插件（`AcpServerConfig::plugin_command_entries`）→ 动态注入；stdio 部署设置 `stdio_command_filter=true`：`clear`/`rewind`（含 alias）既不出现在 available commands，也不被 slash command 拦截，而是 fall-through 作为普通 prompt 进入 agent；TUI/print 保持命令行为，`session/rewind*` RPC 不受影响；`session/command/compact/pipeline.rs` **仅 re-export** `peri_agent::session::exec::compact_pipeline::execute_compact` |
| 改 cancel / continuation 链路 | `src/session/mod.rs` + `src/host/continuation.rs` | `SessionManager::cancel_session`（:400，过渡路径）/`cancel_all_agents`（:771）/`cancel_cascade_children_for`（:753）；`cancel_arms_continuation`（continuation.rs:66）；`run_continuation_scheduler`（:111） | 按 (session_id, turn_id, attempt_id) 三元组定位，clear_queue 默认 false；cancel 置位 `continuation_armed`（epoch 代际校验防过期执行，`continuation_still_valid` :89）；cancel > 续跑 > promote > retry 优先级由 Agent 判定；契约 ARC-CANCEL-001 |
| 改 caps 门控 | `src/session/mod.rs` | `set_pending_caps`（:436，initialize 暂存）/`consume_pending_caps`（:469，session/new 消费）/`ensure_session_caps`（:506）/`effective_host_caps`（:455） | 发送扩展事件前按该 session 的 caps 门控；cap 未双向协商不得投影；事件改动必须覆盖 caps 门控层 |
| 改装配/中间件链/部署 | `src/host/assemble.rs` + `src/host/stage_builder.rs` | `assemble_server_config(HostAssemblyInput)`；`assemble_hook_groups`；`build_stage_context`；`build_session_manager` | ACP stage bridge 只转发 `FrozenSessionData`，language/MetaHarness/date/prompt projection 均从该 snapshot 派生；链序事实源仍是 Agent 层 `production_blueprint`（ARC-MIDDLEWARE-001） |
| 改 rewind | `src/dispatch/rewind.rs` + `src/session/command/rewind.rs` + `src/host/prompt.rs` | `rewind_preview`（:52）；`rewind_execute`（:215）；`rewind_candidates`（rewind_candidates.rs）；`stdio_filters_command` | `session/rewind*` RPC 仅在双向协商 `peri.rewind` 后可用：preview 返回有界 project-relative 文件影响 + 一次性指纹，execute 前重算历史，指纹缺失/过期拒绝；统一宿主注册使 stdio/TUI 都可调用 RPC（cap 未协商时 -32601）。另有部署差异：stdio 的 slash `/rewind`（及 alias）从命令投影隐藏并 fall-through 进 agent，TUI/print 仍执行内置命令 |

## 子系统

### src/session/（会话生命周期 + 注册表）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 会话注册表/API | session/mod.rs | `AcpSession`（:70，provider_id/model_alias/active_agents/goal_state）；`SessionManager`（:157，`new_session` :273 / `close_session` :362 / `build_frozen_data` :534 / caps 门控族 :436-506 / `v2_queue_for` :651 / `session_inbox_for` :663） |
| 执行编排（re-export 桥） | session/executor.rs | 仅 re-export `peri_agent::session::exec::executor`（run_session_loop、is_keepgoing、FrozenSessionData、ContinuationRequest 等，:17-22） |
| 事件 sink | session/event_sink.rs | `TransportEventSink`（:57，`push_unstable_event` :80）；`StdioEventSink`（:425，agent_client_protocol 直发） |
| 内置命令注册 | session/command/ | `register_builtins`（mod.rs:124）；compact（:26）/clear/rewind；compact pipeline 仅 re-export（compact/pipeline.rs:11） |
| LLM 实例池 | session/agent_pool.rs | `AgentPool`（:30，`has_valid_cache` :64 / `invalidate` :81） |
| 目标状态 | session/goal_state/mod.rs | `GoalState`（:59，`set_goal` :80 / `snapshot` :167） |
| cron 桥 | session/cron_bridge.rs | `SessionCronBridge`（:14，`start` :29，session 级跨 turn 存活） |
| 状态构建 | session/state_builders.rs | `parse_permission_mode`（:19）/`apply_profile_effort`（:29）/`build_config_options`（:67） |

### src/event/（事件映射与转发）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 事件 DTO | event/mod.rs | `AcpEvent`（:42，22 个变体，tag+content serde）；re-export `peri_acp_types::event_v2::*_event_to_executor`（:30） |
| v1→协议映射 | event/mapper.rs | `map_event`（:51）；`MappedEvent`（:22，standard/standard_with_src） |
| 事件泵 | event/forwarder.rs | `spawn_eventbus_forwarder`（:78，biased select render 优先） |
| 安全活动投影 | event/activity.rs | `map_agent_activity`（:93，allowlist DTO） |
| OAuth 事件 | event/oauth.rs | `HostOAuthEvent`（:102，host 级通道，不依赖 session event_sink） |

### src/prompt/ 与 prompts/sections/（prompt 组装）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 环境检测 | prompt/mod.rs | `PromptFeatures::detect`（:42）；`PromptEnv::with_frozen_date`（:104） |
| 模板渲染 | prompt/mod.rs | `PromptTemplate::render`（:342，按 features 门控 section） |
| section 模板 | prompts/sections/01..15_*.md | 纯 markdown 事实源，改文案改这里 |

### src/provider/（LLM 配置）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| Provider 构建 | provider/mod.rs | `LlmProvider`（:23）；`from_config_for_alias`（:125）；`into_model`（:246） |
| 配置结构 | provider/config.rs | `PeriConfig`（:13）/`AppConfig`（:177，`merge_overrides` :232）/`ProviderConfig`（:456） |
| 配置加载/保存 | provider/store.rs | `ConfigSource`（:78，`load_at` :92 / `save` :199 / `load_lenient` :121） |

### src/transport/（传输抽象）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 传输 trait | transport/mod.rs | `AcpTransport`（:24） |
| mpsc 实现 | transport/mpsc.rs | `spawn_pump` 对任一方向关闭执行 pair 级 terminal；`send_or_close` 统一 outbound failure；`mpsc_transport_pair` 共享 router/ID 空间 |
| stdio 实现 | transport/stdio.rs | pump 显式处理 EOF/read error 并观察 router close；`write_envelope` 让 writer mutex/write/flush 全程竞速 terminal；legacy cancel 与入站 id 域校验保持不变 |
| 请求-响应匹配 | transport/router.rs | `RequestRouter` 原子持有 pending/terminal 状态；`PendingRequest` Drop 同步按 owner identity 注销；`CancellationToken` 提供 lost-wake-safe close 观察 |

### src/dispatch/（共享业务纯函数）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 方法分发聚合 | dispatch/mod.rs | re-export：`build_initialize_response`、`handle_prompt`、`rewind_execute`、`fork_session`、`replay_session_history` 等 |
| 命令执行 | dispatch/execute_command.rs | `execute_command`（:75） |
| rewind | dispatch/rewind.rs | `rewind_preview`（:52）/`rewind_execute`（:215） |
| UI 命令条目 | dispatch/commands.rs | `register_ui_entries`（:73） |

### src/broker/（HITL/AskUser 桥）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 交互 broker | broker/transport_broker.rs | `AcpTransportBroker::request`：同实例内完整 transport-forwarded Approval/Questions 共用异步 gate；AutoApprove 绕过；Approval→RequestPermission、Questions→elicitation/create |

### src/host/（部署单元 = 装配面）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 服务循环 | host/mod.rs | `run_acp_server`（:248）；`dispatch_prompt_turn`（:496）；`SessionState`（:66，frozen/agent_pool/workflow_middleware/continuation_armed/lease） |
| 方法注册面（mpsc） | host/requests.rs + host/requests/*.rs | `handle_request`（requests.rs:22，30 个方法分派到子模块；各 handle_* 均为 `pub(super)` 定义在对应子文件） |
| notification 处理 | host/notify.rs | `handle_notification`（:28）/`extract_session_id`（:153）；通知 wire 基线 `host/unify_wire_baseline_test.rs`（批 0 建立、统一后回归基线：发射面 payload 与 schema typed `SessionNotification` 序列化逐字段一致，见 docs/design/acp-host-unify.md） |
| prompt 执行体 | host/prompt.rs | `run_prompt`（:35）；`take_recall_for_turn`（:763）；`build_compact_hooks`（:776） |
| 续跑调度 | host/continuation.rs | `run_continuation_scheduler`（:111） |
| Host 任务所有权 | host/task_scope.rs | `HostTaskOwner` / `HostTaskSpawner`；生产 timeout driver + 测试 controlled phase driver |
| writer lease | host/lease.rs | `WriterLease`（:20，多读者单 writer） |
| 装配 | host/assemble.rs | `assemble_server_config`（:136） |
| stage 构建 | host/stage_builder.rs | `build_stage_context`：消费单一 `FrozenSessionData`，派生 frozen language/MetaHarness/date 与 Agent 装配输入，禁止从当轮 config 建第二事实源 |
| workflow 薄壳 | host/workflow_agent.rs | `create_session_workflow_middleware`（:192，装配经 `WorkflowMiddlewareFactory` 端口） |
| stdio 部署 | host/stdio/ | `run_acp_stdio`（mod.rs:39，`StdioInput` → `assemble_stdio_config` → `run_acp_server_with_sessions`，业务处理走统一宿主）；集成测试 `run_server_integration_test.rs`（initialize → session/new → 通知 wire 链路） |

### src/agent/（装配面薄壳）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 装配说明 | agent/mod.rs | 模块文档：`build_agent`/`build_stage_context` 装配桥在 `host/stage_builder`，workflow agent 执行器已归位 `peri_agent::agent::workflow`；本目录无实现文件 |

## 跨模块契约（指向 architecture-contracts.md，不复制正文）

- ARC-BOUNDARY-001：TUI 交互主路径经 ACP transport，不得直驱 Agent 运行时；ACP 仅协议化薄壳 + 装配面宿主
- ARC-TRANSPORT-001：stdio/MPSC terminal 结算当前与后续请求；response、caller cancellation、close 对 pending 至多生效一次；连接静默无隐式 timeout
- ARC-HOST-SHUTDOWN-001：host/MCP 任务的 non-Clone deployment owner、weak spawner、EOF 会话并集收口与锁外 pool close 契约
- ARC-CANCEL-001：cancel 三元组定位（`CancelRequest` 事实源 `peri-acp-types::identity`），幂等与终态归 Agent 层；`SessionManager::cancel_session` 为过渡路径
- ARC-EVENT-001：事件链路单事实源 Agent 发射（v2 EventBus）→ ACP 映射/转发（`peri-acp/src/event/`）→ 客户端；禁止 v1 中间态与第二套投递
- ARC-FROZEN-001：frozen 数据会话内不可漂移（`build_frozen_data` 会话创建时构建）
- ARC-KEEPGOING-001：空白 prompt（`MessageContent::is_empty()`）＝ keepgoing；ACP executor 短路 + `push_done` 退出 loading
- ARC-TOOLS-001：`BaseTool::is_direct()` 自声明可见性（工具注册在 Agent/middlewares 层，ACP 只持 `shared_tools` 视图）
- ARC-SERIAL-001：prompt cache 相关序列化顺序确定，禁止 HashMap 迭代序（`shared_tools` 用 BTreeMap）
- ARC-MIDDLEWARE-001：中间件链序事实源 `production_blueprint`（peri-agent session 工厂），ACP 不重排
- ARC-SECRET-001：日志/错误/遥测不得泄露 secret（provider api_key 仅在 LlmProvider 内部持有）
