# 跨模块架构契约

只记录跨模块稳定不变量。具体测试规范见 `docs/design/testing-standards.md`。

### ARC-BOUNDARY-001

- **Scope**：`peri-tui`、`peri-acp`、`peri-agent`。
- **Rule**：TUI 的用户交互主路径经 ACP transport 调用服务；不得从 TUI 直接驱动 `peri-agent` 或 `peri-middlewares` 的 Agent 运行时。TUI 可在启动和配置层复用相关 crate 的类型与初始化能力，Agent 执行入口仍保持在 ACP 会话路径。
- **Verify**：人工检查 TUI 的 prompt、cancel、session 等请求经 ACP client/transport；RCRA 循环定义与 cancel 执行权在 Agent 层——循环本体为 `peri-agent/src/agent/stages/mod.rs` 的 `run_react_loop`（Receive 唯一退出口 + cancel 检查，Model 中止由 Agent 发起），子 agent 的 Cascade/Independent 判定与终止执行复用 `peri-acp-types/src/session.rs` 定义的 `cancel_cascade_agents` / `cancel_all_agents`（Agent session runtime 持有和调用；`AgentRuntime`/`CancelPolicy`/`AgentStatus` 的契约类型以 `peri-acp-types` 为事实源）；ACP 仅定位（`SessionManager::cancel_session` / `close_session` 查 session 映射）并传递 active_agents 注册表，经 `peri-agent/src/session/exec/executor_helpers.rs` 的 `build_and_execute_agent_v2` 驱动循环（L5 归位：执行本体在 Agent 层 `session/exec/`，ACP 侧仅协议化薄壳 `peri-acp/src/session/executor.rs` 与装配面宿主 `peri-acp/src/host/stage_builder.rs` 驱动调用）。

### ARC-CANCEL-001

- **Scope**：`peri-controller`、`peri-runtime`、`peri-agent`（cancel 链路）。
- **Rule**：cancel 链路为 `Controller →(定位转发) Runtime →(查映射) Agent 句柄`，按 (session_id, turn_id, attempt_id) 三元组定位（canonical 类型 `CancelRequest` 事实源 `peri-acp-types::identity`，含 clear_queue/policy 字段）；幂等判定与 turn 终态唯一（Completed/Interrupted）归 Agent 层（§9：Agent 持有最终执行权），上层只定位与转发、不解释取消语义；`clear_queue` 默认 false（cancel ≠ 清除待办，MQ 未消费消息保留为下轮 attempt 输入）；cancel > 续跑 > promote > retry 优先级由 Agent 判定。目标态接线随 L5 落位；当前 ACP 内部 `SessionManager::cancel_session` 为过渡路径。
- **Verify**：`cargo test -p peri-controller --lib controller`（cancel 三元组透传 / 未知 session 类型化 `ControllerError::CancelFailed`）；`cargo test -p peri-runtime --lib runtime`（cancel 转发幂等一致、clear_queue/policy 透传）；`cargo test -p peri-acp-types --lib identity`（`CancelRequest` 契约：三元组稳定、默认不清队列、序列化往返）；人工检查 cancel 请求自 TUI 经 ACP → Controller → Runtime → Agent 句柄全链路透传（L5 后复核）。

### ARC-FROZEN-001

- **Scope**：会话、Prompt、SubAgent。
- **Rule**：会话创建时冻结日期、项目指引、skills 摘要、MetaHarness 和 system prompt；同一会话及其 SubAgent 复用冻结数据，禁止中途重新读取而改变 prompt 前缀。冻结数据必须作为版本化 owner state 经 `ThreadStore` 专用接口持久化（不进入 `ThreadMeta` / list projection）：冷 `session/load` / `resume` 恢复原快照；legacy 缺失时只能用 `ThreadMeta.cwd` 构建，并以原子 write-once/CAS 回填，竞争失败方必须重读 winner。未知未来版本、损坏快照、metadata/store 错误均 fail closed 且不得覆盖原 blob。`fork` 继承 source 的精确快照；new/fork 写快照失败须补偿删除新 thread，禁止留下无 frozen owner state 的可用会话。
- **Verify**：`cargo test -p peri-acp --lib frozen_snapshot`、`cargo test -p peri-acp --lib test_session_load_cold_host_restores_original_frozen_prompt`、`cargo test -p peri-resources --lib frozen_snapshot`、`cargo test -p peri-middlewares --lib frozen_claude_md`；人工检查 `build_frozen_data`、`session/frozen_snapshot.rs`、session lifecycle 与 SubAgent `with_frozen_data` 调用。

### ARC-SESSION-LOAD-001

- **Scope**：`peri-tui` 普通 thread 切换、ACP session lifecycle。
- **Rule**：普通 thread load 必须在 producer 的同步入队边界取得引用计数 reservation，并持有到 `session/load` 提交或请求被丢弃；`ensure_session` 与所有 prompt 入口在选择 Stable session / 打开 prompt lease 前等待 reservation 清零。等待不得持有 lifecycle operation gate；Stable 决策或 `open_prompt` 必须在 reservation mutex 下线性化。send failure 不得把 guard 暴露给调用方，shutdown 必须能取消 in-flight load 并释放 reservation。compact replay 必须先 reserve load，再 drain buffered input，禁止依赖异步 consumer 调度顺序。
- **Verify**：`cargo test -p peri-tui --lib load_reservation`、`cargo test -p peri-tui --lib test_failed_load_dispatch_releases_reservation`、`cargo test -p peri-tui --lib test_shutdown_cancels_inflight_load_and_releases_reservation`、`cargo test -p peri-tui --lib test_compact_turndone_reload`；人工检查 `ThreadLoadDispatcher::send`、`AcpTuiClient::open_prompt_after_session_loads` 与 consumer drop/cancel 分支。

### ARC-EVENT-001

- **Scope**：v2 事件（`peri-acp-types/src/event_v2.rs`）、v1 `ExecutorEvent` 协议化载体、ACP 映射、TUI 通知。
- **Rule**：事件链路为单事实源 `Agent →(emit v2 事件，ObserveEvent 身份透传) →(协议序列化面映射) ACP →(协议化) TUI`：新增或变更事件必须覆盖完整链路——发射（v2 EventBus，唯一发射点，禁止 Agent 层构造 v1 `ExecutorEvent`；v1 中间态已退役，历史迁移记录已归档）、协议序列化面映射（`event_v2::*_event_to_executor`，穷尽匹配，禁止 wildcard 兜底，仅 ACP 协议化/发射侧同步映射使用）、ACP 映射/转发（`peri-acp/src/event/`）、能力门控（如适用）和客户端消费；终止事件必须使客户端离开 loading 状态。主 turn 返回终态前必须 await 本轮 root EventBus forwarder 完成，保证 final `LlmCallEnd/UsageUpdate` 先于 `AgentDone/TurnDone`；forwarder JoinError 是内部执行失败，不得超时后 fail-open，也不得提前返回空历史：仍须完成 transcript/recall/compaction 提取，并按 `AgentExecutionFailed → TurnEnded(Error/Internal) → done` 唯一收尾。辅助 agent 的 `LlmCallEnd` 必须以 `_meta.peri.sourceAgentId` 保留来源身份，不能覆盖父 turn 的 root usage；TUI 对每次 root usage observation 都必须替换最终样本，missing/zero/inconsistent observation 显式 clear，禁止保留更早的低 coverage 样本。标准 ACP 没有 turn-error `SessionUpdate`：fatal turn failure 的 canonical wire 结果是 `session/prompt` JSON-RPC error response；`AgentExecutionFailed` 仅作 capability-gated 客户端兼容投影，不能替代标准响应。工具结果的 live/replay 投影必须同时保留标准 `ToolCallUpdate.content` 与兼容 `rawOutput`，失败状态使用标准 `failed`，失败展示文本不得为空。面向不同客户端的降维投影必须在 ACP 边界从 canonical event 构造独立、版本化、allowlist DTO，不能把 TUI 私有 `event_json` 原样转给 Hub/Web；cap 未双向协商时不得投影。v2_tx 双轨直连已下线（`2026-08-05-3.0-m-event-chain-canonical.md`），TUI 事件仅经 ACP 协议化路径，禁止恢复第二套事件投递。v1 兼容层仅保留协议序列化面需要的最小映射（在 `peri-acp-types`），wire format 不变。
- **Verify**：`cargo test -p peri-acp --lib mapper`（含 `variant_coverage_test` 的 map_event 穷尽断言）；`cargo test -p peri-agent --lib events_v2`（协议序列化面映射穷尽 + 身份透传）；`cargo test -p peri-agent --lib model_bridge`（流式事件 v2 直发，无 v1 中间态）；`cargo test -p peri-acp-types --lib identity`（canonical envelope / session_seq 单调契约）；人工检查 `peri-acp/src/event/`、事件 sink 和 `peri-tui/src/kit/acp_notifier.rs` 的对应分支。

### ARC-TOOLS-001

- **Scope**：工具注册、搜索与执行。
- **Rule**：工具以 `BaseTool::is_direct()` 自声明可见性；`true` 才直接进入 LLM tools，`false` 的工具只能由 `SearchExtraTools` 发现、`ExecuteExtraTool` 执行。包装层必须透传该 trait 语义。每个 turn 的真实能力事实源是 `build_session_tool_view` 产出的 session-local 工具视图：它应用 middleware disabled 状态与 agent allowlist/disallowlist 后再分类。动态目录在 Reason 边界按 `catalog refresh → working map swap → before_reason_catalog → before_model → pin` 发布；ToolSearch 必须在专用 `before_reason_catalog` hook 中把索引与 Search/Execute request-local resolver 重绑到该 working map，禁止等待下一次 `before_agent` 或额外 turn。ToolSearch 元工具的 direct capability 描述、deferred 索引和代理执行，以及 PTC 的稳定工具目录与内部调用，都必须绑定该视图，不得用静态全局工具清单推断运行时可用能力。PTC 默认装配（`PtcMiddleware=false` 可关闭）；canonical `RunPtcCode` 是 deferred-only 工具，只能经 `SearchExtraTools → ExecuteExtraTool` 调用，不能直接进入 LLM tools；既有 direct tools 不受 PTC 影响。旧名 `run_code` 不可执行、不是 alias，仅作为 ToolSearch 迁移关键词。外层 `RunPtcCode` 是普通 Node.js 任意代码执行入口，必须与 Bash 同级审批。PTC 内部 `tools.*` 调用复用 canonical single-invocation seam；policy、HITL、事件与 tool card 均投影到 effective target，并沿用 timeout 与 cancel，不得嵌套完整 Act batch、写入 synthetic transcript 或重复结算 batch 状态。模型产生的 assistant raw wrapper call 仅为协议配对而保留，不代表执行目标或审批目标。内部事件 ID 必须可关联真实外层 `RunPtcCode` tool call。该 effective-target 审批不约束 Node 原生文件系统、进程、环境变量或网络 API，不得将 PTC 描述为 sandbox 或隔离环境。JavaScript 执行环境为 ESM-only：模块只能用动态 `await import(...)` 加载，static `import` 与 `require` 均不可用。
- **Verify**：`cargo test -p peri-middlewares --lib tool_search`；`cargo test -p peri-agent --lib session::exec`；检查 `peri-agent/src/session/exec/stage_builder.rs`、`peri-agent/src/agent/stages/reason.rs`、`peri-middlewares/src/tool_search/` 与包装工具实现；回归测试须覆盖 direct 工具被禁用或过滤后不再出现在元工具 description。

### ARC-CAPABILITY-CLOSURE-001

- **Scope**：middleware、provider、built-in capability 与 MetaHarness 开关。
- **Rule**：关闭能力必须在同一 frozen/session-local policy 下同时关闭 direct tools、deferred index/resolver、slash routes、ACP updates、TUI completion、静态 prompt/examples、subagent/workflow 继承与 runtime authorization。只隐藏 catalog/UI 或只移除 middleware 实例不算关闭；依赖其他 middleware 的能力必须显式验证依赖闭包。
- **Verify**：为每个可关闭能力运行 presence/absence 矩阵，覆盖注册、描述、发现、执行和客户端投影；检查 `build_session_tool_view`、middleware 装配、command registry、prompt section 与 ACP/TUI 投影都使用同一 session-local policy，禁止用静态全局名单替代；依赖能力关闭时必须验证 dependent 能力安全失败或同步消失。

### ARC-HITL-001

- **Scope**：工具审批、用户提问、MetaHarness 关闭面与提示词段落。
- **Rule**：`PermissionMiddleware` 独占工具审批、`PermissionMode` 与 `10_hitl`；`HumanInTheLoopMiddleware` 独占 `AskUserQuestion` 与 `12_ask_user`。两项能力必须独立装配和关闭：middleware 缺席时，其工具、钩子和提示词贡献必须同时从当前 session-local 视图消失。`HumanInTheLoopMiddleware=false` 的现行语义是关闭提问，不再表示关闭审批；旧配置迁移不得静默按旧语义解释。经同一 `AcpTransportBroker` 实例转发的 Approval 与 Questions 必须共享 capacity=1 的异步交互门，门覆盖完整 context（含多 item Approval），不得在 item 间交错；`ApprovalMode::AutoApprove` 是本地决策，必须绕过该门。同步单位是 broker 实例而非全局 transport 或 session identity；pending/terminal 结算仍只归 ARC-TRANSPORT-001。TUI 对 `session/request_permission` 与 `elicitation/create` 先按各自 schema 提取 non-empty session identity；invalid、no-active、stale、deleted 或 notification/bridge 投递失败必须用对应标准 cancel response 结算，且不得发布 pending UI state。exact-current 请求在 forward 前注册由进程内唯一 client instance、session generation、prompt epoch 与 checked token 组成的 semantic owner；typed RequestId 只作 wire identity。用户响应、bridge reject、session/prompt/cancel/transport terminal 通过同一 owner registry first-claim，session operation gate 同时序列化 claim、transition 与完整同步 UI publication。`new` 的未知目标 ordinary notification 仅进入容量 64 的 FIFO，目标 commit 后只刷新 exact-target；load replay 使用已知 target。`PromptLease`、transition 与已 claim settlement batch 的 Drop 必须同步撤销并把 owned plan 交给 weak-endpoint worker，不能恢复 owner 或保活 notifier/transport。冻结请求 A 的 owner-aware terminal cleanup 不得关闭后来活跃的 B surface；AskUser 本地表单 fingerprint 必须包含 owner identity。
  Startup session lifecycle 同属该 operation gate：initial 与 first-submit 必须共用 gate 内 Stable 重检的 `ensure_session`；resume/continue 必须在 submit consumer 可达前建立 client-owned restore reservation，异步 lookup 后在同一 gate 内 load 或释放 reservation，禁止延迟 startup producer 替换首个 prompt 的 session。显式 `/clear` 保持 unconditional new。
- **Verify**：`cargo test -p peri-middlewares --lib permission`；`cargo test -p peri-middlewares --lib hitl`；`cargo test -p peri-middlewares --lib assembly_test`；`cargo test -p peri-acp --lib -- broker::transport_broker`；`cargo test -p peri-tui --lib -- acp_client::client::reverse_tests`；`cargo test -p peri-tui --bin peri -- cli_print`；`cargo test -p peri-tui --lib acp_notifier`；`cargo test -p peri-tui --lib acp_bridge`；`cargo test -p peri-tui --lib acp_events`；检查 `peri-acp-types/src/meta_harness.rs` 的 section holder 与 middleware/tool 清单、`peri-agent/src/session/factory.rs` 的 `[Permission, AskUser, SubAgent]` 蓝本，并检查 `AcpTransportBroker::request` 的 AutoApprove 锁前返回与完整转发 context guard。

### ARC-STDIO-001

- **Scope**：ACP stdio/IDE transport 与统一 host。
- **Rule**：stdio 请求必须经 `run_acp_stdio → run_acp_server_with_sessions → handle_request` 进入统一 ACP host，禁止恢复独立 typed-handler 业务路径。`session/new` response 必须先于首次 commands notification；stdio 的 rewind/clear 类输入不由命令层拦截，而是落入模型 prompt。legacy `type:cancel` 仅是全 session 强停兜底，不得被解释为标准 `session/cancel` 的身份、continuation 或队列语义。
- **Verify**：`cargo test -p peri-acp --lib host::stdio`；人工检查 `peri-acp/src/host/stdio/mod.rs` 与 `run_server_integration_test.rs` 的 initialize/new、response ordering、rename、prompt error、command filter 和 cancel 用例。

### ARC-TRANSPORT-001

- **Scope**：ACP stdio 与 MPSC transport 的 request 生命周期。
- **Rule**：transport 终止必须以稳定的 `AcpError(-32603, "Transport closed")` 结算当前及后续 request；匹配 response、caller cancellation 与 terminal close 对同一 pending request 至多生效一次。连接仍存活但对端静默不等于终止，不得为 `send_request` 隐式增加通用 timeout；发起 I/O 失败的调用保留其具体 write/flush 错误，同时使同一逻辑连接进入 terminal 状态。
- **Verify**：`cargo test -p peri-acp --lib -- transport::router`、`cargo test -p peri-acp --lib -- transport::mpsc`、`cargo test -p peri-acp --lib -- transport::stdio`；人工检查 stdio EOF/read/write/flush 与任一 MPSC pump/channel 关闭均汇入同一 terminal 生命周期。

### ARC-HOST-SHUTDOWN-001

- **Scope**：`peri-acp` host/stdio，`peri-middlewares` MCP pool，`peri-tui` MCP panel deployment。
- **Rule**：ACP transport 终止是一个显式所有权事务：先关闭 host 任务准入并取消会话，再在可控 cooperative grace 后 abort/drain host-owned 任务；对 MCP 必须按 `pool.begin_shutdown → McpTaskOwner.begin/shutdown → pool.shutdown` 顺序执行。`HostTaskOwner` 与 `McpTaskOwner` 均是 deployment-held、non-Clone 强 owner；ACP 仅经 `peri-acp-types::ports::McpTaskOwnerPort` 持有 boxed owner capability，具体 `McpTaskOwner` 与 keyed registry 仍归 `peri-middlewares`。配置/task/pool 只保留 weak spawner，callback/notifier 只弱引用 pool；pool 不得反向持有会捕获 `Arc<McpClientPool>` 的 task handle。MCP 的 init/OAuth/reconnect/subscription 准入与 owner 注册在 pool lifecycle gate 下线性化，`Open → Closing` 后 callback 注册、新连接/service 发布与新任务均拒绝。Pool service-close 必须由 pool state 持有单一、不捕获 pool 的 shutdown worker；调用者取消、并发或重试只能继续观察同一事务，不能取得 drained service 的唯一所有权。只有每个 service 的 close/join 终态均已记录才可发布 `Closed`；`close_with_timeout == Ok(None)` 必须显式报告 `Incomplete` 并保持 `Closing`。EOF 对 local 与 `SessionManager` ID 并集执行 pre-close/close，任何 task join、LSP/MCP close 或外部 callback 都不得持有 session/lifecycle/registry/services 锁。超过 abort-drain guard 只能报告 `Incomplete` 并保持 `Closing`，不得宣称已释放图。
- **Boundary**：本契约只 join 本轮显式迁移的 host/MCP 任务；Agent `TaskManager`、Permission 等待、prewarm discovery、compact hooks、command notifications、event forwarders 与 request-level session close/delete LSP parity 仍是独立边界。
- **Verify**：`cargo test -p peri-acp --lib -- host::task_scope`、`cargo test -p peri-acp --lib -- host::stdio::run_server_integration_tests`、`cargo test -p peri-middlewares --lib -- mcp::task_scope`、`cargo test -p peri-middlewares --lib -- mcp::client`、`cargo test -p peri-tui --lib -- app::mcp_lifecycle_tests`。

### ARC-WORKFLOW-RPC-001

- **Scope**：`peri-workflow` Node stdio JSON-RPC 与 agent 生命周期。
- **Rule**：RPC 请求必须先登记 pending 再写入；每帧使用 NDJSON 并 flush；stdout/child 结束及 malformed protocol frame 必须 drain pending，不能静默丢帧。`agent/run` 必须先注册再 spawn，kill/deregister 以所有权 token 防止旧 task 删除新句柄；`workflow/start` 失败或超时必须移除 active channel；kill 后 `killed` 是唯一终态，message loop 或 EOF 不得覆盖为 `failed`。
- **Verify**：`cargo test -p peri-workflow --lib rpc`；`cargo test -p peri-workflow --lib runner`；检查 `peri-workflow/src/rpc.rs` 的 `send_request`、`write_line`、`drain_pending` 与 `runner.rs` 的 register/spawn/kill 顺序。

### ARC-KEEPGOING-001

- **Scope**：`peri-tui`、`peri-acp`、`peri-agent`。
- **Rule**：空白 user prompt（按 content block 判空，必须用 `MessageContent::is_empty()`，禁止用 `text_content().trim()` 替代）是「继续跑 loop」指令（keepgoing），唯一生产者是 TUI keepgoing 按钮；keepgoing turn 不注入 recall；空历史 + 空白 prompt 时 ACP executor 短路返回，且必须发送终止通知（`push_done`）使客户端退出 loading；畸形请求（`message.content` 反序列化失败）静默落入 keepgoing 路径是防御性设计，各 transport 行为一致。
- **Verify**：`cargo test -p peri-agent --lib session::exec::executor_test`（`is_keepgoing` 判空三例 + keepgoing 短路 push_done + request_id 透传）；`cargo test -p peri-agent --lib stages`（`test_append_messages_empty_prompt_skipped` / `test_append_messages_whitespace_prompt_kept`）；人工检查 `peri-agent/src/session/exec/executor.rs` 短路分支与 `peri-tui/src/kit/submit_consumer.rs` 的 keepgoing 提交路径。

### ARC-SERIAL-001

- **Scope**：跨请求复用的 Prompt、工具注册与 provider payload。
- **Rule**：影响 prompt cache 的序列化顺序必须确定；不得直接依赖 `HashMap` 迭代顺序生成 tools 或其他缓存前缀。使用 `BTreeMap`、稳定排序或固定注册顺序，并保持包装层顺序不变。`PromptSectionZone` 的 cached/uncached seam 必须跨 `String` handoff 保留：唯一保留控制字由 `peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY` 定义，所有 provider 构造 wire request 前必须消费且不得泄漏。支持显式 cache breakpoint 的 adapter 以单一控制字区分静态/动态 system block；重复控制字须剥离全部并对显式 breakpoint fail-closed（整个 system 不显式缓存）。不支持显式 breakpoint 的 adapter 只做字节守恒剥离，不宣称控制 provider 的隐式缓存。无控制字输入是否采用 legacy fallback 由具体 adapter 契约决定。
- **Verify**：检查工具注册表及 provider payload 的收集路径；修改后运行相关工具注册测试，并比较相同输入的连续序列化结果。运行 `cargo test -p peri-acp --test prompt_cache_boundary`、`cargo test -p peri-model --lib -- system_cache`，覆盖四态字节守恒、显式 breakpoint adapter 的重复控制字 fail-closed、provider wire 无控制字及动态 suffix 顺序。

### ARC-MIDDLEWARE-001

- **Scope**：生产中间件链。
- **Rule**：中间件顺序是行为契约，不得按名称、便利性或局部需求重排；链的唯一事实源是 Agent 层 session 工厂（链序蓝本 `production_blueprint`），装配实现位于 `peri-middlewares/src/assembly.rs`（依赖反转完成后物理迁入 Agent 层），详细任务入口为 `peri-middlewares/CLAUDE.md`。
- **Verify**：人工检查 `peri-agent/src/session/factory.rs` 的 `production_blueprint` 槽位顺序与 `peri-middlewares/src/assembly.rs` 的槽位构造（蓝本与构造一一对应，条件注册/Hook 组展开按装配实现判断）；修改该顺序时按 `docs/design/testing-standards.md` 增加或更新验证。

### ARC-SECRET-001

- **Scope**：配置加载、日志、错误、遥测、测试与提交。
- **Rule**：真实密钥、token、密码、私钥和连接串不得写入源码、fixture、日志、错误响应、遥测 payload 或版本库。运行时可通过环境变量、密钥管理或项目已支持且受本机权限保护的本地配置加载；输出和诊断只保留安全上下文。
- **Verify**：`git diff --check`；人工审阅变更中本地配置与环境注入、`tracing` 调用、错误格式化和测试 fixture，确认没有真实 secret 或完整认证信息。

### ARC-PTC-ARTIFACT-001

- **Scope**：`@peri-code/ptc` npm package、Rust 固定版本安装/启动路径与 PTC wire handshake。
- **Rule**：生产 PTC package 的固定 identity 是 `@peri-code/ptc@0.2.3`。Rust 不内嵌 artifact、不从仓库 `dist` 启动，也不在 Cargo 构建时要求 Bun；缓存缺失或无效时在清空继承环境后，仅以 `PATH`、受控临时 `HOME`/cache 和公共 registry 执行 `npm install --ignore-scripts --no-audit --no-fund --no-update-notifier --prefix <staging> @peri-code/ptc@0.2.3`。安装与 adapter 运行均不得继承 token、cloud 凭据、`NODE_OPTIONS` 或完整宿主环境；adapter 运行仅保留 `PATH`，Windows 额外 allowlist `SystemRoot`、`WINDIR`、`TEMP`、`TMP`。默认仅支持公共 registry，私有 registry 必须预装或显式提供最小安全配置。缓存变更由跨进程 lockfile 串行化；损坏 target 在锁内 rename 到 quarantine，绝不直接删除可能正在使用的目录；rename 冲突必须验证 winner。Artifact entry 必须先以 canonical path 验证其仍位于 package 内，再以普通 absolute path 传给 Node argv；不得将 Windows verbatim `\\?\` path 作为 CLI entry。Node 必须在接收 source 前完成 `ptc/start` protocol/build handshake，启动或 handshake 协议失败时仅将本地 cache 来源在 cleanup 后锁内隔离，用户 source 执行失败和 npx fallback 不得清缓存。默认 npm 安装失败时安全失败；仅当 `PERI_PTC_ALLOW_NPX_FALLBACK=1` 时允许精确版本 `npx` fallback。npm/npx 不得输出 stderr 或泄漏 token/source。发布顺序固定为先运行 `npm run prepublishOnly`，成功后再运行 `npm publish`。
- **Verify**：检查 package metadata、TypeScript 常量与 Rust 常量同步；用临时 prefix 和可注入 installer/mock 覆盖 valid、wrong identity、path escape、固定版本命令、原子 rename 与 fallback，测试不得访问网络；发布前运行 `npm run prepublishOnly`，文档变更运行 `git diff --check`。
