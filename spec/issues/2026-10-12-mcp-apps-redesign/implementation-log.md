# MCP Apps 重设计：实现日志

契约来源：`~/Downloads/2026-10-12-peri-mcp-apps-redesign.md`（设计建议，本轮开始实现）。
本文件记录**已落地的改动、验证证据、已知缺口**，供中断后恢复。

- worktree：`.worktrees/peri-mcp-apps`
- 分支：`refactor/mcp-apps-canonical-invocation`
- 基线：`origin/main` @ `f01098c66`（已确认：与设计文档基线 `607f8a222` 之间，全部 Apps 相关源码逐字节相同）
- 构建命令：`./scripts/cargo-rmcp-patched.sh check|test --locked ...`（`patches/README.md` 要求，不要用裸 cargo）

## 验证基线

冷编译 `check --locked --workspace` 通过（5m48s，零错误）。

## 阶段 1：执行契约 —— 已完成

设计目标：typed origin、直接 outcome、真实 invocation context；模型行为与链序不回退。

### 1.1 `peri-acp-types` 契约类型

新增：

- `tool_invocation.rs` — `InvocationId`（UUID v7 newtype）、`ToolCallId`、`InvocationOrigin{Model,Nested,App,Host}`、`EffectiveToolCall{invocation_id, tool_name, input, origin}`（原 `parent_invocation_id` 字段删除）。
- `tool_outcome.rs` — `RawToolResult`（transparent `Value` 保真承载）、`JsonRpcError`、`RemoteToolError{jsonrpc,in_band}`、`ToolOutcome{Unreported|Reported{is_error,raw}}`。

变更：

- `ToolOutput` 新增 `outcome` 字段与 `from_raw_result/with_outcome/outcome()/is_error()/raw()`。
- `EffectiveToolError` 新增 `remote_error: Option<RemoteToolError>`。
- `EffectiveToolDispatcher` 收敛为单入口 `dispatch() -> Result<ToolOutput, EffectiveToolError>`（`dispatch_output` 删除）。
- `ToolContext` 用 typed `InvocationId/InvocationOrigin/ToolCallId`，新增 `with_invocation(...)`。
- `mcp_apps.rs` 的 `RawCallToolResult`/`JsonRpcError` 改为 re-export，**消除同名异物**。

证据：`check -p peri-acp-types --all-targets` 通过；`test -p peri-acp-types --lib` = 562 passed。

### 1.2 `peri-agent` canonical invocation + `InvocationContext`

新增 `peri-agent/src/agent/stages/invocation_context.rs`（319 行，`pub(crate)`）：

- session 只读事实（session_id/run_id/agent_id/cwd/messages 快照/context_usage/session_resources）
- `ExecutionEntryOrigin`（模型批次逐条特化）
- `turn: Option<ModelTurnFacts>` —— **可选**，为阶段 2 的 idle invocation 预留；无 turn 时显式拒绝，不伪造
- 端口：`InvocationQueuePort`、`EventBus`、`cancellation`、`resolver`、`middleware_chain`

所有权链切断（关键）：`StageContext` 不再被 dispatcher 持有 → `lease → dispatcher → {InvocationContext, pinned catalog}`。
`run_before_tool_approvals`/`dispatch_concurrent`/`settle_results` 只读 `InvocationContext`；`run_after_tool`/`run_on_error` 用窄状态 `InvocationState`（`QueueState` 绑会话队列）；`run_interruptible_hook` 只收 `CancellationToken`。

证据：`check -p peri-agent --all-targets` 通过；`test -p peri-agent --lib` = 1064 passed（另跑 `--tests`、`--doc` 均绿）。

### 1.3 MCP bridge 直接返回 raw outcome（修 [P2] 结果旁路）

- `McpToolBridge::invoke_output` 返回 raw 保真的 `ToolOutput`；`isError:true` 不再是提前返回的错误路径，而是结果事实 `ToolOutcome::Reported{is_error:true, ...}`。
- 删除 `mcp-app:` 字符串前缀来源判定 → typed `InvocationOrigin`。
- 删除 `McpAppBindingLeaseRegistry.raw_results` 及 `record_raw_result`/`take_raw_result`/`purge_raw_results_for_connection`、cleanup 的 TTL/容量段。
- relay 直接消费 dispatcher 返回值，不再「自己拼 invocation 字符串去 registry 捞 raw」；删除 `apps_relay.rs:258-259` 与源码相反的注释。
- `peri-acp` 侧跟随修复（`session_replay.rs` 等）。

证据：`check --workspace` 通过；`test -p peri-middlewares --lib` = 1790 passed；`test -p peri-acp --lib` = 822 passed；ARC-MCP-APPS-001 的两条 Verify（`mcp::apps` 25 passed、`host::mcp_apps` 9 passed）通过。

### 阶段 1 已知缺口（如实记录）

1. `apps_invoke.rs` 的 Host invoke **仍恒 `PolicyDenied`（fail-closed）**——按设计属阶段 2「共同审批入口」，本轮有意未放通。
2. lease 仍持 `dispatcher` 与 `ctx.cancellation.clone()`；移除条件：阶段 3 改由 canonical 入口按 App session 身份取用。
3. 上游 JSON-RPC 的顶层未知扩展字段在 rmcp `CallToolResult` 解析时仍会丢（`RawToolResult` 已保真承载已解析部分）；覆盖需 transport 层抓原始 JSON。
4. 6 个既有测试的语义由「线路 `isError` → `Err`」翻转为「`Ok` + `is_error=true`」（符合设计 §5 的 Completed 定义），文本断言逐字保留。
5. `peri-agent --lib` 曾出现 1 次不可复现的失败（并发 cargo 负载下），随后 3 次连跑均 1064 passed。

## 阶段 2：session 执行环境 —— 进行中

### 2.1 `McpAdmissionPreparation`（extract session-owned 原语）—— 已完成

按设计 §4.2「只抽取不依赖输入的 MCP readiness / candidate 准备原语，撤回整体前移」：

- 新增 `peri-middlewares/src/mcp/admission.rs`（338 行）：持 `tool_pool`/`session_id`/`cancel`/`closed_instances`（全部 session 级事实，无 turn 输入），迁入 `await_system_ready`/`open_typed_bridges`/`recheck_system_snapshot`/`startup_tool_update`/`startup_agent_error` 与 `SystemReadySnapshot`。
- `McpMiddleware` 由持有改为**委托**（`admission()` 投影构造），`middleware.rs` 933 → 719 行。
- hook 顺序与错误映射逐字不变：`await_system_ready → startup_tool_update → stage_startup_tools`。
- 未接线任何 idle 调用方（按设计留给后续批次）。

证据：`check -p peri-middlewares --all-targets` 通过；`test -p peri-middlewares --lib` = 1790 passed；`mcp::middleware` 44 passed；`mcp::tool_bridge` 26 passed；`mcp::apps` 25 passed。

### 2.2 审批按可信 owner 路由 + session entitlement 注入点 —— 已完成

按设计 §4.3（审批与身份认证分别处理）：

- **`TrustedInteractionOwner`（`peri-acp-types/src/interaction.rs`）**：`Model{session_id,turn_id,attempt_id}` / `App{app_id,attachment_id,connection_epoch}` / `Host{namespace,seq}`；`AttachmentId` / `ConnectionEpoch` newtype（完整 Attachment / RequestLedger 仍留阶段 3）。`UserInteractionBroker::request(owner, ctx)` 签名携带领属。
- **归属铸造（单一权威 `peri-middlewares/src/interaction_owner.rs`）**：middleware 取 `BeforeToolState::{session_id, execution_binding}`（Agent 层新增 `StateView::session_id`，`InvocationState`/`BoundApprovalState`/`AgentContext` 从真实 `InvocationContext`/`StageContext` 投影）；工具取 `ToolContext::{session_id, turn_binding}`（新增字段，canonical dispatch 从 `TurnContext::execution_binding` 绑定）。**缺失即显式 `ToolRejected`**（`UNTRUSTED_OWNER_REASON`），不伪造 turn 身份、不静默跳过、不降级为自动批准；Bypass / 免审批分支不铸造归属，行为不变。
- **互斥门升为 session-owned**：门由 `SessionState.interaction_gate`（host session 状态，跨轮存活）持有，`AcpTransportBroker::new(transport, session_id, gate)` 借用；`run_prompt` 每轮仍新建 broker（endpoint 可替换），但同一 session 跨轮互斥。`host_request_seq` 为 Host 程序化交互序号（`SCHEDULED_TRIGGER_NAMESPACE` 用于 cron 触发审批，归属只能是 Host——先于模型 turn，没有 turn 事实）。
- **`SessionAuthorizationPort`（`peri-acp-types/src/session_authorization.rs`）**：`grant()` 默认实现恒 `None` = fail-closed；`SessionGrant::covers` 要求三元组全等；文档写明与 `SessionAccessPort` 的区别（运行时数据访问 ≠ 授权证明）。**未接入 Model turn 审批路径**（消费者是后续批次的 session 级 admission owner）。
- **`InteractionUnavailable` 落点**：扩展既有 `UnansweredCause::NoOnlineEndpoint`（单一权威，不造平行枚举），既有 `NonInteractiveClient`/`Unknown` 语义不变；审批（permission 单条/批量、hook ask）与提问（AskUser）三条消费路径都已按「未获批准 / 如实转述原因」处理，绝不伪装成「用户拒绝」。生产者（session 交互路由判定无在线 endpoint）属后续批次。

证据：`check --locked --workspace`（含 `--all-targets`）通过；`test -p peri-acp-types --lib` = 575 passed、`--doc` = 6 passed / 2 ignored；`test -p peri-acp --lib` = 823 passed（新增 session-owned 门跨轮共享 + 跨 session 不共享两条）；`test -p peri-middlewares --lib` = 1790 passed。

### 2.2b 审查发现的正确性缺口修正 —— 已完成

只修正确性，不做结构性重构；范围 = `peri-middlewares`、`peri-agent`（少数投影/调用点）、
`peri-acp-types`（仅 typed 错误）。

1. **会话身份缺失不再回退 `agent_id`**（高）：`StageContext::project_session_id` 返回
   `Option<String>`，缺失即 `None`；`InvocationContext::session_id` / `AgentContext::session_id`
   / `InvocationState::session_id` 如实转述；`ToolContext.session_id` 缺失即不绑定
   （`with_session_identity` 未被调用，`turn_generation` 仍取真实 turn）。此前
   `Project_session_id` 会把 `agent_id` 当会话身份，铸造出
   `TrustedInteractionOwner::Model { session_id: <agent_id> }`，使 `untrusted_owner_error`
   成为死分支。回归：`interaction_owner_test.rs`（6 条）、
   `tests/mcp_host_policy_contract.rs::missing_session_identity_is_fail_closed_and_never_reaches_the_wire`
   （缺归属 ⇒ 零 broker 交互 + 零 MCP wire 调用 + 显式失败）。
2. **`require_turn` typed 化 + `turn: None` 真实构造路径**：新增
   `AgentError::NotAvailableWithoutTurn { entry, session_id }`（`peri-acp-types/src/error.rs`，
   分类 `not_available_without_turn`），`require_turn` 不再返回 `Other(anyhow)`；审批入口
   `run_before_bound_tools_batch` 无 turn 时逐条返回**同一个 typed 错误**（不降级为
   `MiddlewareError`；`AgentError` 不实现 `Clone`，按同一 entry + session 身份重建同值）；
   新增 `InvocationContext::from_stage_idle` 作为 `turn: None` 的唯一构造路径（供阶段 2.4
   idle 入口接线，当前 `#[allow(dead_code)]`，接线后删除）。回归：
   `invocation_context_test.rs::execution_without_turn_is_explicitly_unavailable`、
   `tool_dispatch/execution_test.rs::idle_entry_returns_typed_not_available_without_turn`、
   `middleware_runner_test.rs::bound_approval_without_turn_returns_typed_unavailable_per_call`、
   `peri-acp-types/src/error_test.rs::not_available_without_turn_is_a_typed_category_with_diagnostics`。
3. **5 处 fail-closed 分支补回归**：permission 单条/批量、hook `ask`、`AskUserQuestion`、
   MCP Agent 激活。各自断言显式拒绝 + 零 broker 交互 + 原因含 `UNTRUSTED_OWNER_REASON`。
   MCP Agent 激活用真实线路夹具做**路径证明**（归属齐备时同一 activation 恰好一次交互并放行），
   证明拒绝来自归属缺失而不是激活失败（用例在 `tool_test/mcp_activation_owner_test.rs`，
   从 `tool_test.rs` 拆出以满足 STD-SIZE-001）。
4. **批量审批过度拒绝修正**：`batch_broker_approve` 在归属缺失时改为逐条判定（只拒绝
   **待审批**条目，免审调用照常放行），与正常路径粒度一致。回归：
   `permission/untrusted_owner_test.rs::batch_approval_without_trusted_owner_rejects_only_calls_needing_approval`。
5. **`Unanswered` 三条路径补回归**（permission 单条/批量、hook `ask`），含
   `NoOnlineEndpoint` 与 `NonInteractiveClient`；语义锁死为「如实转述、未获批准」，不伪造
   「用户拒绝」。

已知缺口（本轮如实记录，未修）：
- `Unanswered` 与「用户拒绝」在 typed 层不可区分（都收敛到 `AgentError::ToolRejected`，
  执行层统一映射 `EffectiveToolErrorCode::UserRejected`）——可区分性只在诊断文本。要恢复需在
  `peri-acp-types` 增加失败分类并改执行层映射，超出本轮授权（见
  `permission/unanswered_test.rs::unanswered_and_user_rejection_share_one_typed_failure`）。
- MCP Agent 激活的「零 wire 调用」做不到：审批键依赖已激活内容（`approval_key` 由
  `registry.activate` 的正文与 digest 导出），因此定义读取（`resources/read`）必然先于归属检查。
  测试改为断言零 broker 交互 + 显式拒绝 + 路径证明。

证据：`check --locked --workspace --all-targets` 通过（零警告）；`test -p peri-agent --lib` = 1066 passed；
`test -p peri-middlewares --lib` = 1808 passed；`test -p peri-acp-types --lib` = 576 passed、
`--doc` = 6 passed / 2 ignored；`test -p peri-middlewares --lib -- mcp::apps` = 25 passed；
`test -p peri-middlewares --test mcp_host_policy_contract` = 8 passed；`test -p peri-acp --lib` = 823 passed（与基线一致）。

## 阶段 1 + 2.1/2.2/2.2b 收尾验证（单一入口复跑）

一次串行批次内复跑（避免并发 cargo 干扰），全部通过：

- `check --locked --workspace --all-targets`：Finished，零错误零警告。
- `test --locked -p peri-acp-types --lib` = 576 passed。
- `test --locked -p peri-agent --lib` = 1066 passed。
- `test --locked -p peri-acp --lib` = 823 passed。
- `test --locked -p peri-middlewares --lib` = 1808 passed / 2 ignored。
- `test --locked -p peri-acp-types --doc` = 6 passed / 2 ignored。
- `test --locked -p peri-middlewares --test mcp_host_policy_contract` = 8 passed。
- `cargo fmt --all --check` 干净。

文件规模（STD-SIZE-001）：本轮曾把 `peri-acp/src/broker/transport_broker_test.rs`
推到 1002 行（超阈值），已按职责拆出 session-owned 门的两个用例到
`peri-acp/src/broker/transport_broker_session_gate_test.rs`（117 行，作为 `tests` 的子模块，
`#[path]` 引入），原文件回到 894 行；拆分后 `test -p peri-acp --lib -- broker::transport_broker`
= 26 passed。`check-file-size.sh` 现余 3 个超阈值文件（`peri-tui/src/kit/panels/mcp.rs` 1637、
`peri-tui/src/kit/acp_events/system.rs` 1007、`peri-resources/.../schema_test.rs` 1038），
经 `git show f01098c66:<path> | wc -l` 逐个核对，均为基线既有，非本轮引入。

## 阶段 2.3/2.4 + 阶段 3 核心（S1–S3）—— 已完成（本批）

设计文档：`.peri/plans/mcp-apps-phase2-idle-and-lease-design.md`（含 §11 root agent 裁决
R1–R9 与追加硬要求）。本批做 S1/S2/S3 + 跨 crate 编译修正；**未做** S4（session 交互
路由）与 S5（生产装配接线），见本文末尾「未做的部分」。

### S1 `peri-acp-types` 契约层

- `interaction.rs`：新增 `InteractionAttribution`（归属**事实**：`ModelTurn` /
  `AppAttachment` / `HostRequest`）+ `from_invocation_origin` + `owner()`（归属→
  `TrustedInteractionOwner` 的**唯一**映射）+ `kind()` / `session_id()`。
  未附着的 App 来源不派生归属（`None`）。
- `tool_invocation.rs`：`InvocationOrigin::App` 字段更正为
  `{app_id, attachment_id: Option<AttachmentId>, connection_epoch: Option<ConnectionEpoch>,
  server_generation}`（内部类型、不上 wire；附件身份与连接纪元各自 typed，与服务端世代
  分开表达）。
- `tools.rs`：`ToolContext.turn_binding` → `attribution: Option<InteractionAttribution>`
  + `with_attribution`（删除优于兼容：唯一消费者是归属铸造）。
- `mcp_apps.rs`：删除 `McpAppsRelayPort::begin_session_turn`；新增 `SessionInvocationPort`
  与 DTO（`SessionInvocationRequest` / `InvocationTarget` / `SessionInvocationOutcome` /
  `NotExecutedCause` / `TaskReceipt` / `SkippedPostHook` / `SessionInvocationOutcomeRecord`）；
  `McpAppsErrorKind` 新增 `ApprovalUnavailable`（审批不可用**不是**用户拒绝，必须用独立
  错误码表达）。
- 测试：`interaction_test.rs`（归属派生 / 未附着 App / owner 映射 / wire 形状）、
  `tool_invocation_test.rs`（App 字段与未附着形态）、`mcp_apps_test.rs`（目标身份分离、
  `ApprovalUnavailable` ≠ `UserRejected`、outcome 记录带跳过事实）。

### S2 `peri-agent`：管线 turn 可选 + idle 上下文

- `invocation_context.rs`：新增 `pub SessionInvocationSource` + `from_source`
  （`turn: None` 的**唯一**构造路径；`events: None` ⇒ 按构造零模型可见事件）；
  **删除 `from_stage_idle`**（要求 `StageContext`，而 idle 入口没有阶段上下文——误导 API）；
  新增 `attribution` / `attribution_for(origin)`（App/Host 从来源派生，模型/嵌套沿用环境
  归属）；`require_model_turn`（**模型来源**缺 turn ⇒ typed `NotAvailableWithoutTurn`，
  App/Host/嵌套返回 `Ok(None)`）；`PostHookSkipSink`（R2）与 `ApprovalUnavailableSink`
  （R9）两个有界记录点。
- `tool_dispatch/{execution,effective_dispatcher}.rs`：`collect_tool_results` /
  `run_before_tool_approvals` / `dispatch_concurrent` 接受 `Option<&ModelTurnFacts>`；
  无 turn 时**不发任何 `RenderEvent`**、不写 `turn_generation`、不写 `step`。
- `middleware_runner.rs`：`InvocationState.turn: Option<&ModelTurnFacts>`；
  `run_before_bound_tools_batch` 无 turn 不再对全部来源早退（App/Host 归属照常跑规则），
  仅**模型来源**逐条返回同一 typed 不可用；`run_after_tool` / `run_invocation_on_error`
  无 turn 时**显式记录跳过 + warn**（R2），不把已发生副作用改写成失败。
- `middleware/capabilities.rs`：`StateView::current_step() -> Option<usize>`（turn 路径
  仍 `Some`，idle 显式 `None`）+ `StateView::interaction_attribution()` +
  `BeforeToolState::approval_unavailable_sink()`；`MiddlewareState` 同步。
- **【2026-10-13 更正，root agent 复核发现】** ~~`session/factory.rs`：
  `invocation_rule_blueprint()` + `build_middleware_chain_with_blueprint`~~ —— **该宣称不成立**：
  全仓 grep 两个符号均为 0 命中（`peri-agent/src/session/factory.rs` 只有既有的
  `production_blueprint()`，:84）。R1 的「派生规则蓝图」**尚未落地**，属 S5 待办。
- `agent/stages/mod.rs` 导出 `InvocationContext` / `SessionInvocationSource` /
  `PostHookSkipSink` / `ApprovalUnavailableSink` / `invocation_dispatcher`（已核实存在）。
- `session_invocation.rs` 的归属记录点由 middleware 侧写入（R9）：`PermissionMiddleware`
  的单条/批量路径与 Hook 的 `ask` 路径在收到 `InteractionResponse::Unanswered` 时把
  typed 原因写进 `ApprovalUnavailableSink`，outcome 记录层据此区分
  `ApprovalUnavailable(cause)` 与 `UserRejected`（`AgentError` 层仍共享 `ToolRejected`，
  分类留待阶段 4，见移除条件）。

2.2b 回归测试改写（同一意图、不放宽断言）：
`invocation_context_test.rs::idle_source_has_no_turn_and_typed_rules_fail_closed`（取代
`execution_without_turn_is_explicitly_unavailable`：idle 来源无 turn、归属由来源派生、
`require_turn` 仍 typed 可诊断、App 来源不要求 turn、模型来源仍 typed 拒绝）；
`tool_dispatch/execution_test.rs::idle_entry_returns_typed_not_available_without_turn`
（改用 `from_source` + 模型来源）+ 新增
`idle_app_invocation_runs_without_turn_facts_or_model_ownership`（App 来源 idle 真实执行、
零事件端口、无 `turn_generation`、工具拿到 App 归属）；
`middleware_runner_test.rs::bound_approval_without_turn_returns_typed_unavailable_per_call`
（模型来源，断言逐字保留）+ 新增
`bound_approval_without_turn_runs_rules_for_attributed_app_entry`（App 归属 idle 不早退、
规则看到 App 归属与 `step = None`）。

### S3 `peri-middlewares`：record/registry 解环 + session-owned canonical 入口

- `mcp/apps.rs`：`McpAppBindingLease` → **`McpAppInvocationRecord`**（纯数据：`record_id`
  / `invocation_id` / server+世代 / resource_uri / instantiating_tool / invocation_token /
  `allowed_tools` / `state{Pending,Attached,Revoked}` / `attachment: AttachmentRoute`
  / `AppDeliveryToken` / `entitlement: EntitlementState::Unenforced`）。
  删除 `dispatcher` / `cancellation` / `turn_generation` / catalog / EventBus 字段。
  `McpAppBindingRegistry`：`issue` / `find` / `consume`（附着）/ `revoke_app` /
  `revoke_connection` / `revoke_session`，**不暴露 `cancel()`**；容量有界（1024，FIFO）。
  新增三层取消令牌类型（`AppDeliveryToken` / `AttachmentDeliveryToken`，各自无
  `From`/`into_inner`，不互相转换）。
- `mcp/session_invocation.rs`（新，`pub mod`）：`SessionInvocationService` 实现
  `SessionInvocationPort`；六步管线（归属校验 → 目标绑定 → 既有 before-tool 规则 →
  批准覆盖最终目标 → `SessionAdmissionOwner` 串行裁决 → 先写 outcome 再投影）；
  `AdmittedPeerTool`（`invoke_output` 内先 `commit.recheck()` 再调 bridge）+
  `InvocationOutcomeStore`（条数 256 有界，单条超 1 MiB 显式 `PayloadTooLarge`）+ 
  `InvocationCancel`（service 签发，父为 session 层）。
- `mcp/session_invocation/admission.rs`（新）：`SessionAdmissionOwner`（短锁无 await；
  epoch / 撤销 / record 状态 / 世代全过后产出 `DispatchCommit`）、`RevocationKey`
  （`App` / `Attachment` / `Session`，有界 4096 FIFO）、`DispatchCommit`（不实现
  `Clone`，携已绑定 peer）。
- `mcp/tool_bridge.rs`：实例化调用签发**纯数据 record**（不再 clone dispatcher / 取消
  令牌；也不再要求 turn generation）；新增 `client_handle()` 供提交复核绑定 peer。
- `mcp/apps_relay.rs`：`call_tool` / `invoke_app` 全部经注入的 `SessionInvocationPort`；
  `close_connection` 撤销该连接全部 Attachment（R7）且**不取消执行**；`close_session`
  撤销会话记录 + `session_closing()`；删除 relay 侧 `active_leases`（记录只在注册表）。
- `mcp/apps_invoke.rs`：恒 `PolicyDenied` **删除**，改为请求级前置校验 + 共同入口
  （Host invoke 与 App 回调同一条 canonical 路径）。
- 删除死 seam：`McpAppsInvoker` / `McpAppsInvocationSeam` / `McpAppsInvocationError`
  （R6 授权；仅测试引用的用例一并删除）。
- `mcp/mod.rs`：`pub mod session_invocation`；`admission` 保持 `pub(crate)`
  （唯一消费者在同 crate，见「判断」）。

新增测试（真实路径；桩只替代「用户决定」）：
`mcp/session_invocation_test.rs`（idle 回调真实执行 + 零模型卡片 + 事后钩子显式跳过、
无在线 endpoint ⇒ `NotExecuted{ApprovalUnavailable(NoOnlineEndpoint)}` 且零 wire、
批准改参与审批期间撤销 ⇒ 复核后零 wire、提交输给撤销 ⇒ 零 wire、关闭一个 App 不取消
同轮执行且其他 App 仍成功、执行结束不留 Arc 回环）、
`mcp/apps_relay_test.rs`（真实 peer 经 canonical 入口执行并逐字回传 raw、未装配入口
显式 fail-closed、连接关闭停止投递但会话不被取消）、
`mcp/apps_invoke_test.rs`（未装配入口时零 wire 拒绝）、
`mcp/apps_test.rs`（record 纯数据 + 单次消费 + 撤销终态 + 跨轮有效 + 连接级撤销隔离）。

### root agent 裁决（R1–R9）对应的落点

| 裁决 | 落点 |
|---|---|
| R1 规则蓝图为派生 + 同一装配器 | **【2026-10-13 更正】该落点未实现**：`peri-agent/src/session/factory.rs::invocation_rule_blueprint()` 与 `build_middleware_chain_with_blueprint` 全仓零命中（原表此处属不成立宣称，已核实）。要求仍有效：由 `production_blueprint()` 白名单派生 `{Hook, Permission}`，被排除槽位逐个注明「为何无法在无 turn 下运行」；目录准入复用 `McpAdmissionPreparation::open_typed_bridges`（service 内不重枚举 pool）；规则链以已装配 `MiddlewareChain` 注入 service（装配点=下一批 S5，见「判断 5」）。`ARC-MIDDLEWARE-001` 的「idle 规则链 ≠ 生产链」登记属 S6 文档收尾。**待办归属：S5。** |
| R2 事后钩子显式记录跳过 | `PostHookSkipSink`（有界去重）+ `InvocationContext::record_skipped_post_hook`（同时 `warn`）；`run_after_tool` / `run_invocation_on_error` 无 turn 时记录并返回 `Ok`（**不**把已发生副作用改写成失败）；跳过事实写入 `SessionInvocationOutcomeRecord.skipped_post_hooks`；回归 `session_invocation_test.rs::idle_callback_runs_without_turn_and_publishes_no_model_cards`（断言 `after_tool:not_available_without_turn`）。 |
| R3 删除 `invoke_session_updates` 合成模型卡片 | 本批的机制面已完成：idle 执行的 `InvocationContext.events: None` ⇒ 按构造零 `RenderEvent`；**调用点删除在 `host/mcp_apps.rs`，属 S5**（未做，见「未做的部分」）。 |
| R4 不消费 `SessionAuthorizationPort` + typed 字段 | `McpAppInvocationRecord.entitlement: EntitlementState::Unenforced`（typed 字段，不是注释）；移除条件记入本日志；未弱化连接绑定 / record 撤销校验。 |
| R5 过渡身份 + `InvocationOrigin::App` 更名 | `AttachmentRoute{attachment_id = record_id, connection_epoch = ConnectionEpoch::initial()}`（类型注释写明 per-View Attachment 属阶段 4）；`InvocationOrigin::App{app_id, attachment_id, connection_epoch, server_generation}`。 |
| R6 更名/删除授权 | `McpAppBindingLease` → `McpAppInvocationRecord`（字段重构）；删除 `McpAppsInvoker` / `McpAppsInvocationSeam` / `McpAppsInvocationError` 与只覆盖死 seam 的用例（`apps_test.rs::invocation_without_canonical_seam_is_unavailable`）。 |
| R7 连接关闭 = 该连接全部 Attachment detach | `McpAppBindingRegistry::revoke_connection` → `McpAppsRelayPort::close_connection` 撤销 record + 交付令牌，**不取消执行**；回归 `session_invocation_test.rs::closing_one_app_does_not_cancel_turn_or_other_app` 与 `apps_relay_test.rs::closed_connection_stops_delivery_without_cancelling_execution`。 |
| R8 串行点在 service；`prompt_in_flight` gate 本批保留 | `SessionAdmissionOwner`（`parking_lot::Mutex` 短锁、持锁无 await；`commit()` 产出携已绑定 peer 的 `DispatchCommit`）；`peri/mcp/invoke` 的 gate 未改动（行为不回退），移除条件=阶段 4 facade 由共同准入决定并发（本日志已记）。 |
| R9 outcome 记录层区分 ApprovalUnavailable / UserRejected | `ApprovalUnavailableSink`（本次执行的 typed 记录点）+ `BlockApprovalState::approval_unavailable_sink`；permission 单条/批量与 hook `ask` 三处消费 `Unanswered` 时写入原因；`map_dispatch_error` 优先产出 `NotExecuted{ApprovalUnavailable(cause)}`，否则按错误码产出 `UserRejected`；回归 `session_invocation_test.rs::no_online_endpoint_is_not_executed_and_never_reported_as_user_rejection`。`AgentError` 层分类留待阶段 4（移除条件已记）。 |

### 我做过的判断（超出设计文档）

1. **`SessionInvocationRequest` 增加 `invocation_id: Option<InvocationId>`**：Host 面
   回执需要稳定 `toolCallId`，而设计把执行身份分配权放在 service。该字段**只作回执
   关联**，不参与任何安全裁决（准入/撤销/世代判定全走服务端事实）。
2. **`NotExecutedCause` 增加 `UserRejected`**：设计枚举只有 5 个原因，但真实用户拒绝
   既不属 `ApprovalUnavailable`（那是「没有人可决定」）也不能被吞成未知；补一个 typed
   变体，App/Host 面据此返回 `PolicyDenied`。
3. **请求令牌不作执行取消源**：设计 §6 注释说 service 只把 `request.cancel` 当父，
   而 §2 又要求关闭 App/连接**不得**中断已 dispatch 的调用。落地为：`request.cancel`
   只用于**准入前**的放弃判定，执行期令牌 `InvocationCancel` 的父是 **session** 层。
4. **`mcp::admission` 保持 `pub(crate)`**（设计说放宽为 `pub`）：唯一消费者是同 crate
   的 service；放宽会新增无消费者的公开面（工程原则 2）。
5. **规则链以「已装配的 `MiddlewareChain`」注入 service**（而非在 service 内构造
   `AssemblyContext`）：production `AssemblyContext` 由 Agent 层 stage 装配逐轮投影，
   在 middlewares 侧伪造一份惰性上下文会引入假端口（如假 `TaskManager` / 假
   `AgentCatalogPort`）。派生蓝图 + 同一装配器的入口留给 S5 在真实 host 装配点调用
   （peri-agent 已提供 `invocation_rule_blueprint()`）。
6. **`ApprovalUnavailableSink` 作为 R9 的中间 typed 通道**：`AgentError` 层不可区分
   「无人可答」与「用户拒绝」，改由审批消费点在收到 `Unanswered` 时写入本次执行的
   记录点，outcome 记录层据此区分；不引入字符串匹配。

### 验证证据（原始输出）

单一入口串行复跑（本批收尾；避免并发 cargo 干扰）。**收尾复跑（2026-10-12 二次确认）**：
`check --locked --workspace --all-targets` = `Finished ... in 1.10s`（exit=0，零错误零警告）；
`test --locked -p peri-acp-types --lib` = `585 passed; 0 failed`、`--doc` = `6 passed; 0 failed; 2 ignored`；
`-p peri-agent --lib` = `1068 passed; 0 failed`；`-p peri-middlewares --lib` = `1816 passed; 0 failed; 2 ignored`；
`-p peri-middlewares --test mcp_host_policy_contract` = `8 passed; 0 failed`；`-p peri-acp --lib` = `823 passed; 0 failed`；
`-- mcp::session_invocation::tests` = `6 passed; 0 failed`、`-- mcp::session_invocation mcp::apps` = `33 passed; 0 failed`；
`-- idle_source_has_no_turn idle_entry_returns_typed idle_app_invocation bound_approval_without_turn
not_available_without_turn`（peri-agent）= `5 passed; 0 failed`；
`bash scripts/check-file-size.sh` = 超阈值 3 个（均为基线既有，逐文件 `git show f01098c66:<path> | wc -l`
核对与当前行数逐字相同：1637/1007/1038）；`fmt --all --check` = 干净。

首轮记录（同一批）：

- `check --locked --workspace --all-targets`：`Finished dev profile [unoptimized + debuginfo]
  target(s) in 1m 12s`，零错误零警告。
- `test --locked -p peri-acp-types --lib` = `585 passed; 0 failed`；
  `--doc` = `6 passed; 0 failed; 2 ignored`。
- `test --locked -p peri-agent --lib` = `1068 passed; 0 failed`。
- `test --locked -p peri-middlewares --lib` = `1816 passed; 0 failed; 2 ignored`。
- `test --locked -p peri-acp --lib` = `823 passed; 0 failed`（与基线一致）。
- `test --locked -p peri-middlewares --test mcp_host_policy_contract` = `8 passed; 0 failed`。
- `test --locked -p peri-middlewares --lib -- mcp::` = `741 passed; 0 failed; 2 ignored`；
  `-- mcp::session_invocation mcp::apps` = `33 passed; 0 failed`。
- `bash scripts/check-file-size.sh`：超阈值 3 个（`peri-tui/src/kit/panels/mcp.rs` 1637、
  `peri-tui/src/kit/acp_events/system.rs` 1007、`peri-resources/.../schema_test.rs` 1038）
  ——均为基线既有，本批新增文件全部 ≤1000 行。
- `bash scripts/check-layer-imports.sh`：`✅ 全边依赖门通过（Seam 3 完整版）`（22 条规则、
  违规边 0；未新增 `import-exemptions.conf` 条目）。
- `./scripts/cargo-rmcp-patched.sh fmt --all --check`：干净（`check_exit=0`）。

### 已知缺口与移除条件

1. **生产装配未接线（S5）**：`PoolMcpAppsRelay` 的 `session_invocation` 字段在
   `host/assemble.rs` 装配点仍为 `None`，App 回调与 Host invoke 走**显式 fail-closed**
   分支（`PolicyDenied`，零 wire 调用）。移除条件 = 下一批（S5）在装配点构造
   `SessionInvocationService` 并 `with_session_invocation(epoch, port)`。
2. **`SessionInteractionRouter` 未实现（S4）**：idle 审批 endpoint 路由、attachment
   epoch 权威（当前首次附着固定 `ConnectionEpoch::initial()`）、
   `NoOnlineEndpoint` 的生产者都留待 S4/S5；`ApprovalUnavailable(NoOnlineEndpoint)`
   目前由 broker 桩驱动验证。
3. **`TaskAccepted(TaskReceipt)` 无生产者**：MCP 任务回执仍以「成功但无 raw 载荷」呈现，
   service 记为 `OutcomeUnknown`（不伪造成功）。移除条件 = bridge 定义 typed 任务回执
   （承接既有的「relay 对成功但无 raw 报 UpstreamProtocolError」缺口）。
4. **`RemoteProtocolError(JsonRpcError)` 仅在 bridge 提供 `RemoteToolError.jsonrpc` 时产生**；
   当前 bridge 尚未在协议失败分支接线远端 JSON-RPC 载荷（既有缺口）。
5. **`entitlement: Unenforced`**（R4）：本阶段不消费 `SessionAuthorizationPort`；移除条件
   = SDK/部署身份层提供凭据的批次。
6. **`AgentError` 层仍未区分 `ApprovalUnavailable` 与 `UserRejected`**（R9 的移除条件）：
   区分只在 outcome 记录层；阶段 4 typed 分类批次收敛到 `AgentError`。

### 未做的部分（S4–S6）

- S4：`host/interaction_router.rs`（session-owned 交互路由、per-connection epoch、
  `NoOnlineEndpoint` 生产者）、`host/shutdown.rs` 的 `retire_connection`。
- S5：`host/assemble.rs` 构造 service + 注入 relay、`server_loop.rs` 的
  `peri/mcp/app`/`peri/mcp/invoke` 分支改走 port、`host/mcp_apps.rs` 删
  `invoke_session_updates`、`host/workspace.rs` 会话关闭调 `session_closing()`。
- S6：§4 删除项收尾、`ARC-MCP-APPS-001` 更新、`docs/standards` 与 code-index 收尾、
  生产链测试（A1 的 `Weak<EventBus>` / `Weak<catalog>` 探针在 S5 落地）。

## 独立审查（2026-10-13，只读 subagent，覆盖 S1–S3）

审查范围：`peri-acp-types` / `peri-agent` / `peri-middlewares` 的 S1–S3 改动（`peri-acp/src/host/**`
当次不在范围）。**已核实无问题**：取消分层（三类令牌互不转换、registry 无 `cancel()`、容量淘汰只
`revoke` 记录）、强引用环断开（`registry → record` 是终点）、admission 提交语义（短锁无 await、
epoch/撤销两次复核、撤销后零 wire 有测试）、turn-optional 不伪造（`from_source` 唯一入口且
`events: None`）、fail-closed 守恒、模型侧链路未回退、死代码删净、本批文件全部 ≤1000 行、
新增测试走真实 pool + rmcp duplex peer + wire 计数。

### 未修问题（按严重度，含修复归属）

1. **【高】`close_session` 被两个语义不同的调用点共用**：`apps_relay.rs:109-116` →
   `session_invocation.rs:712-720` 会取消 session 令牌（`InvocationCancel` 的父，
   `session_invocation.rs:120`）并撤销全量 record；调用点同时有 turn 级 `session/cancel`
   （`host/server_loop.rs:538`）与会话关闭（`:469`）。**接线后每次用户打断会撤销全部 App 并中断
   已 dispatch 调用**，违反设计 §2/§4 与 A2。修复归属：**S5**。
2. **【中】准入视图丢失会话关闭集**：`session_invocation.rs:466-474` 用 `Default::default()`
   （空 `closed_instances`）构造 `McpAdmissionPreparation`，而 `open_typed_bridges` 依赖该集合过滤
   （`mcp/admission.rs:264-269/196-200`）⇒ Host invoke 可解析到已关闭的 builtin 实例工具，
   绕过 MetaHarness 关闭策略。修复归属：**S5**。
3. **【中】`PermissionDenied → NotExecutedCause::TargetRevoked` 语义失真**
   （`session_invocation.rs:668-674`、`:235-241`）：策略拒绝/无归属在 outcome 记录里被记成
   「目标已撤销/换代」。修复归属：S5。
4. **【中】`DispatchCommit` 的「一次提交一次 wire 资格」未在类型上成立**：只做到不 `Clone`，
   `AdmittedPeerTool::invoke_output(&self)`（`:284-293`）可反复调用。修复归属：S5。
5. **【中】R3 未达成**：`host/mcp_apps.rs:86`（定义 `:101`）的 `invoke_session_updates` 仍在为
   Host invoke 发布合成模型卡片。修复归属：**S5**。
6. **【中】规则链类型缺口**：`SessionInvocationService.rules: Arc<MiddlewareChain>`
   （`session_invocation.rs:306/323`）无法区分会话窄链与 per-turn 链，注入生产链即形成
   session→turn 强引用。修复归属：S5（R1 的派生蓝图正是为此）。
7. 【低】空闲入口把 `messages` 填空表（`:573`）是对规则的伪事实；`ToolContext.turn_generation`
   成只写字段且注释过时（`peri-acp-types/src/tools.rs:623-624`）；`run_before_bound_tools_batch`
   的逐条分支生产不可达（`execution.rs:216` 先整体返回）；`cleanup` 注释与代码顺序不符
   （`apps.rs:241-256`）；`revoke_connection` 返回值被 `revoke_session` 忽略；`McpAppInvokeRequest::sequence()`
   是进程级静态计数器（阶段 4 会得到假去重身份）；`session_authorization.rs` 为无消费者公开面。

### 自述与事实不符（已在 S4/S5 章节要求更正）

1. `UserRejected` 被写成「超出设计文档的判断」，实为 R9 点名要求。
2. 「模型来源逐条返回 typed 不可用」在生产不可达（`execution.rs:216` 先整体 `?` 返回）。
3. `IdleInvocationState` 名称不存在，实际类型为 `InvocationState`。
4. R3 被记为「机制面已完成、调用点属 S5」，与「本批应删合成模型卡片」不符。
5. `close_session` 的调用点语义冲突未记录。

**审查局限**：无 stash 可比的中间态；`peri-acp/src/host/**` 未审，故第 1、5 条只作证据引用。

## 阶段 2.3/2.4 S4–S5：生产接线 —— 已完成（本批）

设计文档：`.peri/plans/mcp-apps-phase2-idle-and-lease-design.md` §5 的 S4/S5 + §11 裁决。
本批把新入口接到生产路径上，终点状态**功能可用**：App 回调与 Host invoke 都经
session-owned canonical 入口真实可达，`NoOnlineEndpoint` 有了唯一生产者。

### S4 `peri-acp`：session-owned 交互路由

- `host/interaction_router.rs`（新）：`SessionInteractionEndpoint`（连接 id + 纪元 +
  transport + 在线位）/ `SessionInteractionRouter`（借用 session 门，持**有界**（≤8）
  在线 endpoint 表、当前模型轮 endpoint、attachment 路由表）/ `AttachmentRoute` /
  `ModelTurnToken` + `ModelTurnGuard`（drop 即清除，只撤自己那次发布）。
  归属 → endpoint：`Model` 取当前轮发布（未注册 transport ⇒ 不发布）；`App` 取
  attachment 路由且**纪元必须相等**（旧纪元丢弃）；`Host` 取请求作用域注入的连接
  （`scope_host_request_connection` 任务作用域），无注入时按**唯一在线 endpoint** 投递，
  多 endpoint 时 fail-closed。命中后经既有 `AcpTransportBroker`（同一门、同一审批 /
  提问编解码）转发；**无在线 endpoint ⇒ `Unanswered{NoOnlineEndpoint}` 立即返回**
  （不降级为 auto-approve / disabled）。表满显式拒绝新连接（不淘汰在线 endpoint）。
- `host/mod.rs` 注册模块；`host/interaction_router_test.rs`（11 例）覆盖上面的选择规则。
- 不 import `peri_middlewares`：`scripts/import-exemptions.conf` 未新增条目。

### S5 `peri-acp`：装配与调用点接线

- `host/workspace.rs`（会话环境装配点）：构造 session 门 → `SessionInteractionRouter`
  → idle 准入规则链 → `SessionInvocationService` → `PoolMcpAppsRelay::new(pool, epoch, port)`；
  `SessionEnvironment` 暴露 `interaction_gate()` / `interaction_router()`。
- `host/workspace_interaction.rs`（新）：`build_idle_invocation_rules` 用**真实 session 级
  `AssemblyContext`** + 同一 `ProductionChainAssembler` 装配派生蓝本；`session_interaction_gate`
  取会话门。
- `host/assemble.rs`：删除 relay 构造（该层不知道 session id；relay 需要 session 级
  canonical 入口，因此与 service 一起落在会话环境装配点——见「我做过的判断」）。
- `host/prompt.rs`：本轮 broker = session router；prompt 装配点按本轮 transport 发布
  模型轮 endpoint（`ModelTurnGuard` 覆盖正常与提前返回），轮末清除；**没有 session 环境
  的退化会话**才回落 `build_transport_broker`（不再是生产语义）。
- `host/execution.rs`（调度触发）：同样优先 router（Host 归属按唯一在线 endpoint）。
- `host/server_loop.rs`：`spawn_prompt` / `spawn_mcp_apps_request` 注册连接 endpoint；
  `peri/mcp/open` 成功后 `bind_attachment`；`peri/mcp/invoke` 用请求作用域注入本连接；
  删除 invoke 结果的合成 `session/update` 投影；`session/close` 走会话环境的 relay。
- `host/mcp_apps.rs`：删除 `invoke_session_updates`（R3）；`InvokeHostResult` 只带 `value`。
- `host/shutdown.rs`：连接关闭时逐会话环境 `router.retire_connection` + relay
  `close_connection`（R7：该连接全部 attachment detach，**不取消**已 dispatch 调用）。
- `host/requests/{session_lifecycle,session_restore}.rs`：`interaction_gate` 取自会话环境
  （与 router 同一 `Arc`）。
- `peri-middlewares`：`PoolMcpAppsRelay::new` 改为**必填** canonical 入口（删除临时
  fail-closed 分支与 `with_session_invocation`）；`apps_invoke.rs` 的临时注释删除。

### 审查发现的处置（本批一并做）

| 项 | 处置 |
|---|---|
| 【高】`close_session` 同时被 turn 级 `session/cancel` 与真正的会话关闭使用 | `server_loop::dispatch_notification` 的 `session/cancel` 分支**删除** Apps 域动作（轮次级取消不触碰 record / 交付令牌 / 执行）；会话级拆除只在 `session/close` 分支。`session_closing` 只由会话面调用 |
| 【中】service 用空 `closed_instances` | `SessionInvocationInputs.closed_instances` 必填；装配点传 `frozen.meta_harness.disabled_middlewares` 的 `builtin_closed_instances` 投影；回归 `apps_invoke_test.rs::host_invoke_cannot_resolve_a_closed_builtin_instance`（TargetUnbound + 零 wire） |
| 【中】R3 未达成 | `invoke_session_updates` 删除；Host invoke 响应只带原请求值 |
| 【中】`rules` 无法区分窄链 / per-turn 链 | 新增 `InvocationRuleChain` 类型（peri-agent）+ `invocation_rule_blueprint()`（由 `production_blueprint()` 白名单派生，逐槽位写明排除原因）+ `build_invocation_rule_chain()`；service 只接受该类型 |
| 【中】`DispatchCommit` 可反复投递 | `AdmittedPeerTool` 改为**取走式**一次性提交（重复投递 ⇒ `NotExecuted{CommitConsumed}`，零 wire） |
| 【中】`PermissionDenied → TargetRevoked` 语义混淆 | 新增 `NotExecutedCause::PolicyDenied`；`AdmittedPeerTool` 增 typed 拒绝记录点，提交后复核失败的**真实原因**（TargetRevoked / TargetUnbound / CommitConsumed）不再经粗粒度 code 丢失 |
| 【低】`revoke_connection` 返回值被忽略 | relay `close_connection` 使用返回值（逐个 detach + debug 日志） |
| 【低】其余登记项 | 见「已知缺口与移除条件」（未在本批修） |

### 我做过的判断（超出设计文档）

1. **relay 与 service 的构造点从 `host/assemble.rs` 移到 `host/workspace.rs`**：设计 §5 S5
   写的是 assemble，但 `SessionInvocationService` 需要 session 身份（请求 / record 归属
   校验、准入视图过滤、会话关闭撤销），而 assemble 的输入里没有 session id（也没有
   frozen 关闭集）。放到会话环境装配点后，router / 规则链 / service / relay 在同一份池、
   同一份关闭集、同一份 provider 快照上构造，且 relay 仍然**必填**注入（无「未装配入口」
   运行时状态）。
2. **`Host` 归属的兜底规则**：只有 `peri/mcp/invoke` 有请求作用域连接；调度触发
   （cron/loop）没有连接身份。选择「**恰好一个在线 endpoint** 时按其唯一性投递，多端点
   时 fail-closed」而不是「无 endpoint 语义」——否则 TUI/stdio 的既有调度审批会从
   「弹窗」退化为「未获批准」（行为回退）。
3. **模型轮 endpoint 按 transport 身份发布**：prompt 各入口（用户 / 队列 / 续跑 / 调度）
   都持有 transport 但没有 connection id，而 ACP 服务实例内 transport ↔ 连接 1:1；
   按 `Arc::ptr_eq` 定位已注册连接，未注册即不发布（fail-closed，不猜连接）。
4. **规则链的消费面**：service 取 `InvocationRuleChain::shared()`（`Arc<MiddlewareChain>` 的
   只读借用），类型上仍与 per-turn `ChainAssembly.chain` 分离。
5. **测试夹具通道**：跨 crate 的真实 peer 夹具需要往池里装句柄（`clients` 是 `pub(crate)`）。
   选择给 peri-middlewares 加 `test-support` 特性（空池 + 装入/移除 peer + 隔离缓存），
   peri-acp 只在 dev-dependencies 启用：生产 API 面不变，`Cargo.lock` 仅 +1 行
   （peri-acp 的 dev 依赖）。`rmcp` 同时成为 peri-acp 的 dev-dependency。
6. **`SessionQueueLookup`**：idle 入口装配早于 `SessionManager` 登记会话，队列改为**按会话现取**
   （投影语义与 `build_stage_context` 一致），装配期不再固定一个 detached 队列。

### 验证证据（原始输出）

单一入口串行复跑（同一时刻只跑一个 cargo）：

- `check --locked --workspace --all-targets`：`Finished dev profile [unoptimized + debuginfo]
  target(s) in 24.07s`，零错误零警告。
- `test --locked -p peri-acp-types --lib` = `585 passed; 0 failed`；`--doc` =
  `6 passed; 0 failed; 2 ignored`。
- `test --locked -p peri-agent --lib` = `1068 passed; 0 failed`；`--doc` = `15 passed; 0 failed`。
- `test --locked -p peri-middlewares --lib` = `1815 passed; 0 failed; 2 ignored`；
  `mcp::apps_invoke mcp::apps_relay mcp::session_invocation` = `17 passed; 0 failed`。
- `test --locked -p peri-acp --lib` = `842 passed; 0 failed`；
  `-- host::interaction_router host::mcp_apps` = `28 passed; 0 failed`；
  `-- host::mcp_apps_idle` = `8 passed; 0 failed`。
- `test --locked -p peri-middlewares --test mcp_host_policy_contract` = `8 passed; 0 failed`；
  `--test canonical_tool_invocation_contract` = `5 passed; 0 failed`。
- `bash scripts/check-file-size.sh`：超阈值 3 个（均为基线既有：`peri-tui/src/kit/panels/mcp.rs`
  1637、`peri-tui/src/kit/acp_events/system.rs` 1007、`peri-resources/.../schema_test.rs` 1038）；
  本批新增文件 `interaction_router.rs` 497 / `interaction_router_test.rs` 457 /
  `mcp_apps_idle_test.rs` 734 / `workspace_interaction.rs` 157，均 ≤1000。
- `./scripts/cargo-rmcp-patched.sh fmt --all --check`：`fmt_exit=0`（干净）。
- `bash scripts/check-layer-imports.sh`：`共 22 条规则，违规边 0` / `✅ 全边依赖门通过（Seam 3 完整版）`
  （未新增豁免条目；`workspace_interaction.rs` 的装配器参数化使其无需 middlewares 全路径）。
- `Cargo.lock`：+1 行（peri-acp 的 `rmcp` dev 依赖）。

### 已知缺口与移除条件

1. **attachment 纪元权威仍在 registry 侧**：`McpAppBindingRegistry::consume` 仍硬编码
   `ConnectionEpoch::initial()`，router 侧同为首纪元，因此纪元检查今天是**防御性**的。
   移除条件 = 阶段 4 per-View 重附着把 epoch 权威交回 router（`bind_attachment` 的返回值
   随 record 一起持久）；S4 单测已直接驱动 epoch 差异覆盖该分支。
2. **`session/cancel` 的回归没有运行时测试**：本批在 `server_loop` 删除该分支的 Apps 域动作，
   但驱动 `dispatch_notification` 需要完整 `AcpServerConfig` 夹具（成本高）。当前覆盖：
   `session_invocation_test.rs::closing_one_app_does_not_cancel_turn_or_other_app`（中件层）
   + `mcp_apps_idle_test.rs::retiring_one_connection_does_not_cancel_dispatched_call_or_other_connection`
   （ACP 层：连接级 detach 不取消已 dispatch 调用）。移除条件 = 阶段 4/5 补 host 级通知路径回归。
3. **规则链的 per-turn 强引用探针**：类型已分离（`InvocationRuleChain`），但未加 `Weak` 探针
   断言「会话规则链不挂 per-turn 对象」；`invocation_rule_blueprint` 的槽位白名单 +
   `Agent` 层既有 A1/A13 探针是当前证据。移除条件 = 阶段 5 联合验收（A1 的
   `Weak<EventBus>` / `Weak<SessionToolCatalogSnapshot>` 探针属阶段 5 的
   `mcp_apps_idle_ownership_test.rs`，不在本批）。
4. **`TaskAccepted(TaskReceipt)` 仍无生产者**（既有缺口，本批未动）。
5. **`RemoteProtocolError(JsonRpcError)` 仅当 bridge 有 `RemoteToolError.jsonrpc` 时产生**（既有缺口）。
6. **`entitlement: Unenforced`**（R4）与 **`AgentError` 层的 typed 分类**（R9）移除条件不变。
7. **`McpAppInvokeRequest::sequence()` 仍是进程级静态计数器**（Host 入口序号）：阶段 4 的
   RequestLedger 会用错（登记项，本批未改）。
8. **`SessionInvocationSource.messages` 传空 `Vec`**（idle 入口无历史）：属「伪事实」登记项，
   收敛为 `Option` 需动 peri-agent 契约，随阶段 4 一并处理。
9. **`PermissionDenied` 之外的策略拒绝语义**：`EffectiveToolErrorCode` 无更细分类，非提交后
   复核失败的策略拒绝仍统一为 `PolicyDenied`（已比「记成 TargetRevoked」准确）。

### 未做的部分（阶段 4/5）

- 阶段 4：`apps/*` facade 七方法（AppRecord/Attachment 完整模型、per-View 重附着与 epoch
  权威回收、`apps/close` per-App 关闭）、RequestLedger 去重、结果 / 预算回收、
  `SessionAuthorizationPort` 接线、`peri/mcp/invoke` 的 `prompt_in_flight` gate 由共同准入
  取代（R8 的移除条件）、`AgentError` 层 typed 分类（R9）。
- 阶段 5：联合切换与验收（A1 `mcp_apps_idle_ownership_test.rs` 的 `Weak<EventBus>` /
  `Weak<catalog>` 探针、A13 无 Arc 回环探针、host 级通知路径回归、文档收尾
  `ARC-MCP-APPS-001` / `ARC-MIDDLEWARE-001` 的「idle 规则链 ≠ 生产链」登记）。

### implementation-log 事实更正（上批自述与代码不符处）

1. `UserRejected` 是 R9 点名要求（「outcome 记录层区分」），不是「超纲判断」。
2. idle 下「模型来源逐条返回 typed 不可用」在生产不可达：实际由 `execution.rs` 的
   `require_model_turn` 整体早退。
3. 类型名是 `InvocationState`（不是 `IdleInvocationState`）。
4. R3 记为「机制面已完成、调用点属 S5」与实际不符：上批 `invoke_session_updates` 仍在生产
   路径上（本批删除）。
5. `close_session` 的调用点语义冲突（turn 级 `session/cancel` 与真正的会话关闭共用）上批
   未记录（本批修正并在上表登记）。
6. 上批所列 `invocation_rule_blueprint()` / `build_middleware_chain_with_blueprint` 实际
   **未落地**（本批补齐，见 S5）。

## 独立审查（2026-10-13，只读 subagent，覆盖 S4/S5）与高 1 修复

审查核实上一轮 6 条发现（`session/cancel` 不触 Apps 域、准入视图带会话真实关闭集、
`PolicyDenied ≠ TargetRevoked`、`DispatchCommit` 取走式、R3 删除、规则链类型分离）
**均已真修**；另给 1 高 3 中 3 低。高风险项本批已修，其余按批次登记。

### 高 1（本批已修）：canonical idle 执行的取消父误用装配哨兵

**缺陷**（审查静态分析 + 我复核的上游源码）：装配点把 `SessionEnvironment::activation`
（池初始化哨兵，语义「会话已装配」，`activate()` 在**会话创建期**即取消，池初始化
`activation.cancelled().await` 等它）当作 service 的 `session_cancel` 与 idle 规则链的
`AssemblyContext.cancel`。`CancellationToken::child_token()` 对已取消父返回**立即取消**的
子节点（tokio-util `sync/cancellation_token/tree_node.rs::child_node`：已取消父直接返回
`is_cancelled: true` 的新节点），而审批 hook 走 `run_interruptible_hook` 的 `biased` select
（取消优先于 hook 结果）⇒ **审批永不发出**、outcome 落 `OutcomeUnknown`、relay 投影成上游
协议错误：生产装配下 App 回调与 Host invoke **全不可用**，且 `session_cancelled()` 恒真。

**修复**：`SessionEnvironment` 新增 `session_lifetime: CancellationToken`（装配期与哨兵
**同点创建**，但**不随 `activate()` 取消**），它同时作为规则链 `AssemblyContext.cancel` 与
service 的 `session_cancel`；`shutdown()` 首个动作取消它（`session/close` 与失败路径排空
都经此处）。服务侧 `session_closing()` 仍按自身语义撤销 record —— 取消树同一，重复取消幂等。

**验证证据（原始）**：

- 回归 `host::requests::tests::idle_invocation_lifetime::session_new_keeps_idle_invocation_cancel_parent_live_until_close`
  （`1 passed; 0 failed`）：走生产链路 `session/new` → `assemble_with_frozen` → `activate()`
  → `session/close`；断言面是 **service 实际持有的令牌**（`#[cfg(test)]` 观察面
  `SessionEnvironment::session_invocation_cancelled()`，只读环境自建字段看不到「注入错了哪根」）。
- **变异判别力（运行时证实缺陷真实存在）**：把装配点改回 `activation.clone()`
  ⇒ 同一用例 `FAILED`：`left: Some(true) / right: Some(false)`
  （即改动前 service 的取消父在**创建期就已取消**）。已复原。
- 负例 `host::mcp_apps_idle_tests::cancelled_session_parent_aborts_before_approval_without_wire`
  （`1 passed`）：取消父已取消而 record **未**撤销 ⇒ 审批帧 0、新增 wire 0、
  outcome 精确等于 `OutcomeUnknown`（不谎报 Completed / NotExecuted）。
- `test --locked -p peri-acp --lib` = `844 passed; 0 failed`（本批前 842，+2 新用例）。

### 未修项与归属（登记，不掩盖）

| # | 事实 | 归属 / 修复条件 |
| - | - | - |
| 中 2 | `UnansweredCause::NoOnlineEndpoint` 被挪用作多种事实的原因：纪元不匹配、归属会话不符、多端点歧义、连接未注册都统一产出该变体，而其 `reason_text()` 声明「本部署没有任何在线的审批 endpoint」——这些分支里在线 endpoint **存在**（非放行风险，是诚实性缺口） | 阶段 4a/4b：契约层新增「本端有在线 endpoint 但无一符合本次归属」的原因变体（`peri-acp-types` wire 枚举 + `McpAppsErrorKind` 映射），router 按分支分别产出；`interaction_router_test.rs` 里固化的用例同步改写 |
| 中 3 | Host 归属无请求作用域时投**唯一在线 endpoint**：已核实不会投错连接（端点表 session-owned；≥2 端点即 fail-closed）。残余①：`register_session_endpoint` 只在 `spawn_prompt` / `spawn_mcp_apps_request` 调用 ⇒ 会话创建后从未 prompt 的连接不注册，调度触发审批从「弹窗」退化为「未获批准」 | 阶段 4a 接线步骤：`session/new` / `session/load` / `session/restore` 也注册当前连接 endpoint（连接归属是会话，不是某一轮）；配一条 `session/load` 后调度审批可达的回归 |
| 中 4 | idle 规则链的 `task_manager` 是游离实例（`Arc::new(TaskManager::new())`），idle 期异步 hook 的 `spawn_owned` 挂在其上，取消令牌不随会话 drain | 本轮只做**注释更正**（不再声称「全部取自真实装配面」，缺口写入函数文档与本节）。阶段 4a：改为消费会话级管理器（turn 路径经 `SessionAccessPort::task_manager` 取会话级管理器再还原具体类型，先例见 subagent `factory/context.rs` 的 `Arc::downcast`）；downcast 失败按 no-op **显式记录**，不静默 |
| 低 5 | 生产装配零端到端覆盖：Apps 测试全部手工构造 service（正是高 1 的漏检原因） | 本批已补「真实装配 + 取消父」回归；阶段 4a 要求 facade 至少一条经 `session/new` 真实装配的端到端 |
| 低 6 | 每轮 broker 互斥门的语义变化未登记：门的归属从 per-broker 私有 mutex 改为 session 共享门（`transport_broker.rs`），跨轮/跨来源审批由可并行变串行 | **本条即登记**（属设计 §3.3 的预期行为，此前只删旧测试换新测试）；无进一步动作 |
| 低 7 | 文档漂移：`architecture-contracts.md` 的 `ARC-MCP-APPS-001` 仍写「connection-owned binding lease」「宿主缺 canonical 审批上下文返回 `PolicyDenied`」；「idle 规则链 ≠ 生产链」未登记 | 阶段 4b/5（已在「未做的部分」登记） |

### 事实更正（针对 S4/S5 自述）

1. 上批「终点状态功能可用：App 回调与 Host invoke 都经 session-owned canonical 入口真实可达」
   在**修复前不成立**——入口可达，但执行被自出生即取消的令牌拦住（高 1）。修复后该表述成立。
2. `workspace_interaction.rs` 的「全部取自真实装配面」不含 `task_manager`（中 4，已更正）。
3. 行数：上批记 `interaction_router.rs` / `mcp_apps_idle_test.rs` 为 497 / 734，实测
   460 / 730（现 460 / 781，含本批 +51 行新用例）。

## 批 C：S5 ledger + S6 facade 接线（2026-10-13，已完成）

### 交付物

1. **S5 `RequestLedger`**（`peri-acp/src/host/apps_ledger.rs` + `apps_ledger_test.rs`）：
   session 级请求账本；namespace 两类（`Attachment` / `Host`）；`last_accepted` 是推进权威
   （只有 `seq = last_accepted + 1` 才登记）；§8 六种情况逐条落成
   `Accepted{slot}` / `JoinedPending{slot}` / `Duplicate{outcome}` / `Conflict` / `ResultExpired` /
   `SequenceGap{expected}` / `CapacityRejected`，加 `UnknownNamespace`（后五项零登记零执行）；
   指纹 = sha2 over 规范化 JSON（facade 版本 + 会话 + 操作类别 + 目标身份 + 键排序参数；
   排除 transport ID / 等待时长 / connectionEpoch）；pending 有上限、complete 按条数 + 字节回收，
   超单条上限 ⇒ 结果不可用（`ResultExpired`）但**保留指纹**（防重事实）；
   `destroy` 后同 ID 不得重建（有界 FIFO tombstone，A16 同进程部分）；容量数字只引自 §4.3 常量。
2. **S6 `SessionAppsFacade`**（`apps_facade.rs` + `apps_facade_alias.rs` + 三个测试文件）：
   七个方法唯一实现；附件身份 / namespace / 纪元原样取自 router；序号与去重走 ledger；
   执行只走 `SessionInvocationPort`；记录事实只走 `McpAppsRelayPort`；错误分工按 D3 收敛
   （请求不合法 ⇒ `AcpError` 且 kind 可按键区分；§8 判决 ⇒ in-band `RequestVerdict`；
   执行事实 ⇒ `AppsRequestOutcome`）。
   接线：`host/mcp_apps.rs`（7 新 wire 名 + 旧 4 名薄别名）、`host/server_loop.rs`
   （方法表、按 `sessionId` 定位、`apps/rerun` 请求作用域连接、`session/new|load|resume` 注册端点）、
   `host/workspace.rs`（装配 facade：router + relay + canonical 入口 + 同源 epoch）、
   `host/mod.rs`（`AcpServerConfig.mcp_apps_facade`）、`host/shutdown.rs`（连接 retire 销毁 Host 域）、
   `dispatch/init.rs` + `host/requests/session_apps_capability.rs`（`_meta["peri.mcpApps"]`，只报已实现能力）。
3. **审查中 3①**：`session/new` / `session/load` / `session/resume` 也注册当前连接 endpoint
   （`session/restore` 不是本仓 wire 名：恢复流程即 `load` + `resume`，见 `requests/session_restore.rs`）；
   回归 `host/requests_session_endpoint_test.rs`：真实 `ServerLoop::run` 跑 `initialize → session/load`，
   随后用**调度触发归属**（Host，无请求作用域）发起审批 ⇒ 审批帧真实送达（`sessionId` 与会话一致）；
   反证用不经 server loop 的会话 ⇒ `Unanswered{NoOnlineEndpoint}`（修复前的退化形态）。
4. **低 5**：`requests_idle_invocation_lifetime_test.rs::session_new_assembled_facade_runs_app_callback_end_to_end`
   —— 经 `session/new`（真实 `assemble_with_frozen`）装配后，用**装配产物的池 / relay / facade** 走通
   「签发 record → `apps/list` → `apps/attach`（零 wire）→ 审批 → canonical 执行恰好一次」，
   审批帧送达该连接（permission mode 显式设为需要审批）。
5. **清理**：`interaction_router.rs` 三处过渡 `#[allow(dead_code)]` 已删（生产调用者 = facade）；
   `apps_relay.rs::host_sequence` **已删除**（`invoke_app` 增显式 `HostInvocationRef{namespace, seq}`，
   归属值来自 facade 的 Host 操作域 + ledger 序号）。保留一个定向 allow：
   `SessionAttachment::state()`（测试观察面）；`ConnectionContext::{insert_app_session, app_session}`
   标为批 D/S8 删除（§A 运行中裁决第 2 条：别名路径不再读写它们）。
6. **§A 运行中裁决落地**（旧 4 别名）：`open` 的 token→record 经 `list_apps` 的
   `model.invocation_id` 匹配（不新增索引），响应 `appSessionId` = router 分配的 `attachment_id`；
   `app` / `resource` 的 `appSessionId` 按附件身份经 `router.lookup` 定位（会话定位由
   `server_loop::locate_alias_session` 在「本连接为在线 endpoint 的会话」中反查）；
   `invoke` 沿用旧语义（客户端字段即目标 —— 遗留 wire 的既成事实）但归属值走 facade 的 Host 域 + ledger 序号；
   三者都不再由 relay 的 lease / `call_tool` / `read_resource` 路径执行。

### 证据（原始输出行）

- `check --locked --workspace --all-targets`：`Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 45.42s`（0 warning）
- `peri-acp-types`：`test result: ok. 600 passed; 0 failed; 0 ignored`
- `peri-middlewares`：`test result: ok. 1830 passed; 0 failed; 2 ignored`
- `peri-acp`：`test result: ok. 878 passed; 0 failed; 0 ignored`（批 B 基线 844 ⇒ +34）
- `peri-agent`：`test result: ok. 1068 passed; 0 failed; 0 ignored`（见批 C 进度文件的收尾节）
- `fmt --all --check`：FMT_EXIT=0
- `check-layer-imports.sh`：`§0 依赖门：共 22 条规则，违规边 0` + `✅ 全边依赖门通过（Seam 3 完整版）`
- `check-file-size.sh`：超阈值 3（与批 A/B 基线相同的三个既有文件；本批新增 / 改动文件全部 <1000）

### 已知边界与不掩埋的偏差（详见批 C 报告与进度文件）

1. `AppAttachSnapshot::reattached` 的**权威事实**在 facade（校验过的重挂接引用）；relay 侧按
   「本次调用前已有附件 **或** 附件身份此前已在记录里（`rebind`）」近似 —— detach 摘除交付集合条目，
   仅看集合会把「detach 后重挂接」误报成首附（已修 relay + facade 取或），但「记录曾有附件、当前无附件
   且非重挂接」仍可能报 false（登记为残余）。
2. 资源读取失败（`read_resources` / `list_resources` 的显式错误）在 `AppsRequestOutcome` 里没有对应
   「错误」形状：4a 按 `NotExecuted{TargetRevoked}`（最接近的「零副作用未执行」事实）投影，
   **不新增 `NotExecutedCause` 变体**（D7 的边界）。原生错误码经旧别名路径仍按键返回（`InvalidResource` / `PolicyDenied`）。
3. `apps/attach` 的容量 / 纪元 / 未知 namespace 拒绝只能走 `AcpError`（响应形状无判决字段），
   经 `RequestVerdict::error_kind()` 投影 —— 判决本身仍在 `RequestVerdict`（单一权威）。
4. `AppsRequestOutcome::Pending` 表示「已登记、尚未终结」：旧别名（同步语义）在
   有界等待超时后把 Pending 投影成 `UpstreamProtocolError`（不谎报结果，也不伪造成功）。

## 未完成的阶段（设计 §10 顺序）

> 状态更新（2026-10-13，S4/S5 落地后）：本节第 1 项已完成；第 2 项的「取消解耦 + canonical
> 入口 + 记录纯数据化」已完成，剩余部分并入第 3 项一起做。

1. **2.3/2.4 session 执行环境与 idle admission owner**——**已做**（S1–S5：idle canonical
   入口、`SessionAdmissionOwner`、`SessionInteractionRouter`、生产装配接线；证据见各批章节）。
2. **3 Apps runtime**——**部分已做**：记录纯数据化（`McpAppInvocationRecord` 无 dispatcher/
   cancellation/turn_generation）、五层取消令牌类型隔离、session-owned canonical 入口、
   `session/cancel` 不再触碰 Apps 域。**仍缺**：AppRecord 完整模型（AppId/incarnation/初始
   invocation/可信 generation/UI URI/实际输入/完整初始结果引用）、Attachment 完整模型
   （多 attachment per record、per-View 重附着、epoch 权威回归 router）、RequestLedger
   （§8 去重与序号规则）、预算与回收（§9）。
3. **4 facade 与 Host**（7 个 `apps/*` 方法 + 能力声明）——**未开始**。本仓无 Web Host 实现，
   §6.1 的 Web AppBridge 适配与验收 A15 属**外部依赖**（Fenix/Web Host 不在本仓，
   `side-projects/mcp-apps/` 仅为 ext-apps 示例场）。
4. **5 联合切换**（删旧权威 + 更新 `ARC-MCP-APPS-001`）——**未开始**（阶段 1/S1–S5 已删
   `begin_session_turn`、lease 的 dispatcher/cancellation/turn_generation、死 seam、raw 旁路、
   合成模型卡片、`mcp-app:` 前缀判定；ARC 文本与 A1–A16 验收属本阶段）。

## 独立代码审查（只读）发现的未修缺口

审查对照设计契约逐条核查阶段 1 / 2.1 / 2.2 的 diff，按严重度列出**仍未修复**的问题。
每条的修复归属与条件如下；这些是阶段 3 的核心目标，不是本轮引入的新债。

1. **【高】App lease 的取消源仍是整个 prompt attempt 的会话令牌**
   `tool_bridge.rs` 把 `ctx.cancellation.clone()` 存入 lease；该令牌来自
   `TurnContext::new(cwd, self.config.cancel_token.clone())`。relay 在
   `close_connection`/`close_session`/`begin_session_turn` 与容量淘汰时直接
   `lease.cancellation.cancel()`。**后果**：关闭/打开失败一个 App 会取消同轮 Act 的 select 与
   所有 hook；且每次新 prompt 先撤销上一轮全部 lease，与设计 §4「新 turn 不撤销 App」冲突。
   **修复条件**：lease 改持 InvocationId 作用域的**子令牌**或撤销标志位，把「关闭 App」与
   「取消执行」在类型上分开（阶段 3）。
2. **【高】强引用环仍在，且 lease 现经 dispatcher 挂住 `Arc<EventBus>` 与队列**
   `registry → lease.dispatcher → InvocationContext{events, queue} + pinned catalog`，而
   `bridge → Arc<McpAppBindingLeaseRegistry>` 回到起点。`v2_execute.rs` 的 forwarder 排空依赖
   「所有 root EventBus sender 已 drop」，而 lease 清理是访问触发 + TTL。**因此设计 §11 的
   A1（Host 永不 open 时 prompt 仍能终结）在代码上尚未满足，且无生产链测试。**
   **修复条件**：lease 不持 dispatcher/catalog/EventBus，只持 outcome 与窄端口（阶段 3）。
3. **【中】session-owned 互斥门把排队时间计入调用方超时预算**
   `broker_timeout`（默认 300s）包在 `broker.request(...)` 外侧，等待门的排队时间会吃掉响应
   预算，被排队的审批可能未送达就被判超时。**未确认可达性**（需同 session 并发两个交互源）。
   **修复条件**：拆分「排队上限」与「用户应答上限」，排队超限返回 `UnansweredCause`
   （与 `NoOnlineEndpoint` 生产者同批，阶段 2.4）。
4. **【中】`Unanswered`（含 `NoOnlineEndpoint`）在 typed 层与「用户拒绝」不可区分**——见上「已知缺口」。
5. **【中/破坏性 API】删除 `PermissionMiddleware::process_batch`**
   （`pub async fn process_batch(&[ToolCall])` → 私有 `process_batch_with_origins`，携带 origin
   与 owner）。已核实生产无调用方（仅测试引用）。**若下游需要批量入口**：新增带 origin+owner
   的显式入口，**不**恢复旧签名。
6. **【低】`SessionGrant::new` 无 sealing**：`grant()` 默认 fail-closed 已核实正确且全仓零消费者；
   但公开构造函数允许伪造授权。**修复条件**：构造收进授权端口（`new` 私有，唯一构造 `grant`），
   与 SDK 身份层同批。
7. **【低】relay 对「成功但无 raw」的结果报 `UpstreamProtocolError`**：MCP 任务回执分支返回
   `ToolOutput::from_legacy`（`outcome` 未报告、无 raw）。**修复条件**：阶段 3 定义 typed 结果
   承载后按 `Reported`/`Unreported` 产出 Completed 或明确的「不可承载」错误；在此之前保持
   fail-closed（不伪造成功）。
8. **【低】`InvocationId` 不强制 UUID v7**（提供 `new`/`From<String>`，非 UUID 值可入）。
   **修复条件**：分配点迁到 session 服务时收敛为 `generate()` + 校验型 `TryFrom<&str>`。

### 修复批次留下的已知缺口

- **无真实会话绑定的子 agent 路径现在无法发起审批/提问** ⇒ 显式拒绝。这是设计要求的
  fail-closed；这些路径的身份生产者属后续 session 交互路由批次。
- **`InvocationContext::from_stage_idle` 无生产调用方**（`#[allow(dead_code)]`），接线条件与
  移除条件写在原地：阶段 2.4 接线后删除该 allow。
- **MCP Agent 激活做不到「零 wire 调用」**：审批键由已激活内容导出，`resources/read` 必然先于
  归属检查；测试用「零 broker 交互 + 显式拒绝 + 路径证明」覆盖。
- **子 agent 的 `permission/mod.rs` 批量审批**已按「只拒绝待审批条目」修正，与正常路径同粒度。

## 恢复指引

1. 本 worktree 未提交任何 commit；`git status` 即全部改动。
2. 复跑验证：见本文件各段的证据行；全量入口是
   `./scripts/cargo-rmcp-patched.sh check --locked --workspace --all-targets` 与
   `test --locked -p <crate> --lib`。
3. 本批（S1–S3）已落地设计 §10 的阶段 2.3/2.4 与阶段 3 核心；下一批从 **S4**
   （`host/interaction_router.rs`）与 **S5**（生产装配接线）继续，验收目标：
   ① 生产 App 回调与 Host invoke 真正经 `SessionInvocationPort` 可达（当前 fail-closed）；
   ② `SessionInteractionRouter` 提供 `NoOnlineEndpoint` 生产者与 attachment epoch 权威。

## 批 D：S8 删旧权威 + 中 4 + 容量拆分 + 残余登记（2026-10-13，已完成）

### 交付物

1. **S8 删旧权威**（旧 4 个 wire 名按裁决 D1 **保留**为同一 façade 的薄别名）：
   - 删除 `ConnectionContext.app_sessions` / `insert_app_session` / `app_session`（含 `clone` / `clear`
     引用）：生产调用者为 0（批 C §A 裁决后别名路径已不再读写它，只剩 `connection_test.rs`），
     替代权威是 `SessionInteractionRouter` 的附件表（附件身份 + 纪元）。
   - 删除 `AppSessionBinding`（旧 lease 形状；唯一生产用途是 `validate_binding` 的入参拼装）。
   - 删除 `host/mcp_apps.rs::initial_binding`（`#[allow(dead_code)]`、0 调用者）及其用例。
   - `McpAppsRelayPort`：删 `open_app` / `validate_binding` / `read_resource` / `call_tool`
     （生产调用者 0，App 回调入口只剩 façade → `SessionInvocationPort`）；
     新增 `binding_protocol_version(app_id)` —— 按 **record 身份**复核（记录不存在 / 已撤销 ⇒
     `PolicyDenied`，世代不符 ⇒ `StaleServerGeneration`，无连接 ⇒ `ServerDisconnected`，成功返回
     协商出的 MCP 协议版本），调用点 = `apps_facade.rs` / `apps_facade_alias.rs` 的 record 投影。
     保留 `invoke_app`（旧 `peri/mcp/invoke` 的生产路径），并在 trait 文档写明删除条件
     （客户端切到 `apps/rerun` 后随旧 4 名一并删除）。
   - 连带删除因上述删除而 0 调用者的内部件：relay 的 `app_record` / `outcome_error_kind`
     （后者由 `SessionInvocationOutcome::error_kind()` 单一权威取代）、
     `AttachmentDeliveryToken::child()`。
   - 旧权威残留清理（D-1 第 5 条的自查结论）：`McpAppBindingRegistry::claim` 的唯一生产调用者是
     已删除的 `open_app`，且它把记录**从注册表摘除**（与 `attach_app` 的第二权威冲突）⇒ 删除；
     用例夹具改用 `find(record_id)`。`McpAppRecord::is_valid`（0 调用者）删除；
     `McpAppRecord::delivery()` 保留（生产 0，但 `apps_test.rs` 用它断言交付令牌未被撤销，
     删除需改动该既有断言且无收益——报告已登记）。
2. **中 4（idle 规则链消费会话级 task manager）**：`build_idle_invocation_rules` 增 `session_task_manager`
   入参；用 trait upcasting + `Arc::downcast` 还原 `peri_agent::agent::async_tasks::TaskManager`；
   downcast 失败 ⇒ `tracing::warn!`（含原因）+ `TaskManager::without_session_scope()`（新增构造器：
   作用域已关闭 ⇒ `spawn_owned` / `begin_external_execution` 被显式拒绝）的 **no-op**，
   **不**回落游离实例。`host/workspace.rs` 装配点传入同点创建的会话级管理器
   （与 `SessionEnvironment::task_manager` 同一实例）。
3. **容量拆分（D-3）**：`apps_relay_test.rs`(997) → `apps_relay_test.rs`(470，夹具) +
   `apps_relay_cases_test.rs`(635，用例)；`mcp_apps_idle_test.rs`(995) → `mcp_apps_idle_test.rs`(589，夹具) +
   `mcp_apps_idle_cases_test.rs`(447，用例)。用例名不变，夹具经 `use super::*` 共享（Rust 隐私：
   子模块可见祖先私有项，无需放宽可见性）。`workspace.rs` 因中 4 的调用点改动一度到 1001 行 ⇒
   压缩注释回到 999。
4. **D-4（`reattached` 第三情形）**：`McpAppRecord` 增 `ever_attached: AtomicBool`（有界、无权威的
   一次性记忆位，`attach` / `rebind_attachment` 成功时置位）；`attach_app` 判据改为
   「本次前集合非空 ∨ 本附件身份在册（rebind）∨ 曾附着过」。新增用例
   `new_view_first_attach_after_all_views_detached_is_reported_as_reattached`；真首附仍报 `false`
   （既有 `attach_returns_snapshot_without_calling_business_tool` 覆盖）。
5. **契约文档**：`docs/standards/architecture-contracts.md` 的 ARC-MCP-APPS-001 机制改为
   附件身份 + 纪元 + ledger 序号（保留 Rule 意图：canonical 入口、缺审批上下文 ⇒ `PolicyDenied`、
   零 MCP 调用、不回落 pool/peer 直调；撤销语义按 D6），`Verify` 改为现存真实路径并实测；
   `docs/code-index/{peri-acp,peri-acp-types,peri-middlewares}.md` 同步。

### 被测用例的迁移（每条的替代覆盖；不是「为过测试而放宽」）

- `peri-acp/src/host/connection_test.rs`：删 3 条旧 lease 断言（insert / lookup / close 清除）；
  能力门断言保留并**增强**（关闭中与关闭后 `apps_enabled()` 均为 false）。
- `peri-acp/src/host/mcp_apps_test.rs`：删 `initial_binding_uses_stable_apps_version`
  （随死代码删除；协议版本常量的单源断言在 `peri-acp-types/mcp_apps_facade_test.rs`）。
- `peri-middlewares/src/mcp/apps_relay_test.rs`：7 处 `relay.call_tool(...)` → 新的
  `RelayFixture::app_callback`（经**同一** canonical 入口 `SessionInvocationPort`，与 façade 同链）；
  错误 kind 断言保留（取自 `SessionInvocationOutcome::error_kind()`，与 façade 同一投影）；
  只有 `call_tool` 才有的「返回协商协议版本」断言迁到新用例
  `binding_protocol_version_reports_negotiated_version_and_rejects_stale_generation`（同一事实、同一来源）。
- `peri-acp/src/host/mcp_apps_idle_test.rs`（拆分 + 迁移）：`instantiate` 不再用 `open_app`
  （旧 lease 合成路径），改为生产同序（router 分配附件身份 / namespace / 纪元 → `relay.attach_app`）；
  `app_callback` 经 canonical 入口驱动；`ViewHandle` 取代 `AppSessionBinding`。
  原「JSON-RPC 信封 `jsonrpc == 2.0`」断言来自已删除的 relay 方法 ⇒ 改为更强的 typed 断言
  （raw 内容逐字相等），旧别名的 wire 形状改由**新增**的
  `apps_facade_cases_test.rs::legacy_aliases_round_trip_json_rpc_id_and_attachment_identity`
  在真实 ACP 分发面钉住（JSON-RPC id 往返、`appSessionId` = 附件身份、`resourceUri` 取自记录）。
- `peri-middlewares/src/mcp/apps_test.rs`：`claim` 夹具 → `find(record_id)`；
  `record_is_single_consume_and_pure_data` → `record_is_pure_data_and_attaches_by_identity`
  （删除 4 条「领取即消费 / 令牌不可复用」断言——该语义随 `claim` 删除，新权威按附件身份附着且
  可多 View；保留并新增「同一 Arc 返回」「记录不被摘除」断言）。
- `peri-middlewares/src/mcp/session_invocation_test.rs`：夹具 `claim` → `find(record_id)`。
- `peri-acp/src/host/requests_idle_invocation_lifetime_test.rs`：抽出 `install_assembly_peer` 夹具
  （机械提取）+ 新增中 4 回归。

### 证据（原始输出行，2026-10-13）

- `check --locked --workspace --all-targets`：`Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 2m 56s`
  （CHECK_EXIT=0，`warning` 计数 0）
- `test --locked -p peri-acp-types -p peri-middlewares -p peri-acp -p peri-agent --lib`（TESTS_EXIT=0）：
  · `test result: ok. 600 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.02s`（peri-acp-types）
  · `test result: ok. 1832 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 132.42s`（peri-middlewares；批 C 1830 ⇒ +2）
  · `test result: ok. 879 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 159.77s`（peri-acp；批 C 878 ⇒ +1）
  · `test result: ok. 1069 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 7.78s`（peri-agent；批 C 1068 ⇒ +1）
  · 本轮**未复现**已登记的并发 flake（`async_hook_without_output_completes_without_warn_noise ... ok`）
- `fmt --all --check`：`FMT_EXIT=0`
- `bash scripts/check-layer-imports.sh`：`§0 依赖门：共 22 条规则，违规边 0` + `✅ 全边依赖门通过（Seam 3 完整版）`（LAYER_EXIT=0，未新增豁免）
- `bash scripts/check-file-size.sh`：`共扫描 2269 个文件，超阈值 3 个（源码 2 / 测试 1）`（SIZE_EXIT=1，
  与批 A/B/C 基线**完全相同的三个既有文件**：`peri-tui/src/kit/panels/mcp.rs` 1637、
  `peri-tui/src/kit/acp_events/system.rs` 1007、`peri-resources/src/sessions/sqlite_store/schema_test.rs` 1038；
  批 D 新增 / 改动文件全部 ≤1000）
- ARC-MCP-APPS-001 的 Verify（实测，逐条）：
  · `test -p peri-middlewares --lib -- mcp::apps`：`test result: ok. 41 passed; 0 failed; 0 ignored`（0.18s）
  · `test -p peri-middlewares --lib -- mcp::session_invocation`：`test result: ok. 8 passed; 0 failed; 0 ignored`（0.04s）
  · `test -p peri-acp --lib -- host::apps_facade`：`test result: ok. 14 passed; 0 failed; 0 ignored`（0.44s）
  · `test -p peri-acp --lib -- host::mcp_apps`：`test result: ok. 20 passed; 0 failed; 0 ignored`（0.02s）

### 已知边界与不掩埋的偏差

1. **中 4 回归的覆盖边界**：端到端「idle 期异步 hook 真的被 spawn 并挂在会话级管理器上」在测试环境
   不可构造 —— `session/new` 的会话装配会**重建** `AcpServerConfig`（`workspace.rs:398`），因此
   测试无法向 idle 链注入 `hook_groups`；而经 settings / plugin 注入 hook 需要 trust store
   （`peri_config::trust`，测试环境无）。回归改为：真实装配输入（host 装配产物的 cfg + 真实
   `SessionManager::new_session_task_manager()`）+ **生产装配函数** `build_idle_invocation_rules`
   + 探针装配器捕获真实 `AssemblyContext.task_manager`，断言 **`Arc::ptr_eq` 会话级同一实例**，
   并断言 downcast 失败分支交出的是**已关闭作用域**（`spawn_owned` 被拒 = no-op）。
   已做**反向验证（mutation）**：把装配点改回 `Arc::new(TaskManager::new())` ⇒ 断言失败
   （`idle 规则链的 task manager 必须是**会话级**同一实例…`）。hook 触发面本身未在本批验证。
2. `McpAppRecord::delivery()` 保留（生产调用者 0；`apps_test.rs` 用它断言「关闭连接 A 不撤销 B 的
   交付资格」，删除需改该断言且无收益）。
3. 旧别名（`peri/mcp/{open,resource,app,invoke}`）与其 DTO 按裁决 D1 保留；删除条件写进 trait 文档
   与 ARC-MCP-APPS-001（外部客户端切到 `peri/mcp/apps/*`）。
4. `apps_facade.rs` 991 行：仍在 1000 以内但已接近上限，后续扩方法需先拆分（登记）。
