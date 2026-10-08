# peri-acp 代码索引

输入提交性能入口：`host/requests/session_io.rs` 在短会话锁内选择已绑定环境，队列 IO 在锁外执行；`host/server_loop.rs::spawn_session_io` 将输入快照 放入 host-owned 请求任务，普通变更与生命周期操作保留原有接收顺序。`host/session_io_test.rs` 覆盖挂起输入操作时快照继续推进、全局会话锁可用与关闭拒绝。

`host/diagnostics.rs::ResponseDiagnostics` 的 `perf.input` 记录 RPC、session、command/input 身份与响应发送完成耗时，不记录正文；该耗时不是键盘到终端绘制的完整延迟。`host/prompt.rs` 在短锁内取得 canonical payload 快照，消息过滤复制在锁外执行。

执行失败协议出口：`host/prompt.rs::execution_failure_to_acp_error` 保留 `kind/status`、`error_category/causes` 与完整有界 Model `diagnostic`；`host/diagnostics.rs::ResponseDiagnostics` 记录 method/rpc/session 和错误内容。日志、ACP message/data 与遥测不做内容脱敏；分类、权限和终止语义保持独立，规则见 ARC-SECRET-001。

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-10-01（ACP 会话级 HTTP/stdio MCP 声明与 Peri instructions 扩展）。
> 依据：peri-acp/CLAUDE.md、docs/standards/architecture-contracts.md、docs/design/peri-acp-protocol.md、源码

## 架构速览

当前进程内部执行的交互身份由 `host/execution.rs` 与 `host/prompt.rs` 管理，`ExecutionStarted` 经 `session/event_sink.rs` 可靠发送，正常终态及装配早退匹配同一 request ID。定时审批持有同一会话 prompt lock，在独立交互作用域内批准、拒绝、取消并收尾；之后实际执行不复用审批身份。完整宿主测试在 `activation_tests::execution_tests`，挂起/恢复 FIFO 验证在 `event::forwarder_test`；批次范围与验证见 `spec/issues/2026-10-08-async-execution-chain-fixes.md`。

当前进程的晚到异步结果由 `host/activation.rs` 监听会话队列的 retained wake 信号；`session/activation.rs` 将监听与自动续跑许可绑定运行时。主 run 正常结束不撤销监听，订阅及 dispatch 结束都复查队列，续跑统一经过 `host/continuation.rs` 与 `prompt_dispatch.rs` 的序列化和代际校验。取消 continuation 抑制再次激活，关闭取消监听；`run_prompt` 捕获尝试开始的队列接纳边界，失败仅禁止边界内输入自动重试，边界后的 Required / EnsureProcessing 消息仍通过统一 `can_activate` 准入。定向宿主回归入口 `host::requests::tests::activation_tests`，边界状态回归 `session::activation::tests`。

续跑 MQ pending 不是排队准入锁：空跑或早退不能阻塞下一次结果；重复通知在 prompt lock 内复核真实待处理消息。回归 `activation_tests::empty_queued_continuation_does_not_block_the_next_child_result` 覆盖旧请求排队后消息被消费、空跑退出、下一子任务结果仍能启动续跑。

普通 prompt/input 经 host → Agent 运行，不要求 reverse admission。Work query/resolve、恢复执行及持久 control 方法撤销；history list/load/resume/replay 保留并装配新 runtime，不加载 owner quarantine。移除与验证见 [active plan](../../spec/issues/2026-10-07-remove-execution-recovery-plan.md)。

Emscripten 的 ACP 部署入口在 `src/host/assemble.rs::assemble_wasm_server_config`：
注入 provider、配置 source、session resources 与 shutdown，复用 `AcpServerConfig`
和 `host/requests.rs` 的单一方法分发；`transport/wire_bridge.rs` 将宿主字节流接入
同一 ACP Host。`host/stdio` 与 `transport/stdio` 仅在原生目标编译，WASM 不装配
builtin MCP、cron 或本地 workspace 资源。远端 MCP 仍经现有 pool 初始化。

配置规则与来源权威见 [`peri-config`](peri-config.md)。ACP `provider/{config,store}.rs`
仅 re-export core 类型与 `settings::ConfigSource`；正常 source 持有 `ConfigurationSystem`，
提供同 scope 的 snapshot、revision 与 CAS 保存。host 装配在 MCP 初始化前注入同一
快照，并取其 provider/Langfuse 投影；Model adapter 构造仍由 ACP 完成。
workspace 资源输入从本次选中 `ConfigSource` 的资源投影取关闭位；lenient 无 snapshot 时只解析该来源已读取的全局正文，缺少可信投影则关闭 bundled skills，不重读默认全局配置；技能 fixture 使用选中的 global 配置路径。新值须显式 reload 并重取 snapshot，
旧 pool 固定旧 Arc，没有 hot watcher。lenient 无 authority 仅临时可读、不可写。

- 数据流：`ACP request → transport(mpsc/stdio) → host 部署单元 → dispatch 纯函数 → SessionManager(frozen/caps) → run_prompt → peri-agent run_session_loop → ExecutorEvent → event/forwarder+mapper → SessionUpdate / AcpEvent → client`
- 服务入口：`src/host/mod.rs` 的 `run_acp_server(AcpTransport, AcpServerConfig)` 与 `host/lifecycle.rs::spawn_acp_server`（TUI/print 保留返回的 non-Clone `AcpHostHandle`，`session/prompt` spawn 后台 task 保证 cancel 可响应）；stdio 部署单元 `src/host/stdio/mod.rs:38` 的 `run_acp_stdio(StdioInput)`（Provider、合并配置与 `ConfigSource` 统一按 canonicalized `input.cwd` 冻结，经共享 session-map 变体 `run_acp_server_with_sessions` 接入统一 host 核心）。方法分发：`src/host/requests.rs:22` 的 `handle_request` match（按方法分派到 `host/requests/` 子模块：session_lifecycle / plugin / config_options / mcp_oauth / workflow / rewind）——**stdio 与 TUI 共用统一 host 核心 + `handle_request`（单一路径，transport 多态）**
- 稳定不变量：`SessionManager` 在每条 session/new、load、resume、fork 路径注册 caps，发送扩展事件前按 session caps 门控；frozen 数据经版本化 ThreadStore snapshot 跨进程复用、会话内不可漂移（ARC-FROZEN-001）；事件改动须覆盖发射/mapper/forwarder/caps 门控/客户端五层（ARC-EVENT-001）；Hub/Web 投影必须从 canonical event 映射为版本化 allowlist DTO（`event/activity.rs`），禁止复用 TUI 私有 `event_json`；中间件链序事实源在 Agent 层 `production_blueprint`（ARC-MIDDLEWARE-001），ACP 仅构造装配上下文；Langfuse bridge/tracer 实现在 `peri-controller/src/langfuse/`，ACP `event/forwarder.rs` 只保留协议化前分支的接线点（None=禁用），不参与业务链路

## 速查表


ACP 出站错误诊断统一位于 `src/host/diagnostics.rs`：`ServerLoop` 的普通请求、prompt、MCP Apps、MCP over ACP、准入拒绝及生命周期锁失败均经 `ResponseDiagnostics::send` 记录方法、RPC ID、可用会话 ID、错误码与错误消息，再原样发送响应；内部错误为 ERROR，其余拒绝为 WARN，正常取消为 DEBUG。`notify.rs` 的会话信息、配置项和命令列表通知共用 `send_session_update` 记录投递失败，不记录请求参数、响应数据或通知内容。针对性回归：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::diagnostics`。

Session ID 恢复与生命周期回归：`src/host/requests_workspace_cases_test.rs` 验证
忽略调用方 cwd、跨实例恢复、执行环境失败与 SessionEnd 排空（独立 history RPC 仍可读）；
`src/host/requests_workspace_assembly_test.rs` 验证装配失败时保留本实例运行句柄，
排空成功后关闭写入准入。统一运行 `cargo test -p peri-acp --lib -- host::requests::tests::workspace_cases`。
重命名持久化及通知回归见 `src/host/requests_lifecycle_cases_test.rs`。
跨实例执行协调由部署方处理，Peri 不提供旧执行接管；ACP 不持有执行 owner、writer lease 或 owner catalog，不校验旧 Agent 停止 proof，也不返回 ownership 只读准入或接管 warning。Store 模式、持久化状态、binding、保存目录与 frozen 校验仍是恢复边界。

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改 ACP 日期、时间戳与有界等待 | `src/session/{frozen,construction,goal_state}.rs` + `src/host/{workspace,requests,supervisor,task_scope}.rs`；底层见 [`peri-time`](peri-time.md) | `peri_time::{calendar_date,now_utc_rfc3339,monotonic_now,interval,timeout}` | session/new 冻结部署日历日期；恢复沿用快照；Workspace 轮询和租约续期保留原预算与分类；契约层 `ThreadMeta::new_at` / `ThreadGoal::new_at` 只接收调用方传入的 `SystemTime`，内部转换为既有 Chrono 字段 |
| 改 Session ID 恢复与会话执行环境 | `src/host/workspace.rs` + `src/host/requests/{session_restore,legacy_session,session_lifecycle}.rs` + `src/host/workspace_resources.rs` + `src/host/assemble.rs` | `prepare_existing`；`check_expected`；`identity_response`；`SessionEnvironment::{assemble,assemble_prepared,task_manager}` | load/resume 按 ID 使用保存 cwd/binding，检查 Store 访问模式、持久化状态与 frozen；不要求请求 cwd 匹配、不取 session 文件锁、不执行 dirty reset。不具备执行环境时明确失败，独立 history RPC 保持可读；不做 owner CAS、旧进程 proof 或 ownership 只读准入。Incomplete 关闭收尾保持独立。 |
| 改异步任务 ACP 投影与操作 | `src/host/requests.rs` + `requests/{session_lifecycle,session_close}.rs` + `src/session/{construction,mod}.rs` | `bind_session_tasks`；`session/bg-tasks`；`session/cancel-bg-task`；`close_session` | session 级订阅 TaskManager 变更并按 revision 发布快照/增量；ACP 取消由 Manager 路由。显式关闭持久登记 intent，排空 prompt、SessionEnd、MCP/task scope 与持久化写入；fresh ACP 通过可信 MCP 连接对账 closing scope，不依据执行 owner descriptor 或旧 Agent proof。保留数据时 `finish_close` 清除 intent，丢响应按 `CloseSettlement` 回读；删除移除会话树。EOF 不取消独立 MCP 任务。跨实例协调归部署方；Peri 不提供旧执行接管；目标语义见 [Session 异步任务架构](../design/session-async-tasks.md)。 |
| 改会话终止 hooks 与定时审批 | `src/host/workspace.rs` + `src/host/assemble.rs` + `src/host/continuation.rs` | `finish_session_end`、`SessionEndState`、`build_session_end_task`、scheduled permission selection | 装配层构造hook执行，环境负责准入和等待；SessionEnd按实际会话单次执行并保留cleanup owner；无效binding仅跳过未开始hook，继续资源关闭；cron校验会话状态与binding并消费会话权限 |
| 改待发送队列控制与执行准入 | `src/host/requests/user_input.rs` + `src/host/user_input.rs` + `src/host/prompt_dispatch.rs` | `handle_user_input`；`ensure_mailbox` / `schedule_mailbox`；`dispatch_prompt_turn_with_input` | 四短 RPC 不等 prompt_lock，session 持 Agent Mailbox；Agent ticket 经同一执行锁启动，RunStarted/done 身份配对，Stop 精确定位，MPSC/stdio 共用请求与事件链（ARC-BOUNDARY-001 / ARC-EVENT-001） |
| 改插件 marketplace 搜索 | `src/host/requests/plugin.rs` + `plugin_search_test.rs` | `handle_search` / `search_marketplace_plugins` | 经 PluginManagerPort 获取缓存目录，复用 `plugin::marketplace::find_marketplace_json` 读取根或 `.claude-plugin` 布局；名称、描述、marketplace 名均忽略大小写匹配；无匹配明确返回空数组；回归经真实 `handle_request` 读取临时磁盘目录 |
| 改插件范围与会话投影 | `src/host/requests/plugin.rs` + `src/host/requests/plugin_scope_test.rs` + `src/host/requests/plugin_identity_test.rs` | `handle_session_snapshot` / `toggle_project_dir` / `mutation_project_dir` | list 的 session 来源不是安装范围，真实记录核实 scope；无记录或范围歧义显式只读，保留 load_error；install/update/uninstall 传递明确 InstallScope（缺省 user），project/local 使用受信 cwd，不优先猜 scoped；真实端口请求回归挂载 `host::requests::tests::plugin_scope_tests` / `host::requests::tests::plugin_identity_tests` |
| 改 marketplace catalog mutation | `src/host/requests.rs` + `src/host/requests/plugin.rs` + `src/host/requests/marketplace_mutation_test.rs` | `handle_marketplace_add` / `handle_marketplace_remove` / `handle_refresh` | `marketplace/add`、`marketplace/remove` 复用 PluginManagerPort 的宿主全局 catalog 端口，sessionId 仅客户端 ticket 上下文，不表示 project scope；读写失败以 -32603 返回并记录日志；真实临时目录验证持久化失败不覆盖旧配置 |
| 改 compact 后失败恢复 | `src/host/prompt.rs` + `src/host/compact_recovery_test.rs` | `finish_prompt_turn` | 不按 `ok` 丢弃可信 canonical snapshot；取消/模型或 forwarder 失败仍保留已提交 Full 摘要；persistence_inconsistent 移除热会话，冷加载恢复磁盘，ARC-COMPACT-001 |
| 验证手动 compact 跨轮与取消 | `src/host/compact_command_test.rs` + `src/session/command/compact_persistence_test.rs` + `src/session/command/compact_report_test.rs` + `tests/compact_command_contract_test.rs` | `run_session_loop` → `finish_prompt_turn`；`TransportEventSink` | 覆盖连续手动、自动 Full 后手动、新连接冷读后手动、摘要生成中取消，以及完整报告输入/排除、仅 reminder 和 child 边界；完整 canonical 历史保持，done 与 PromptResponse 在真实 MPSC 通知上同一终态；原始两项复现保留为 public API 集成回归 |
| 改 System Reminder producer/ACP 投影 | `src/session/dynamic_mcp.rs` + `src/host/continuation.rs` + `src/session/event_sink.rs` + `src/dispatch/session_replay.rs` | `SessionDynamicMcpNotificationSink`；`enqueue_cron_trigger`；`push_system_reminder`；`send_system_reminder` | Dynamic MCP lifecycle/OAuth 与 Cron trigger 直接入 canonical queue；不改变 OAuth/cron 控制；ACP client 声明 `peri.systemReminder` 时收结构化 event，否则只收展示 fallback；load/replay 不伪装 user message；旧 Compact plain-text Human 经 `compact_reminder::legacy_compact_reminders` 生成 Legacy 通知，MPSC/stdio 共用出口 |
| 新增/改会话协议方法 | `src/host/requests.rs` + `src/host/requests/{session_lifecycle,session_restore,legacy_session}.rs` + `src/host/server_loop.rs` + `src/session/frozen_snapshot.rs` + `src/dispatch/session_fork.rs` | `handle_new/fork`；`handle_load/resume`（session_restore.rs）；`new_session_from_prepared`；`encode_frozen_snapshot` / `decode_frozen_snapshot`；`after_new_response` | new 持久化 frozen 后发布，可执行 load/resume 读取原快照并仅在此路径用保存 cwd 准备/接纳 legacy；独立 history RPC 不要求装配可执行环境。可执行路径坏/未知 frozen 与存储错误仍 fail closed。load response 前 replay/通知保持，驻留空历史补 canonical payload；fork 独立复制消息/flags、继承 frozen。已移除 `peri/session_reset_dirty` 路由，`peri.sessionRecoveryV1` 与执行恢复协商已移除，history load/resume 保留，不用 dirty 确认换取恢复资格；stdio 与 TUI 共用 host |
| 改 prompt 执行流程（keepgoing/挂起注入/错误响应） | `src/host/prompt.rs` + `src/host/prompt_dispatch.rs` + `src/session/executor.rs` | `run_prompt`；`prompt_wire_response` / `execution_failure_to_acp_error`；`dispatch_prompt_turn`；`session/executor.rs` **仅 re-export** `peri_agent::session::exec::executor` 的执行入口（ARC-BOUNDARY-001） | 挂起时 prompt 注入 inbox；keepgoing 短路在 Agent 层；重试中的 `LlmRetrying` 是进度事件，不结束 prompt；仅 fatal `PromptResult.failure` 在历史/state/cancel-token 后处理完成后映射为 `session/prompt` JSON-RPC server error（`-32000`）：message 保留脱敏限长后的 LLM/provider 原意，allowlist data 携带 `kind` 与可选 HTTP `status`、受控 diagnostic facts；ACP 不序列化完整 AgentError/ModelError/provider body；cancel/interrupted/max iterations/输出截断预算耗尽仍返回携带对应停止原因的标准 `PromptResponse`，协议成功不代表任务完成（ARC-OUTPUT-COMPLETION-001）；mpsc/stdio 共用统一 host |
| 改事件映射（ExecutorEvent → 协议） | `src/event/mapper.rs` + `src/event/tool_projection.rs` + `src/event/mod.rs` + `src/event/activity.rs` + `src/session/event_sink.rs` + `src/session/event_sink/{legacy,stdio}.rs` + `src/dispatch/session_replay.rs` | `map_event`；`map_agent_activity`；`project_tool_start` / `project_tool_completion`；`tool_result_content`（保留 mapper public 路径）；`TransportEventSink::push_event`；`push_legacy_event`；`StdioEventSink::push_event`；`AcpEvent` DTO | Transport sink 依次发送标准 update → safe activity → legacy，两个扩展面按各自 caps 门控；工具 live/replay 的 kind、完成状态、展示内容与 safe failure meta 由 `tool_projection` 统一；adapter 保留来源、replay 标记及 rawOutput 格式差异。ToolEnd live/replay 使用标准 `failed`/`completed`，同时写标准 `ToolCallUpdate.content` 与兼容 `rawOutput`，失败空文本有安全 fallback；SubAgent 来源写入 ACP 标准 `SessionNotification._meta.peri.sourceAgentId`（mpsc/stdio 同构，typed SDK 往返保留）；`CompactStarted/CompactCompleted` 经 `peri/agent_event` 透传 strategy、trigger 与安全计数供 TUI 展示；`BgRegistryEvent` 是私有功能载体：无标准 `SessionUpdate`，TUI 私有事件仍按 `agent_event` cap 门控，Hub/Web 仅经 `map_agent_activity` 输出去正文、哈希 correlation 的 capability-gated allowlist 摘要；契约 ARC-EVENT-001 |
| 改 Goal 状态、持久化与客户端投影 | `src/session/goal_state/mod.rs` + `src/session/event_sink/legacy.rs` + `src/event/{mod,mapper}.rs`；契约 DTO 在 `peri-acp-types/src/{goal,event,event_v2}.rs` | `GoalState::snapshot`；`GoalController::increment_continuation`；`StateEvent::GoalSnapshot` → `ExecutorEvent::GoalSnapshot` → `AcpEvent::GoalSnapshot` | continuation 计数归 session Goal 状态持有并随 Goal 持久化；Agent 每轮发只读快照，event sink 按 `agent_event` capability 投递给客户端，TUI 不直读 Agent/Middleware；契约 ARC-BOUNDARY-001 / ARC-EVENT-001 |
| 改事件发射/forwarder | `src/event/forwarder.rs` | `spawn_eventbus_forwarder(handles, on_event, bridge) -> JoinHandle<()>` | 消费 v2 EventBus 三通道（render/state/observe），**biased select：render 先于 state**（防 partial 污染）；主 executor/workflow 必须在 producer drop 后 await handle，禁止 terminal 越过 final usage；JoinError fail closed；Langfuse 在协议化前分支消费；observe Lagged 容错；映射后经 `on_event(UnstampedEvent, ExecutorEvent)` 送 event_sink |
| 改 Hub/Web 事件投影 | `src/event/activity.rs` | `map_agent_activity(&ExecutorEvent) -> Option<AgentActivityWire>`（:93）；`AgentActivityKind`（:19）/`AgentActivityStatus`（:36） | `peri.agentActivity` 安全摘要面：allowlist 字段 + `safe_label`/`truncate_utf8`/`hash_correlation` 清洗；禁止携带消息/路径/输出/错误正文；cap 未双向协商不投影 |
| 改 provider/模型/配置 | `src/provider/mod.rs`、`src/host/requests/config_options.rs`；core `peri-config/src/{provider,settings,system}.rs` | `LlmProvider::{from_config,from_config_for_alias,from_source,into_model}`；core `ConfigSource::save(expected_revision, &PeriConfig)`；`handle_set_config_option` / `handle_update_config` | 持久字段先构造并验证 candidate，保存成功后用 accepted snapshot 发布；失败不更新 live provider / agent cache、不 notify、不 success。更新同源会话连接保留各自 profile/frozen；延迟 UI draft 和远程 wire 的编辑基线 token 仍需审计 |
| 改 transport（新增传输） | `src/transport/mod.rs` + `mpsc.rs` + `stdio.rs` + `router.rs` | `AcpTransport` trait；`mpsc_transport_pair()`；`RequestRouter::{register,dispatch,close,wait_closed}`；`PendingRequest`；`StdioTransport::from_reader_writer` | router 以 owned pending handle 统一线性化 response、caller cancellation 与 terminal close，数字 ID 在正数域回绕并以 owner identity 防 stale handle 误删；终止以稳定 `Transport closed` 结算当前/后续请求，连接静默仍无隐式 timeout。MPSC 任一 pump/channel 关闭终止逻辑 pair，并保留已转发 incoming queue；stdio reader EOF/error 与所有 writer 路径汇入同一 terminal 状态。String response id 仍走 unmatched 转发；legacy `{"type":"cancel"}` 仍只在 stdio pump 精确拦截。契约：ARC-TRANSPORT-001；测试：`router_test.rs`、`mpsc_test.rs`、`stdio_test.rs`。 |
| 改 host 退出 / Langfuse 部署关闭 | `src/host/lifecycle.rs` + `src/host/shutdown.rs` + `src/host/task_scope.rs` + `src/session/mod.rs` | `spawn_acp_server`（lifecycle.rs:37）；`AcpHostHandle::shutdown`（:67）；`HostExitContext::finish`；`SessionManager::take_for_close`（session/mod.rs:249）/`AcpSession::close_resources`（:173） | 真实 host 任务保留至 join，取消等待不取走句柄；Incomplete 把任务和实际待关闭 session 留在退出 context 重试，完整 drain 后才使用 fresh assembly 的 non-Clone Langfuse 关闭权限；共享外部注入不授权（ARC-HOST-SHUTDOWN-001） |
| 改 prompt 组装（system prompt） | `src/prompt/mod.rs` + `src/prompt/section_validation.rs` + `prompts/sections/*.md` | `PromptTemplate::render`；`PromptEnv::frozen`（生产重渲染唯一入口，`local_probe` 仅测试）；`section_validation::{validate_section_override,sanitize_section_overrides}` | render 按 zone/order 拼接 section，并用 `peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY` 把 cached/uncached seam 交给 provider；剥离 token 后须保持旧 prompt bytes，empty 不生成 token（ARC-SERIAL-001）；日期与平台 / OS / git 状态在会话创建时注入（`FrozenRuntimeEnv`），禁止中途重读；section id 与 (zone,order) 唯一、Cached 段纯静态在构造期显式失败；覆盖文本按预算 / reserved marker / 已知占位符准入校验，非法覆盖拒绝应用并保留内置（H2/H3/M11/L3）；空内容段落过滤区分「有意为空的可选段」（`persona` / `language`，记 debug）与「必需段异常为空」（显式 warn + section id/来源/状态，不渲染且不伪造正文）（L2） |
| 改 HITL/AskUser 交互 | `src/broker/transport_broker.rs`（TUI/stdio 统一 broker，批 3 后无第二实现） | `AcpTransportBroker`、`impl UserInteractionBroker`（`request` 是完整转发 context 的串行化点）；`with_auto_approve` / `with_timeout`；`parse_ask_user_timeout` / `ask_user_timeout` | 同一 broker 实例的转发 Approval/Questions 共享 capacity=1 异步门，多 item Approval 不可被 Questions 插入；AutoApprove 锁前本地返回。审批逐 item 发 `session/request_permission` RPC（仅 allow_once/reject_once 两选项），问题聚合为单个 `elicitation/create` form；传输失败默认 Reject（防误放行）；提问超时兜底：统一构造点读 env `PERI_ASK_USER_TIMEOUT_SECS`（缺失/非法 → 默认 300s，`0` → 不超时）。`parse_elicitation_response` 对 cancel 先读 `_meta` 的 `peri.elicitationUnanswered`（`UnansweredCause`）：键存在 → `InteractionResponse::Unanswered`（取值无法识别收敛为 `Unknown`，不转述自由文本），仅键缺失才回落空 `Answers`；生命周期取消不声明原因 |
| 改命令路由/内置命令 | `src/session/command/mod.rs` + `src/dispatch/commands.rs` + `src/host/prompt.rs` + `src/host/notify.rs` | `register_builtins`（command/mod.rs:124，compact/clear/rewind/LoopPlaceholder）；`register_ui_entries`（commands.rs:73）/`ui_route_entries`（:38）；`stdio_filters_command`；`send_available_commands_update` | 注册顺序 = 内置 → 插件静态命令（`AcpServerConfig::plugin_command_entries`）→ 动态注入；技能命令由 MCP 发现异步投影：系统来源只注册 `core:{skill}` 裸名，外部 MCP 只注册 `{server}:{skill}`；`SkillsMiddleware` 关闭或系统实例断连会撤下系统命令。stdio 部署设置 `stdio_command_filter=true`：`clear`/`rewind`（含 alias）既不出现在 available commands，也不被 slash command 拦截，而是 fall-through 作为普通 prompt 进入 agent；TUI/print 保持命令行为，`session/rewind*` RPC 不受影响；`session/command/compact/pipeline.rs` **仅 re-export** `peri_agent::session::exec::compact_pipeline::execute_compact` |
| 改 cancel / continuation 链路 | `src/session/mod.rs` + `src/host/continuation.rs` | `SessionManager::cancel_session` / `cancel_all_agents` / `cancel_cascade_children_for`；`cancel_arms_continuation`（continuation.rs:66）；`run_continuation_scheduler`（:111） | 按 (session_id, turn_id, attempt_id) 三元组定位，clear_queue 默认 false；cancel 置位 `continuation_armed`（epoch 代际校验防过期执行，`continuation_still_valid` :89）；cancel > 续跑 > promote > retry 优先级由 Agent 判定；契约 ARC-CANCEL-001 |
| 改 caps 门控 | `src/session/caps.rs` | `set_pending_caps`（initialize 暂存）/`consume_pending_caps`（session/new 消费）/`ensure_session_caps`/`effective_host_caps` | 发送扩展事件前按该 session 的 caps 门控；cap 未双向协商不得投影；事件改动必须覆盖 caps 门控层 |
| 改装配/中间件链/部署 | `src/host/assemble.rs` + `src/host/prepared.rs` + `src/host/stage_builder.rs` | `assemble_server_config(HostAssemblyInput)`；`HostCapabilities` 随部署输入传至会话并在准备期冻结 builtin 关闭键；`HostAssemblyInput::workspace_input`；`assemble_hook_groups`；`build_stage_context`；`build_session_manager` | ACP stage bridge 只转发 `FrozenSessionData`，language/MetaHarness/date/prompt projection 均从该 snapshot 派生；部署能力缺席时不构造 Cron、插件、settings hooks、builtin 或 stdio MCP 相应能力，冻结关闭键同时过滤 10_hitl 静态提示清单；链序仍以 `production_blueprint` 为准；builtin 上下文在 `run_initialize` 前注入；非 Clone 会话存储关闭权仍由部署输入持有 |
| 改 beta flag 会话装配（`config.betas`） | `src/host/prepared.rs`（新会话从选中配置来源投影，随 frozen 冻结）+ `src/host/workspace.rs`（恢复用 blob；派生 Bash 缺省 + 诊断日志）+ `src/host/stage_builder.rs`（派生 Agent 缺省）+ `src/session/frozen.rs` / `frozen_snapshot.rs`（冻结值构建与持久化） | `FrozenContext::beta_flags`；`HostAssemblyInput::workspace_bash_default_run_in_background`；`StageBuildInput::agent_default_run_in_background` | 装配期读一次、随会话冻结：新会话取选中 `ConfigSource` 快照投影，恢复/fork 只认持久 blob（不按当前配置重建）；flag id（`peri_acp_types::beta_flags::FULL_ASYNC_TOOLS`）→ 语义布尔的解析**只**在这两处发生，工具/middleware 不接受 flag 语义；证据 `src/host/requests_beta_flags_test.rs`、`src/host/executor_flow_beta_flag_test.rs` |
| 改 bare 文件/终端能力 | `src/host/assemble.rs` + `peri-middlewares/src/mcp/{config,initialize}.rs` | `run_initialize_bare` → `load_bare_config` → 常规 `initialize_config` | 仅 builtin workspace；不读取用户 MCP 或插件配置，保留同一 session TaskManager；`PERI_MCP_BUILTIN=off` / `0` 仍生效。真实 CLI 验证：`cargo test -p peri-tui --test print_bare --test print_background_exit -- --test-threads=1` |
| 改 rewind | `src/dispatch/rewind.rs` + `src/session/command/rewind.rs` + `src/host/prompt.rs` | `rewind_preview`（:52）；`rewind_execute`（:215）；`rewind_candidates`（rewind_candidates.rs）；`stdio_filters_command` | `session/rewind*` RPC 仅在双向协商 `peri.rewind` 后可用：preview 返回有界 project-relative 文件影响 + 一次性指纹，execute 前重算历史，指纹缺失/过期拒绝；文件回退经会话绑定的 MCP pool 调 Workspace `workspace/rewindFiles`，Workspace 以 task scope capability 准入并预检，失败时 ACP 保持历史不裁剪。统一宿主注册使 stdio/TUI 都可调用 RPC（cap 未协商时 -32601）；stdio 的 slash `/rewind`（及 alias）从命令投影隐藏并 fall-through 进 agent，TUI/print 仍执行内置命令 |

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
| LLM 实例池 | session/agent_pool.rs | `AgentPool`；`has_valid_cache` / `invalidate`；完整 provider 配置（含 connection/key/options）经进程加盐 SHA256 形成内部指纹，阻止在途旧工厂回填后复用旧连接 |
| 目标状态 | session/goal_state/mod.rs | `GoalState`（:59，`set_goal` :80 / `snapshot` :167） |
| cron 桥与生产回归 | session/cron_bridge.rs、host/workspace.rs、host/requests_cron_test.rs | `SessionCronBridge` 跨 turn 存活；会话 scheduler 同时注入 builtin 与 bridge，部署 tick 策略沿 `WorkspaceAssembly` 传递；`cron_deployment_` 用例覆盖公开部署、双会话与真实 continuation |
| Cron 管理 ACP 请求 | host/requests/cron.rs、host/requests/cron_endpoint_test.rs | cron/list、cron/toggle、cron/remove 校验 session、closing、实际 environment 与持久 workspace scope；只消费会话 CronSchedulerPort，不回退宿主 scheduler |
| 状态构建 | session/state_builders.rs | `parse_permission_mode`（:19）/`apply_profile_effort`（:29）/`build_config_options`（:67） |

### src/event/（事件映射与转发）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 事件 DTO 与共享兼容转换 | event/mod.rs + peri-acp-types/src/event_v2/executor_mapping.rs | `AcpEvent` 使用 tag+content serde；`*_event_to_executor` 经 types crate 根路径 re-export，ACP 不复制另一套转换；TurnCompleted 来自 Render 层 |
| v1→协议映射 | event/mapper.rs | `map_event`（:51）；`MappedEvent`（:22，standard/standard_with_src） |
| 工具 live/replay 投影 | event/tool_projection.rs | `project_tool_start` / `project_tool_completion`；共享 kind/status/content/safe failure，adapter 保留兼容字段差异；`infer_tool_kind`（:73）先经 `original_tool_name_of_effective` 归一再用既有分支（`WebFetch` from a source-bound builtin tool → `ToolKind::Fetch`），**不新增** effective name 字面量分支，未知 / 外部 `mcp__*` 仍为 `Other`（ARC-TOOLS-001） |
| LLM usage 可选字段与来源 | event/mapper.rs + event/mapper_test.rs | `map_event` 的 `LlmCallEnd` 分支；有 usage 才产生 `UsageUpdate`，tokenStats cap 开启才附加计数 `_meta`；cacheReadTokens 缺省省略、显式零保留，sourceAgentId 与计数独立透传；TUI 消费入口见 peri-tui 索引与 ARC-EVENT-001 |
| 事件泵 | event/forwarder.rs | `spawn_eventbus_forwarder`（:78，biased select render 优先） |
| 安全活动投影 | event/activity.rs | `map_agent_activity`（:93，allowlist DTO） |
| OAuth 事件 | event/oauth.rs | `HostOAuthEvent`（:102，host 级通道，不依赖 session event_sink） |

### src/prompt/ 与 prompts/sections/（prompt 组装）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 环境检测 / 冻结 | prompt/mod.rs；`peri-acp-types/src/frozen.rs` 的 `FrozenRuntimeEnv` | `PromptRuntimeEnv::detect`（仅内容准入点探测一次）→ `PromptEnv::frozen` 消费冻结快照；旧快照缺字段 = unavailable（`RUNTIME_ENV_UNAVAILABLE`，不重探本地值） |
| 模板渲染 | prompt/mod.rs | `PromptTemplate::render`（按已装配持有者的 section 声明渲染；四态生成 cache boundary transport token） |
| section 模板 | prompts/sections/*.md | 纯 markdown 事实源，改文案改这里 |

### src/provider/（LLM 配置）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| Provider 构建 | `provider/mod.rs`；规则 `peri-config/src/provider.rs` | `LlmProvider::{from_source,from_resolved,from_config_for_alias,into_model}`；消费 `ResolvedProvider`，alias/profile/default/environment 规则归 core，ACP 适配具体 Model |
| 配置结构 | `provider/config.rs`（re-export）；事实源 `peri-config/src/app.rs` | `PeriConfig` / `AppConfig` / `ProviderConfig` / `ProfileConfig` / `Profiles`；领域 merge 与 validation 归 core |
| 配置加载/保存 | `provider/store.rs`（re-export）；`peri-config/src/{settings,system,source}.rs` | `ConfigSource::{load_at,load_lenient,snapshot,reload_merged}`、`save(expected_revision, &PeriConfig) -> Result<Arc<ConfigurationSnapshot>>`；caller 在编辑开始捕获 token，成功后消费 accepted snapshot；正常 source 持有 `ConfigurationSystem`，固定布局/同文件不拆层，workspace 相对差异保存与字节 CAS 保留兄弟域；输入 I/O 经独立配置 MCP，布局/校验失败不可误写全局；reload 显式，不热替换旧 pool 或 session prefix |

### src/transport/（传输抽象）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 传输 trait | transport/mod.rs | `AcpTransport`（:24） |
| mpsc 实现 | transport/mpsc.rs | `spawn_pump` 对任一方向关闭执行 pair 级 terminal；`send_or_close` 统一 outbound failure；`mpsc_transport_pair` 共享 router/ID 空间 |
| 原始帧桥 | transport/wire_bridge.rs | `WireBridge` 在外部 JSON-RPC 帧与现有 MPSC client transport 间双向转发；ACP 方法仍由共享 Host 分发，显式 `close` 结算 pending |
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
| 宿主所有权与服务入口 | host/mod.rs | `run_acp_server` / `run_acp_server_inner` 持有 deployment owner；`SessionState` 保持 frozen/agent_pool/continuation 与关闭状态，不保存执行 lease |
| 消息循环与请求分类 | host/server_loop.rs | `ServerLoop::run`；`spawn_prompt` / `spawn_mcp_apps_request` / `spawn_acp_mcp_request` 经 task owner 准入；`dispatch_request` 保持 response → after_new_response → 会话 setup 声明受理（`session_setup` + `requests::acp_mcp::attach_session_servers`）；`mcp/message` 通知在 `dispatch_notification` 内就地路由 |
| Prompt 编排 / 预测 | host/prompt_dispatch.rs + host/prediction.rs | `dispatch_prompt_turn` 保留 host 根 re-export；`spawn_prediction` 在原 prompt lock 范围内准入 |
| OAuth 事件投递 | host/oauth_delivery.rs | `spawn_oauth_consumer` / `deliver_oauth_event`；safe 与 legacy caps 分别裁决 |
| EOF 收尾 | host/shutdown.rs + host/lifecycle.rs | `shutdown_host` 借用唯一强 owner；先撤销准入，再取消并 drain 会话，最后关闭 MCP；`HostExitContext::finish` 在排空为 `Complete` 之后消费部署关闭权（`AcpServerConfig::session_store_shutdown`，仅装配点注入），未确认的关闭报 `Incomplete` 并保留上下文重试（重复关闭重新做真实检查） |
| 方法注册面（mpsc） | host/requests.rs + host/requests/*.rs | `handle_request`（requests.rs:22，30 个方法分派到子模块；各 handle_* 均为 `pub(super)` 定义在对应子文件） |
| MCP over ACP（client 声明的 `type: "acp"` server） | host/requests/acp_mcp.rs + host/server_loop.rs + host/workspace.rs | `attach_session_servers`（setup 响应后受理 `mcpServers`）；`AcpTransportGateway`（`AcpMcpGatewayPort` 实现：`mcp/connect` / `mcp/message` / `mcp/disconnect` 出站）；`route_inbound`（按 `connectionId` 定位承载会话服务）；`SessionEnvironment::shutdown` 调 `AcpMcpServerPort::close_session` | 会话 setup 各自解析、会话级服务持有连接；建连在后台（不阻塞会话建立，失败留在 MCP 池状态面），内层 MCP 错误码原样透传；契约 ARC-MCP-ACP-001 |
| ACP HTTP/stdio MCP 与 Agent 指令 | `host/requests/session_mcp_setup.rs` + `host/requests/session_lifecycle.rs` + `host/prepared.rs` + `host/assemble.rs` + `peri-middlewares/src/mcp/{client,initialize}.rs` | `session_mcp_servers`；`PreparedSessionInputs::build_frozen_after_activation`；`McpClientPool::set_session_servers` | `session/new`、冷 load/resume、fork 的 MCP 声明在池初始化前注入；bare 只消费显式 HTTP `workspace`，其余会话 MCP 保持关闭；HTTP `workspace` 取代内置实例，`tools/list` 是工具清单权威；新会话的 `_meta["peri.instructions"]` 写入 frozen system prompt，冷恢复复用快照。ACP `initialize` 仅协商 HTTP 能力，不承载实例配置 |
| notification 处理 | host/notify.rs | `handle_notification`（:28）/`extract_session_id`（:153）；`host/unify_wire_baseline_test.rs` 锁定发射面 payload 与 schema typed `SessionNotification` 的逐字段一致性；统一 host 入口见 ARC-STDIO-001 与 `docs/design/architecture.md` |
| prompt 执行编排 | host/prompt.rs | `run_prompt` 借用既有 AcpServerConfig 与当轮参数；`take_recall_for_turn`；保留 session 快照、Controller 执行及 canonical 结果回写顺序 |
| prompt 模型工厂 | host/prompt/models.rs | `build_model_factories`；闭包复用当轮 provider/config 快照与同一 session AgentPool，缓存按 provider fingerprint 校验 |
| prompt 观测装配 | host/prompt/telemetry.rs | `build_langfuse_hooks` / `build_forwarder_launcher`；turn hooks 与 bridge 共享 tracer，事件消费顺序仍归 event/forwarder.rs |
| prompt stage 装配 | host/prompt/stage.rs | `build_stage_bridge` / `build_compact_hooks`；逐次 stage 构造原 compact hooks，保留 host/prompt.rs 的 hook re-export |
| 续跑调度 | host/continuation.rs | `run_continuation_scheduler`（:111） |
| Host 任务所有权 | host/task_scope.rs | `HostTaskOwner` / `HostTaskSpawner`；生产 timeout driver + 测试 controlled phase driver |
| 装配 | `host/assemble.rs` | `assemble_server_config`；`build_legacy_frozen_data` 按保存目录准备配置与插件输入；新建 MCP pool 在初始化前绑定 `ConfigSource` snapshot，host Langfuse 消费同快照的 observability；观测成功时安装指标出口，未配置时不安装、指标不落盘 |
| stage 构建 | host/stage_builder.rs | `build_stage_context`：消费单一 `FrozenSessionData`，派生 frozen language/MetaHarness/date 与 Agent 装配输入，禁止从当轮 config 建第二事实源；frozen 中的项目指令正文来自 P4 内容准入期经 builtin `workspace` 实例读取的 `peri-instruction://workspace/{main\|local}` 资源（W5：`AgentsMdMiddleware` 为纯 adapter、不读盘） |
| workflow 薄壳 | host/workflow_agent.rs | `create_session_workflow_middleware`（:192，装配经 `WorkflowMiddlewareFactory` 端口）；生产工厂由 `host/assemble.rs` 经 `default_workflow_middleware_factory_with_pool(mcp_pool_concrete.clone())`（:514）注入**带 MCP 池**的实例，使 workflow agent 工具面与主链一样可见 selected builtin tools using raw names |
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
- ARC-MIDDLEWARE-001：中间件链序事实源 `production_blueprint`（peri-agent session 工厂），ACP 不重排；链内已无 Web / Artifact 槽位
- ARC-CAPABILITY-CLOSURE-001：builtin 实例关闭（策略键 / `disabled: true` / `PERI_MCP_BUILTIN=off`）必须同时从首个模型请求 tools、deferred 目录、subagent 继承面与 workflow agent 工具列表消失；ACP 侧的落点是 host seam 断言与 workflow agent 工厂注池（`host/assemble.rs`）。四个实例的声明为 web 2 + artifact 1 + workspace 7 direct、cron 3 deferred，因此后两面只对 direct 实例有实际过滤效果（`open_builtin_bridges` 只收 `is_direct()`），对 `cron` 是平凡成立
- ARC-SECRET-001：日志/错误/遥测不得泄露 secret（provider api_key 仅在 LlmProvider 内部持有）

- 部署关闭回归：`host/stdio/langfuse_shutdown_test.rs` 的真实尾部事件、waiter 取消、共享会话、MCP/session Incomplete 重试与 HTTP 失败终态；`transport/mpsc_test.rs::test_explicit_close_rejects_both_pending_directions_and_delivers_eof` 证明显式 close 结算双向 pending 并让两端 EOF。
- System MCP 启动准入回归（B-07 宿主 seam，契约 2/3/4）：`host/mcp_v4_startup_test.rs`（模块 `host::mcp_v4_startup_tests`，`host/mod.rs:52-54` 挂载；真实子进程 rmcp stdio + counting model + 临时 HOME，用例 `#[serial]` 且 `#[cfg(not(windows))]`）——命令 `cargo test -p peri-acp --lib -- host::mcp_v4_startup_tests`。它不覆盖 crate 内 seam（归 `peri-middlewares` 的 `mcp::mcp_v4_seam`）与工具调用策略（归 `mcp_host_policy_contract`）。
- Builtin MCP 宿主 seam（v4-part-2）：`host/mcp_v4_builtin_test.rs`（模块 `host::mcp_v4_builtin`，`host/mod.rs:56-57` 挂载）断言两个 builtin 实例在**首个模型请求**中暴露三个冻结 effective name、关闭面只收缩该请求面、被提升为 direct 的工具走完整审批链（approve ⇒ wire `tools/call` 恰一次；reject ⇒ 0 次）——命令 `cargo test -p peri-acp --lib -- host::mcp_v4_builtin`；`host/mcp_v4_wire_fixture_test.rs`（模块 `host::mcp_v4_wire_fixture`，`host/mod.rs:68-69` 挂载）是带 wire 日志与 `tools/call` 分支的夹具（迁移前基线与迁移后对照共用同一观察量），命令 `cargo test -p peri-acp --lib -- host::mcp_v4_wire_fixture`。文件名必须保留 `_test.rs` 后缀：该夹具引用业务 crate（`peri_middlewares` / `peri_model`），靠 `scripts/check-layer-imports.sh` 的测试文件豁免才不构成越层 import；模块名经 `#[path]` 保持不变。三者均不覆盖真实外网抓取与真实上传。v4-part-4 wave 3 的 session 级 seam **发送端**用例另见 `host/workspace_seam_test.rs`（模块 `host::workspace_seam`，`host/mod.rs:108-110` 挂载，命令 `cargo test -p peri-acp --lib -- host::workspace_seam`）：`session_environment_holds_the_workspace_input_it_injected` 断言会话环境产出的 per-session `TaskManager` 与 session 级 `on_bg_complete` 就是送进 builtin 上下文的那份、且经 `ensure_session_with_task_manager` 登记后仍是同一 `Arc`；`session_bg_complete_callback_defers_into_the_registered_session` 断言回调把 Shell 完成经 inbox 投为一条 `Defer` 并唤醒。该文件只断言到注入面——池的 `BuiltinInstanceContext` 读取面是 `peri-middlewares` 的 `pub(crate)`，宿主不可见（A33）。
- Workspace 资源面宿主 seam（W1/W3-J6/W4b）：`host/requests_skill_resources_test.rs`（模块 `host::requests::tests::skill_resources`）覆盖项目技能进冻结摘要、`disableBundledSkills=true` 隐藏 builtin、关闭实例 ⇒ 技能/指令不可得且**不回落磁盘**、`{cwd}/agents` 第二根接线与同 id 优先级、系统 skill 单一裸名命令及 `SkillsMiddleware` 关闭位撤下该命令；`host/requests_meta_resources_test.rs`（模块 `host::requests::tests::meta_resources`）覆盖段落覆盖经 `peri-meta://` 进入生产 `session/new`、覆盖不可得时保持内置且不读磁盘——命令 `cargo test -p peri-acp --lib -- host::requests::tests::skill_resources` 与 `cargo test -p peri-acp --lib -- host::requests::tests::meta_resources`。
- 子 Agent 请求面捕获（H1/H2）：`host/executor_flow_child_chain_test.rs`（模块 `host::executor_flow_tests::child_chain_tests`，`executor_flow_test.rs` 挂载）用真实子链装配（`peri_middlewares::subagent::SubagentChainAssemblerImpl::with_registry(会话 MCP skill registry)` + `build_subagent_middlewares` + `ExecuteExtraToolResolver`）、真实 durable 子会话与 `CapturePromptModel` 捕获定义型/fork/前后台/live resume 的最终 `ModelRequest`——命令 `cargo test -p peri-acp --lib -- child_chain_tests`。断言：项目指令/技能摘要/延迟工具目录/子身份各恰一次、父冻结身份与 `Agent`/`AskUserQuestion`/`Workflow` 工具不出现、关闭 AgentsMd/Skills/ToolSearch 后贡献与延迟入口缺席、resume 携带子会话历史与追加指令。装配器由测试夹具**本地构造同型实例**（ACP 层不引用 middlewares 装配入口）：生产接线在 `peri-middlewares/src/assembly.rs` → `subagent/tool/configuration.rs::with_mcp_skills`（宿主只提供会话 skill registry），故「宿主把 registry 接进工具」这一段接线漂移不被本文件捕获。不覆盖进程重启后的子会话执行（本分支无持久执行恢复）与真实 hook/permission。
- 子 Agent 冷恢复（宿主态重建）验收：本分支**没有**冷执行恢复路径（`host/cold_execution.rs` 随持久执行恢复一并移除），因此不承载该验收；相关覆盖由 `host::executor_flow_tests::child_chain_tests` 的 live resume 用例接续（同一持久 store、父侧新委派身份、子会话历史与追加指令）。
