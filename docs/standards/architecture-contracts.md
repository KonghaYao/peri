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
- **Rule**：会话创建时冻结日期、项目指引、skills 摘要和 system prompt；同一会话及其 SubAgent 复用冻结数据，禁止中途重新读取而改变 prompt 前缀。
- **Verify**：`cargo test -p peri-middlewares --lib frozen_claude_md`；人工检查 `build_frozen_data`、`from_frozen_parts` 与 SubAgent `with_frozen_data` 调用。

### ARC-EVENT-001

- **Scope**：v2 事件（`peri-acp-types/src/event_v2.rs`）、v1 `ExecutorEvent` 协议化载体、ACP 映射、TUI 通知。
- **Rule**：事件链路为单事实源 `Agent →(emit v2 事件，ObserveEvent 身份透传) →(协议序列化面映射) ACP →(协议化) TUI`：新增或变更事件必须覆盖完整链路——发射（v2 EventBus，唯一发射点，禁止 Agent 层构造 v1 `ExecutorEvent`；v1 中间态已退役，历史迁移记录已归档）、协议序列化面映射（`event_v2::*_event_to_executor`，穷尽匹配，禁止 wildcard 兜底，仅 ACP 协议化/发射侧同步映射使用）、ACP 映射/转发（`peri-acp/src/event/`）、能力门控（如适用）和客户端消费；终止事件必须使客户端离开 loading 状态。标准 ACP 没有 turn-error `SessionUpdate`：fatal turn failure 的 canonical wire 结果是 `session/prompt` JSON-RPC error response；`AgentExecutionFailed` 仅作 capability-gated 客户端兼容投影，不能替代标准响应。工具结果的 live/replay 投影必须同时保留标准 `ToolCallUpdate.content` 与兼容 `rawOutput`，失败状态使用标准 `failed`，失败展示文本不得为空。面向不同客户端的降维投影必须在 ACP 边界从 canonical event 构造独立、版本化、allowlist DTO，不能把 TUI 私有 `event_json` 原样转给 Hub/Web；cap 未双向协商时不得投影。v2_tx 双轨直连已下线（`2026-08-05-3.0-m-event-chain-canonical.md`），TUI 事件仅经 ACP 协议化路径，禁止恢复第二套事件投递。v1 兼容层仅保留协议序列化面需要的最小映射（在 `peri-acp-types`），wire format 不变。
- **Verify**：`cargo test -p peri-acp --lib mapper`（含 `variant_coverage_test` 的 map_event 穷尽断言）；`cargo test -p peri-agent --lib events_v2`（协议序列化面映射穷尽 + 身份透传）；`cargo test -p peri-agent --lib model_bridge`（流式事件 v2 直发，无 v1 中间态）；`cargo test -p peri-acp-types --lib identity`（canonical envelope / session_seq 单调契约）；人工检查 `peri-acp/src/event/`、事件 sink 和 `peri-tui/src/kit/acp_notifier.rs` 的对应分支。

### ARC-TOOLS-001

- **Scope**：工具注册、搜索与执行。
- **Rule**：工具以 `BaseTool::is_direct()` 自声明可见性；`true` 才直接进入 LLM tools，`false` 的工具只能由 `SearchExtraTools` 发现、`ExecuteExtraTool` 执行。包装层必须透传该 trait 语义。每个 turn 的真实能力事实源是 `build_session_tool_view` 产出的 session-local 工具视图：它应用 middleware disabled 状态与 agent allowlist/disallowlist 后再分类。ToolSearch 元工具的 direct capability 描述、deferred 索引和代理执行都必须绑定该视图，不得用静态核心工具清单推断运行时可用能力。
- **Verify**：`cargo test -p peri-middlewares --lib tool_search`；`cargo test -p peri-agent --lib session::exec`；检查 `peri-agent/src/session/exec/stage_builder.rs`、`peri-agent/src/agent/stages/reason.rs`、`peri-middlewares/src/tool_search/` 与包装工具实现；回归测试须覆盖 direct 工具被禁用或过滤后不再出现在元工具 description。

### ARC-HITL-001

- **Scope**：工具审批、用户提问、MetaHarness 关闭面与提示词段落。
- **Rule**：`PermissionMiddleware` 独占工具审批、`PermissionMode` 与 `10_hitl`；`HumanInTheLoopMiddleware` 独占 `AskUserQuestion` 与 `12_ask_user`。两项能力必须独立装配和关闭：middleware 缺席时，其工具、钩子和提示词贡献必须同时从当前 session-local 视图消失。`HumanInTheLoopMiddleware=false` 的现行语义是关闭提问，不再表示关闭审批；旧配置迁移不得静默按旧语义解释。
- **Verify**：`cargo test -p peri-middlewares --lib permission`；`cargo test -p peri-middlewares --lib hitl`；`cargo test -p peri-middlewares --lib assembly_test`；检查 `peri-acp-types/src/meta_harness.rs` 的 section holder 与 middleware/tool 清单、`peri-agent/src/session/factory.rs` 的 `[Permission, AskUser, SubAgent]` 蓝本。

### ARC-STDIO-001

- **Scope**：ACP stdio/IDE transport 与统一 host。
- **Rule**：stdio 请求必须经 `run_acp_stdio → run_acp_server_with_sessions → handle_request` 进入统一 ACP host，禁止恢复独立 typed-handler 业务路径。`session/new` response 必须先于首次 commands notification；stdio 的 rewind/clear 类输入不由命令层拦截，而是落入模型 prompt。legacy `type:cancel` 仅是全 session 强停兜底，不得被解释为标准 `session/cancel` 的身份、continuation 或队列语义。
- **Verify**：`cargo test -p peri-acp --lib host::stdio`；人工检查 `peri-acp/src/host/stdio/mod.rs` 与 `run_server_integration_test.rs` 的 initialize/new、response ordering、rename、prompt error、command filter 和 cancel 用例。

### ARC-WORKFLOW-RPC-001

- **Scope**：`peri-workflow` Node stdio JSON-RPC 与 agent 生命周期。
- **Rule**：RPC 请求必须先登记 pending 再写入；每帧使用 NDJSON 并 flush；stdout/child 结束必须 drain pending。`agent/run` 必须先注册再 spawn，kill/deregister 以所有权 token 防止旧 task 删除新句柄；kill 后 `killed` 是唯一终态，message loop 或 EOF 不得覆盖为 `failed`。
- **Verify**：`cargo test -p peri-workflow --lib rpc`；`cargo test -p peri-workflow --lib runner`；检查 `peri-workflow/src/rpc.rs` 的 `send_request`、`write_line`、`drain_pending` 与 `runner.rs` 的 register/spawn/kill 顺序。

### ARC-KEEPGOING-001

- **Scope**：`peri-tui`、`peri-acp`、`peri-agent`。
- **Rule**：空白 user prompt（按 content block 判空，必须用 `MessageContent::is_empty()`，禁止用 `text_content().trim()` 替代）是「继续跑 loop」指令（keepgoing），唯一生产者是 TUI keepgoing 按钮；keepgoing turn 不注入 recall；空历史 + 空白 prompt 时 ACP executor 短路返回，且必须发送终止通知（`push_done`）使客户端退出 loading；畸形请求（`message.content` 反序列化失败）静默落入 keepgoing 路径是防御性设计，各 transport 行为一致。
- **Verify**：`cargo test -p peri-agent --lib session::exec::executor_test`（`is_keepgoing` 判空三例 + keepgoing 短路 push_done + request_id 透传）；`cargo test -p peri-agent --lib stages`（`test_append_messages_empty_prompt_skipped` / `test_append_messages_whitespace_prompt_kept`）；人工检查 `peri-agent/src/session/exec/executor.rs` 短路分支与 `peri-tui/src/kit/submit_consumer.rs` 的 keepgoing 提交路径。

### ARC-SERIAL-001

- **Scope**：跨请求复用的 Prompt、工具注册与 provider payload。
- **Rule**：影响 prompt cache 的序列化顺序必须确定；不得直接依赖 `HashMap` 迭代顺序生成 tools 或其他缓存前缀。使用 `BTreeMap`、稳定排序或固定注册顺序，并保持包装层顺序不变。
- **Verify**：检查工具注册表及 provider payload 的收集路径；修改后运行相关工具注册测试，并比较相同输入的连续序列化结果。

### ARC-MIDDLEWARE-001

- **Scope**：生产中间件链。
- **Rule**：中间件顺序是行为契约，不得按名称、便利性或局部需求重排；链的唯一事实源是 Agent 层 session 工厂（链序蓝本 `production_blueprint`），装配实现位于 `peri-middlewares/src/assembly.rs`（依赖反转完成后物理迁入 Agent 层），详细任务入口为 `peri-middlewares/CLAUDE.md`。
- **Verify**：人工检查 `peri-agent/src/session/factory.rs` 的 `production_blueprint` 槽位顺序与 `peri-middlewares/src/assembly.rs` 的槽位构造（蓝本与构造一一对应，条件注册/Hook 组展开按装配实现判断）；修改该顺序时按 `docs/design/testing-standards.md` 增加或更新验证。

### ARC-SECRET-001

- **Scope**：配置加载、日志、错误、遥测、测试与提交。
- **Rule**：真实密钥、token、密码、私钥和连接串不得写入源码、fixture、日志、错误响应、遥测 payload 或版本库。运行时可通过环境变量、密钥管理或项目已支持且受本机权限保护的本地配置加载；输出和诊断只保留安全上下文。
- **Verify**：`git diff --check`；人工审阅变更中本地配置与环境注入、`tracing` 调用、错误格式化和测试 fixture，确认没有真实 secret 或完整认证信息。
