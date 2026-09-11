# peri-acp 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-11（模块职责拆分与 compact/历史恢复修复合并）
> 依据：peri-acp/CLAUDE.md、docs/standards/architecture-contracts.md、docs/design/peri-acp-protocol.md、源码

## 架构速览

- 数据流：`ACP request → transport(mpsc/stdio) → host 部署单元 → dispatch 纯函数 → SessionManager(frozen/caps) → run_prompt → peri-agent run_session_loop → ExecutorEvent → event/forwarder+mapper → SessionUpdate / AcpEvent → client`
- 服务入口：`src/host/mod.rs` 的 `run_acp_server(AcpTransport, AcpServerConfig)` 与 `host/lifecycle.rs::spawn_acp_server`（TUI/print 保留返回的 non-Clone `AcpHostHandle`，`session/prompt` spawn 后台 task 保证 cancel 可响应）；stdio 部署单元 `src/host/stdio/mod.rs:38` 的 `run_acp_stdio(StdioInput)`（Provider、合并配置与 `ConfigSource` 统一按 canonicalized `input.cwd` 冻结，经共享 session-map 变体 `run_acp_server_with_sessions` 接入统一 host 核心）。方法分发：`src/host/requests.rs:22` 的 `handle_request` match（按方法分派到 `host/requests/` 子模块：session_lifecycle / plugin / config_options / mcp_oauth / workflow / rewind）——**stdio 与 TUI 共用统一 host 核心 + `handle_request`（单一路径，transport 多态）**
- 稳定不变量：`SessionManager` 在每条 session/new、load、resume、fork 路径注册 caps，发送扩展事件前按 session caps 门控；frozen 数据经版本化 ThreadStore snapshot 跨进程复用、会话内不可漂移（ARC-FROZEN-001）；事件改动须覆盖发射/mapper/forwarder/caps 门控/客户端五层（ARC-EVENT-001）；Hub/Web 投影必须从 canonical event 映射为版本化 allowlist DTO（`event/activity.rs`），禁止复用 TUI 私有 `event_json`；中间件链序事实源在 Agent 层 `production_blueprint`（ARC-MIDDLEWARE-001），ACP 仅构造装配上下文；Langfuse bridge/tracer 实现在 `peri-controller/src/langfuse/`，ACP `event/forwarder.rs` 只保留协议化前分支的接线点（None=禁用），不参与业务链路

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改待发送队列控制与执行准入 | `src/host/requests/user_input.rs` + `src/host/user_input.rs` + `src/host/prompt_dispatch.rs` | `handle_user_input`；`ensure_mailbox` / `schedule_mailbox`；`dispatch_prompt_turn_with_input` | 四短 RPC 不等 prompt_lock，session 持 Agent Mailbox；Agent ticket 经同一执行锁启动，RunStarted/done 身份配对，Stop 精确定位，MPSC/stdio 共用请求与事件链（ARC-BOUNDARY-001 / ARC-EVENT-001） |
| 改插件 marketplace 搜索 | `src/host/requests/plugin.rs` + `plugin_search_test.rs` | `handle_search` / `search_marketplace_plugins` | 经 PluginManagerPort 获取缓存目录，复用 `plugin::marketplace::find_marketplace_json` 读取根或 `.claude-plugin` 布局；名称、描述、marketplace 名均忽略大小写匹配；无匹配明确返回空数组；回归经真实 `handle_request` 读取临时磁盘目录 |
| 改 compact 后失败恢复 | `src/host/prompt.rs` + `src/host/compact_recovery_test.rs` | `finish_prompt_turn` | 不按 `ok` 丢弃可信 canonical snapshot；取消/模型或 forwarder 失败仍保留已提交 Full 摘要；persistence_inconsistent 移除热会话，冷加载恢复磁盘，ARC-COMPACT-001 |
| 改 System Reminder producer/ACP 投影 | `src/session/dynamic_mcp.rs` + `src/host/continuation.rs` + `src/session/event_sink.rs` + `src/dispatch/session_replay.rs` | `SessionDynamicMcpNotificationSink`；`enqueue_cron_trigger`；`push_system_reminder`；`send_system_reminder` | Dynamic MCP lifecycle/OAuth 与 Cron trigger 直接入 canonical queue；不改变 OAuth/cron 控制；ACP client 声明 `peri.systemReminder` 时收结构化 event，否则只收展示 fallback；load/replay 不伪装 user message；旧 Compact plain-text Human 经 `compact_reminder::legacy_compact_reminders` 生成 Legacy 通知，MPSC/stdio 共用出口 |
| 新增/改会话协议方法 | `src/host/requests.rs`（注册面，`handle_request` :22，按方法分派到 `host/requests/{session_lifecycle,plugin,config_options,mcp_oauth,workflow,rewind}.rs`）；`src/host/server_loop.rs`（`session/prompt` 单独处理，spawn 后台 task）；`src/session/frozen_snapshot.rs`（版本化 frozen owner state）；`src/dispatch/session_fork.rs`（fork payload 独立复制）；stdio 侧部署装配点 `src/host/stdio/mod.rs`（`run_acp_stdio` 持有进程日志初始化，`assemble_stdio_config` 只装配配置，业务处理走统一 `run_acp_server`） | `handle_new/load/resume/fork`（requests/session_lifecycle.rs）；`fork_session`；`encode_frozen_snapshot` / `decode_frozen_snapshot`；`after_new_response`；其余 plugin/config/workflow/rewind handler | new 持久化 frozen 后才发布 session；load/resume 冷恢复原快照，legacy 缺失按 ThreadMeta.cwd 原子 write-once 回填、loser 重读 winner，未知/损坏版本及存储错误 fail closed；fork 继承 source frozen，并以新 `MessageId` 复制 payload/compact flags，使新 thread 独立拥有可压缩历史；new/fork 写失败补偿删除。`session/load` 保持 response 前 replay/通知；load/resume 补载驻留空历史时同步 canonical payload 与消息投影，后续 prompt/fork 从同一 payload 读取；`session/prompt` 是唯一 spawn 后台执行的方法；stdio 与 TUI 共用统一 host |
| 改 prompt 执行流程（keepgoing/挂起注入/错误响应） | `src/host/prompt.rs` + `src/host/prompt_dispatch.rs` + `src/session/executor.rs` | `run_prompt`；`prompt_wire_response` / `execution_failure_to_acp_error`；`dispatch_prompt_turn`；`session/executor.rs` **仅 re-export** `peri_agent::session::exec::executor` 的执行入口（ARC-BOUNDARY-001） | 挂起时 prompt 注入 inbox；keepgoing 短路在 Agent 层；重试中的 `LlmRetrying` 是进度事件，不结束 prompt；仅 fatal `PromptResult.failure` 在历史/state/cancel-token 后处理完成后映射为 `session/prompt` JSON-RPC server error（`-32000`）：message 保留脱敏限长后的 LLM/provider 原意，allowlist data 携带 `kind` 与可选 HTTP `status`；cancel/interrupted/max iterations 仍返回成功 `PromptResponse`；mpsc/stdio 共用统一 host |
| 改事件映射（ExecutorEvent → 协议） | `src/event/mapper.rs` + `src/event/mod.rs` + `src/event/activity.rs` + `src/session/event_sink.rs` + `src/session/event_sink/{legacy,stdio}.rs` + `src/dispatch/session_replay.rs` | `map_event`；`map_agent_activity`；`tool_result_content`；`TransportEventSink::push_event`；`push_legacy_event`；`StdioEventSink::push_event`；`AcpEvent` DTO | Transport sink 依次发送标准 update → safe activity → legacy，两个扩展面按各自 caps 门控；ToolEnd live/replay 使用标准 `failed`/`completed`，同时写标准 `ToolCallUpdate.content` 与兼容 `rawOutput`，失败空文本有安全 fallback；SubAgent 来源写入 ACP 标准 `SessionNotification._meta.peri.sourceAgentId`（mpsc/stdio 同构，typed SDK 往返保留）；`CompactStarted/CompactCompleted` 经 `peri/agent_event` 透传 strategy、trigger 与安全计数供 TUI 展示；`BgRegistryEvent` 是私有功能载体：无标准 `SessionUpdate`，TUI 私有事件仍按 `agent_event` cap 门控，Hub/Web 仅经 `map_agent_activity` 输出去正文、哈希 correlation 的 capability-gated allowlist 摘要；契约 ARC-EVENT-001 |
| 改 Goal 状态、持久化与客户端投影 | `src/session/goal_state/mod.rs` + `src/session/event_sink/legacy.rs` + `src/event/{mod,mapper}.rs`；契约 DTO 在 `peri-acp-types/src/{goal,event,event_v2}.rs` | `GoalState::snapshot`；`GoalController::increment_continuation`；`StateEvent::GoalSnapshot` → `ExecutorEvent::GoalSnapshot` → `AcpEvent::GoalSnapshot` | continuation 计数归 session Goal 状态持有并随 Goal 持久化；Agent 每轮发只读快照，event sink 按 `agent_event` capability 投递给客户端，TUI 不直读 Agent/Middleware；契约 ARC-BOUNDARY-001 / ARC-EVENT-001 |
| 改事件发射/forwarder | `src/event/forwarder.rs` | `spawn_eventbus_forwarder(handles, on_event, bridge) -> JoinHandle<()>` | 消费 v2 EventBus 三通道（render/state/observe），**biased select：render 先于 state**（防 partial 污染）；主 executor/workflow 必须在 producer drop 后 await handle，禁止 terminal 越过 final usage；JoinError fail closed；Langfuse 在协议化前分支消费；observe Lagged 容错；映射后经 `on_event(UnstampedEvent, ExecutorEvent)` 送 event_sink |
| 改 Hub/Web 事件投影 | `src/event/activity.rs` | `map_agent_activity(&ExecutorEvent) -> Option<AgentActivityWire>`（:93）；`AgentActivityKind`（:19）/`AgentActivityStatus`（:36） | `peri.agentActivity` 安全摘要面：allowlist 字段 + `safe_label`/`truncate_utf8`/`hash_correlation` 清洗；禁止携带消息/路径/输出/错误正文；cap 未双向协商不投影 |
| 改 provider/模型/配置 | `src/provider/mod.rs` + `config.rs` + `store.rs` | `LlmProvider` enum（mod.rs:23，OpenAi/Anthropic）；`from_config`（:118）/`from_config_for_alias`（:125）/`into_model`（:246）；`PeriConfig`（config.rs:13）；`ConfigSource`（store.rs:78，读写路径唯一事实源，`load_at` :92 / `save` :199） | 模型切换走 `session/set_config_option` 的 `configId="model"` 分支（requests/config_options.rs:62，`handle_set_config_option` :44）；`session/update_config` 校验 providers 非空 + profile→provider 引用；`AgentPool::has_valid_cache`（session/agent_pool.rs:64）按 provider 指纹复用 LLM 实例 |
| 改 transport（新增传输） | `src/transport/mod.rs` + `mpsc.rs` + `stdio.rs` + `router.rs` | `AcpTransport` trait；`mpsc_transport_pair()`；`RequestRouter::{register,dispatch,close,wait_closed}`；`PendingRequest`；`StdioTransport::from_reader_writer` | router 以 owned pending handle 统一线性化 response、caller cancellation 与 terminal close，数字 ID 在正数域回绕并以 owner identity 防 stale handle 误删；终止以稳定 `Transport closed` 结算当前/后续请求，连接静默仍无隐式 timeout。MPSC 任一 pump/channel 关闭终止逻辑 pair，并保留已转发 incoming queue；stdio reader EOF/error 与所有 writer 路径汇入同一 terminal 状态。String response id 仍走 unmatched 转发；legacy `{"type":"cancel"}` 仍只在 stdio pump 精确拦截。契约：ARC-TRANSPORT-001；测试：`router_test.rs`、`mpsc_test.rs`、`stdio_test.rs`。 |
| 改 host 退出 / Langfuse 部署关闭 | `src/host/lifecycle.rs` + `src/host/shutdown.rs` + `src/host/task_scope.rs` + `src/session/mod.rs` | `spawn_acp_server`（lifecycle.rs:37）；`AcpHostHandle::shutdown`（:67）；`HostExitContext::finish`；`SessionManager::take_for_close`（session/mod.rs:249）/`AcpSession::close_resources`（:173） | 真实 host 任务保留至 join，取消等待不取走句柄；Incomplete 把任务和实际待关闭 session 留在退出 context 重试，完整 drain 后才使用 fresh assembly 的 non-Clone Langfuse 关闭权限；共享外部注入不授权（ARC-HOST-SHUTDOWN-001） |
| 改 prompt 组装（system prompt） | `src/prompt/mod.rs` + `prompts/sections/*.md` | `PromptTemplate::render`；`PromptFeatures::detect`；`PromptEnv::with_frozen_date` | render 按 zone/order 拼接 section，并用 `peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY` 把 cached/uncached seam 交给 provider；剥离 token 后须保持旧 prompt bytes，empty 不生成 token（ARC-SERIAL-001）；frozen date 在会话创建时注入，禁止中途重读（ARC-FROZEN-001） |
| 改 HITL/AskUser 交互 | `src/broker/transport_broker.rs`（TUI/stdio 统一 broker，批 3 后无第二实现） | `AcpTransportBroker`、`impl UserInteractionBroker`（`request` 是完整转发 context 的串行化点）；`with_auto_approve` / `with_timeout`；`parse_ask_user_timeout` / `ask_user_timeout` | 同一 broker 实例的转发 Approval/Questions 共享 capacity=1 异步门，多 item Approval 不可被 Questions 插入；AutoApprove 锁前本地返回。审批逐 item 发 `session/request_permission` RPC（仅 allow_once/reject_once 两选项），问题聚合为单个 `elicitation/create` form；传输失败默认 Reject（防误放行）；提问超时兜底：统一构造点读 env `PERI_ASK_USER_TIMEOUT_SECS`（缺失/非法 → 默认 300s，`0` → 不超时） |
| 改命令路由/内置命令 | `src/session/command/mod.rs` + `src/dispatch/commands.rs` + `src/host/prompt.rs` + `src/host/notify.rs` | `register_builtins`（command/mod.rs:124，compact/clear/rewind/LoopPlaceholder）；`register_ui_entries`（commands.rs:73）/`ui_route_entries`（:38）；`stdio_filters_command`；`send_available_commands_update` | 注册顺序 = 内置 → 本地 skills → 插件（`AcpServerConfig::plugin_command_entries`）→ 动态注入；stdio 部署设置 `stdio_command_filter=true`：`clear`/`rewind`（含 alias）既不出现在 available commands，也不被 slash command 拦截，而是 fall-through 作为普通 prompt 进入 agent；TUI/print 保持命令行为，`session/rewind*` RPC 不受影响；`session/command/compact/pipeline.rs` **仅 re-export** `peri_agent::session::exec::compact_pipeline::execute_compact` |
| 改 cancel / continuation 链路 | `src/session/mod.rs` + `src/host/continuation.rs` | `SessionManager::cancel_session` / `cancel_all_agents` / `cancel_cascade_children_for`；`cancel_arms_continuation`（continuation.rs:66）；`run_continuation_scheduler`（:111） | 按 (session_id, turn_id, attempt_id) 三元组定位，clear_queue 默认 false；cancel 置位 `continuation_armed`（epoch 代际校验防过期执行，`continuation_still_valid` :89）；cancel > 续跑 > promote > retry 优先级由 Agent 判定；契约 ARC-CANCEL-001 |
| 改 caps 门控 | `src/session/caps.rs` | `set_pending_caps`（initialize 暂存）/`consume_pending_caps`（session/new 消费）/`ensure_session_caps`/`effective_host_caps` | 发送扩展事件前按该 session 的 caps 门控；cap 未双向协商不得投影；事件改动必须覆盖 caps 门控层 |
| 改装配/中间件链/部署 | `src/host/assemble.rs` + `src/host/stage_builder.rs` | `assemble_server_config(HostAssemblyInput)`；`assemble_hook_groups`；`build_stage_context`；`build_session_manager` | ACP stage bridge 只转发 `FrozenSessionData`，language/MetaHarness/date/prompt projection 均从该 snapshot 派生；链序事实源仍是 Agent 层 `production_blueprint`（ARC-MIDDLEWARE-001） |
| 改 rewind | `src/dispatch/rewind.rs` + `src/session/command/rewind.rs` + `src/host/prompt.rs` | `rewind_preview`（:52）；`rewind_execute`（:215）；`rewind_candidates`（rewind_candidates.rs）；`stdio_filters_command` | `session/rewind*` RPC 仅在双向协商 `peri.rewind` 后可用：preview 返回有界 project-relative 文件影响 + 一次性指纹，execute 前重算历史，指纹缺失/过期拒绝；统一宿主注册使 stdio/TUI 都可调用 RPC（cap 未协商时 -32601）。另有部署差异：stdio 的 slash `/rewind`（及 alias）从命令投影隐藏并 fall-through 进 agent，TUI/print 仍执行内置命令 |

## 子系统

### src/session/（会话生命周期 + 注册表）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 会话注册表/API | session/mod.rs | `AcpSession` / `SessionManager` 持有唯一 live session 注册表；host EOF 经 `take_for_close` 把实际记录移交退出 context，`close_resources` 返回可重试结果；公开 `close_session` 保留原 request 关闭 API |
| 会话装配 / frozen 数据构建 | session/construction.rs + session/frozen.rs | `build_session` / `build_command_registry`；`build_frozen_data` 从会话创建时的配置构造冻结 prompt；持久化快照仍在 frozen_snapshot.rs |
| Session 访问与 bridge 装配 | session/access.rs + session/bridges.rs + session/dynamic_mcp.rs | `SessionAccessPort`、`v2_queue_for` / `session_inbox_for`；`bind_cron_continuation`；`SessionDynamicMcpNotificationSink` 保留 public re-export，使用 weak inbox 投递 |
| 执行编排（re-export 桥） | session/executor.rs | 仅 re-export `peri_agent::session::exec::executor`（run_session_loop、is_keepgoing、FrozenSessionData、ContinuationRequest 等，:17-22） |
| 事件 sink / capability 编排 | session/event_sink.rs | `TransportEventSink`（:51）；`push_event`（:124）保留标准 update → safe activity → legacy 顺序与 caps 查询；其他通知面和 transport owner 留在根模块 |
| legacy TUI 事件面 | session/event_sink/legacy.rs | `TransportEventSink::push_legacy_event`（:21，私有）：`ExecutorEvent` → `AcpEvent` → `peri/agent_event`；调用方持有 agent_event 门控；结果/实例 ID 保持原 wire 载荷 |
| typed stdio sink | session/event_sink/stdio.rs | `StdioEventSink`（:38，根模块 public re-export）；`push_event`（:64）仅发标准更新；`session_notification` 统一 `_meta.peri.sourceAgentId`；不持有 transport 注册表 |
| sink 回归 | session/event_sink_test.rs | 原 `session::event_sink::tests` 路径保留；覆盖 compact/rewind/retry、caps、来源元数据与 typed SDK roundtrip，以及双 cap 时 safe activity 先于 legacy 且不携带结果正文/原始实例 ID |
| 内置命令注册 | session/command/ | `register_builtins`（mod.rs:124）；compact（:26）/clear/rewind；compact pipeline 仅 re-export（compact/pipeline.rs:11） |
| LLM 实例池 | session/agent_pool.rs | `AgentPool`（:30，`has_valid_cache` :64 / `invalidate` :81） |
| 目标状态 | session/goal_state/mod.rs | `GoalState`（:59，`set_goal` :80 / `snapshot` :167） |
| cron 桥 | session/cron_bridge.rs | `SessionCronBridge`（:14，`start` :29，session 级跨 turn 存活） |
| 状态构建 | session/state_builders.rs | `parse_permission_mode`（:19）/`apply_profile_effort`（:29）/`build_config_options`（:67） |

### src/event/（事件映射与转发）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 事件 DTO 与共享兼容转换 | event/mod.rs + peri-acp-types/src/event_v2/executor_mapping.rs | `AcpEvent` 使用 tag+content serde；`*_event_to_executor` 经 types crate 根路径 re-export，ACP 不复制另一套转换；TurnCompleted 来自 Render 层 |
| v1→协议映射 | event/mapper.rs | `map_event`（:51）；`MappedEvent`（:22，standard/standard_with_src） |
| LLM usage 可选字段与来源 | event/mapper.rs + event/mapper_test.rs | `map_event` 的 `LlmCallEnd` 分支；有 usage 才产生 `UsageUpdate`，tokenStats cap 开启才附加计数 `_meta`；cacheReadTokens 缺省省略、显式零保留，sourceAgentId 与计数独立透传；TUI 消费入口见 peri-tui 索引与 ARC-EVENT-001 |
| 事件泵 | event/forwarder.rs | `spawn_eventbus_forwarder`（:78，biased select render 优先） |
| 安全活动投影 | event/activity.rs | `map_agent_activity`（:93，allowlist DTO） |
| OAuth 事件 | event/oauth.rs | `HostOAuthEvent`（:102，host 级通道，不依赖 session event_sink） |

### src/prompt/ 与 prompts/sections/（prompt 组装）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 环境检测 | prompt/mod.rs | `PromptFeatures::detect`（:42）；`PromptEnv::with_frozen_date`（:104） |
| 模板渲染 | prompt/mod.rs | `PromptTemplate::render`（按 features 门控 section；四态生成 cache boundary transport token） |
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
| 宿主所有权与服务入口 | host/mod.rs | `run_acp_server` / `run_acp_server_inner` 持有 deployment owner；`SessionState` 保持 frozen/agent_pool/continuation/lease 状态 |
| 消息循环与请求分类 | host/server_loop.rs | `ServerLoop::run`；`spawn_prompt` / `spawn_mcp_apps_request` 经 task owner 准入；`dispatch_request` 保持 response → after_new_response |
| Prompt 编排 / 预测 | host/prompt_dispatch.rs + host/prediction.rs | `dispatch_prompt_turn` 保留 host 根 re-export；`spawn_prediction` 在原 prompt lock 范围内准入 |
| OAuth 事件投递 | host/oauth_delivery.rs | `spawn_oauth_consumer` / `deliver_oauth_event`；safe 与 legacy caps 分别裁决 |
| EOF 收尾 | host/shutdown.rs | `shutdown_host` 借用唯一强 owner；先撤销准入，再取消并 drain 会话，最后关闭 LSP/MCP |
| 方法注册面（mpsc） | host/requests.rs + host/requests/*.rs | `handle_request`（requests.rs:22，30 个方法分派到子模块；各 handle_* 均为 `pub(super)` 定义在对应子文件） |
| notification 处理 | host/notify.rs | `handle_notification`（:28）/`extract_session_id`（:153）；`host/unify_wire_baseline_test.rs` 锁定发射面 payload 与 schema typed `SessionNotification` 的逐字段一致性；统一 host 入口见 ARC-STDIO-001 与 `docs/design/architecture.md` |
| prompt 执行编排 | host/prompt.rs | `run_prompt` 借用既有 AcpServerConfig 与当轮参数；`take_recall_for_turn`；保留 session 快照、Controller 执行及 canonical 结果回写顺序 |
| prompt 模型工厂 | host/prompt/models.rs | `build_model_factories`；闭包复用当轮 provider/config 快照与同一 session AgentPool，缓存按 provider fingerprint 校验 |
| prompt 观测装配 | host/prompt/telemetry.rs | `build_langfuse_hooks` / `build_forwarder_launcher`；turn hooks 与 bridge 共享 tracer，事件消费顺序仍归 event/forwarder.rs |
| prompt stage 装配 | host/prompt/stage.rs | `build_stage_bridge` / `build_compact_hooks`；逐次 stage 构造原 compact hooks，保留 host/prompt.rs 的 hook re-export |
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

- ARC-COMPACT-001：失败结果仍采纳可信历史；writer 失败要求冷恢复
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

- 部署关闭回归：`host/stdio/langfuse_shutdown_test.rs` 的真实尾部事件、waiter 取消、共享会话、MCP/session Incomplete 重试与 HTTP 失败终态；`transport/mpsc_test.rs::test_explicit_close_rejects_both_pending_directions_and_delivers_eof` 证明显式 close 结算双向 pending 并让两端 EOF。
