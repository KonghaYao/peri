# Peri Swarm `requires_action` 实现验收（2026-11-21）

- 设计契约：`spec/issues/2026-11-21-peri-swarm-requires-action-design.md`（修订 4，已批准）。
- 实现基线：`deca51f44`（分支 `feat/swarm-requires-action`，隔离 worktree）。
- 复验返修基线：`ba52f5c4d`；§1.6、§3 与 §4 的返修结论针对五类复验问题，
  §2 的计数是返修后的最终状态。
- 第二轮独立复验返修基线：`e7519a281`（2026-11-22）；本文件 §1.7、§2、§3 与
  §4 已按该轮 B-1/H-1/M-1/M-2/M-3/L-1..L-4 的修复更新。
- 观察基线（设计时）：`22e44e0b9`。
- 范围：官方 `agent.session.requires_action` 的 MCP 审批映射、答复回路、单次结算、
  状态投影、SSE 顺序、失效语义、历史回放契约与 `OfficialSession.environment` 快照。

## 1. 实现结构

### 1.1 触发范围（fail-closed 判定点）

- 触发只来自网关扩展 `agent.tools[].require_approval`。官方 SDK 没有该字段，
  纯官方入参无法开启审批；这一点没有改变。
- 会话级 `PermissionMode` 仍是 `Bypass`。新增的是**范围**：
  `peri_acp_types::permission::SharedPermissionMode` 携带
  `mcp_approval_servers`（声明 server 名集合），`peri-middlewares` 的
  `decide_by_mode(Bypass)` 只在“外部 MCP 且绑定 server 命中集合”时升级为审批。
  集合为空时 `Bypass` 行为与之前完全一致。
- 匹配基于绑定来源：`ApprovalItem.mcp_origin = {server, tool}`（来自
  `BoundToolOrigin.mcp_server_name/mcp_tool_name`），`session/request_permission`
  的 params `_meta.peri.mcp` 携带同一事实。同名工具在另一个未声明的 server 上
  不进入审批范围（`escalation_servers` 只含声明 server）。
- **fail-closed 判定点**（按顺序）：
  1. 无法归属来源（`_meta.peri.mcp` 缺失、绑定目标缺失或损坏）→ 仍按外部 MCP
     识别（有效名 `mcp__{server}__{tool}`，或 `ToolInvoke` 目标以 `mcp__` 开头）
     并进入审批，不默认放行；无法识别的普通内置调用仍按原模式放行；
  2. 来源可确定但声明对象形式没有为该工具给出 `always`/`never`/`default` →
     进入待办等待；
  3. 命中声明的 **MCP 审批范围但本端没有审批通道** → `ToolRejected`
     (`PolicyDenied`)，不静默放行（`peri-middlewares`）；
  4. 请求不属于任何打开的 `session/prompt` → 立即以 ACP `cancelled` 明确拒绝，
     不留下永久待办。
- 声明 `never` 的工具由网关在收到请求时立刻回 `allow_once`（声明事实，不是匹配
  失败）。声明 server 之外的绑定不会被 host 升级，因此审批范围没有扩大。
- 子 agent 委派：子链不继承全量 HITL，但父会话的共享模式（含审批范围）与 broker
  经 `SubagentChainContext` 进入子链；范围非空时子链装配 `PermissionMiddleware`，
  命中范围的外部 MCP 调用走同一审批。空范围不装配，子链行为与基线一致；范围存在
  但 broker 缺失时子链显式 `PolicyDenied`。

### 1.2 待办身份与代次

- 公开 id：`appr_<uuidv7>`（不透明）。官方 `request_id` 与 ACP JSON-RPC id 无关。
- 公开 action 采用官方嵌套形状
  `{type:"mcp_approval_request",request_id,turn_id,request:{type:"mcp_tool_call",
  server_label,name,arguments,message}}`；`turn_id` 来自打开该待办的 turn，
  `name`/`arguments` 缺失分别为 `null`，`message` 只描述待审批动作。
- 待办记录（仅内存，随 actor 代次存活）：公开 action、原始 ACP `RequestId`
  （`Value` 保存，答复时原样回填）、`turnRequestId`（挂起 `session/prompt` 的
  request id = 执行代次）、`itemSeq`、`deadline`。
- 只结算“当前有效”的待办：未结算、未超过
  `peri_acp_types::interaction::APPROVAL_WAIT`（与 middleware `BROKER_TIMEOUT`
  同源，300s）、`turnRequestId` 等于当前 `LogState.active`。
  `ItemLog::required_actions` 是唯一判定点，`sessions.retrieve`、SSE 快照、
  `sessions.list` 与回放共用。
- 结算记录保留 256 条（UUIDv7 键按时间排序，超出丢弃最旧），用于同幂等键重放。

### 1.3 持久事件契约与游标

- 不新增表/列；审批事实写成 `swarm_items` 行（`kind=approval`），沿用现有 item
  `seq` 游标与 `Last-Event-ID`：
  - pending：`{type:"mcp_approval_request",state:"pending",requestId,turnId,
    turnRequestId,serverLabel,name,toolCallId,arguments}`；
  - settled：`{type:"mcp_approval_request",state:"settled",requestId,decision,
    reason,inputId}`，`reason ∈ answered|expired|turn_ended|actor_lost`。
- SSE `agent.session.requires_action` 的 `event_id = evt_item_<seq>_requires_action`，
  完全由持久行 `seq` 派生（无时钟、无随机）。pending 行发当前集合；settled 行再发
  同类型快照，空集合表示“已无待办”，SDK 的 dedupe 因此不会丢掉状态变化。
- 回放按行折叠：pending 加入、settled 移除；已结算动作不会复活。订阅建立后追加
  一帧当前快照（与实时同一 `event_id`，按 id 幂等），覆盖 opener 在游标之前的重连。
  ACP `subscribe` 与生产 actor `GET /events` 共用同一 backlog 装配：先结算过期
  待办，再读游标之后的回放窗口，最后追加当前快照。
- 官方答复 body：`{type:"agent.session.input.mcp_approval_request_result",
  request_id,response:{type:"mcp_tool_call",decision}}`。`response.type` 必须是
  `mcp_tool_call`；顶层 `decision`/`requestId` 只是原生别名，纯官方形状不会 400。
- 两阶段送达：先认领待办并送 ACP，成功后才写 `settled` 并返回成功回执；送达失败
  释放认领、待办仍可答复并返回 502。并发答复只有一个认领成功，其余得到
  `required_action_settled(reason="delivering")`。settled 行先落库再删除内存待办：
  落库失败返回 `503 required_action_settlement_failed` 并保留待答复事实；不会出现
  “回执成功但未持久化”或“回执失败但已删除”。
- 硬崩溃恢复：actor `on_start` 扫描 `state=pending` 的 approval 行，追加结算行
  （turn 仍打开为 `actor_lost`，turn 已结束为 `turn_ended`），再参与回放/投影。
- 官方 item 投影不变：只有 root `ItemKind::Message` 进入 `turn.item.*`；
  `kind=approval` 只是自定义 item，不会伪造官方 item。
- `StreamKey=(agent_id,message_id,kind)`、`phase`/`outputIndex` 只对 root Message、
  旧 payload 缺键默认值、失败 turn 的 `turn.failed → error → session.failed`
  顺序均未改动。

### 1.4 环境快照

- `OfficialSession.environment` 新增并随全部快照投影（`session.created`、
  `in_progress`、`requires_action`、`idle`、`failed`、回放与 `from_session_view`）。
  数据来自会话行的 `environment`：hosted 是 `{type:"openai_hosted",id}`，
  `none` 归一化为 `{type:"none"}` 且不带 id；未知保持 `none`，不造 id。

### 1.5 错误码（答复回路）

| 情形 | HTTP | code | details |
| --- | --- | --- | --- |
| 未知/跨 session/旧 actor 代次 | 404 | `unknown_required_action` | `requestId` |
| 已结算、并发失败方、过期、turn 结束、actor 丢失 | 409 | `required_action_settled` | `requestId`,`reason`（并发送达中为 `delivering`） |
| 同 `Idempotency-Key` 重放 | 202 | —（返回同一回执，`disposition=duplicate`） | — |
| 决策无法送达 host | 502 | `upstream_unavailable` | `requestId`；待办仍可答复 |
| 结算行无法持久化 | 503 | `required_action_settlement_failed` | `requestId`,`operation`；待办保留、认领释放 |
| 非法 decision / 缺 `request_id` / `response.type` 不符 | 400 | `invalid_input` | `field` |

答复输入：`POST /v1/agents/sessions/{id}/events`
`{"events":[{"type":"agent.session.input.mcp_approval_request_result","request_id":"appr_…","response":{"type":"mcp_tool_call","decision":"approve|deny|cancel"}}]}`
（`request_id` 也接受 `requestId`/`approval_request_id`；顶层 `decision` 作为原生
扩展保留）。它不创建/排队新 turn，只把 decision 写回原 ACP JSON-RPC id。

### 1.6 复验五类问题的返修位置与回归测试

| # | 问题 | 代码位置 | 返修 | 回归测试 |
| --- | --- | --- | --- | --- |
| 1 | `required_actions` 非官方嵌套形状 | `peri-swarm/src/domain/approval.rs`（`RequiredAction::to_json` / `required_action_json`）、`peri-swarm/src/actor/reader.rs:999`、`peri-swarm/src/service/mod.rs`（`fold_approval`） | 改为 `{type,request_id,turn_id,request:{type,server_label,name,arguments,message}}`；`request_id`/`turn_id` 由待办持久事实派生，缺失 `name`/`arguments` 为 `null`；实时与回放共用同一投影函数 | `a_pending_action_matches_the_official_nested_shape_and_keeps_null_fields`、`declared_always_opens_a_pending_approval_and_broadcasts_requires_action`、`a_requires_action_snapshot_is_an_official_session_event`、`replay_folds_approval_rows_and_never_revives_a_settled_action` |
| 2 | 官方 answer body 走 400 | `peri-swarm/src/http/request.rs`（`OfficialApprovalResultBody`、`approval_shape_error`、`approval_decision`） | 读取 `response.decision` 并校验 `response.type == "mcp_tool_call"`；保留顶层别名；错误 details 指向 `events[0].response.type` / `events[0].response.decision` | `an_approval_result_event_resolves_to_a_decision`、`the_native_top_level_decision_alias_still_resolves`、`an_approval_response_type_must_be_mcp_tool_call`、`an_approval_result_requires_the_official_response_object` |
| 3 | 来源缺失时 middleware 静默放行 | `peri-middlewares/src/permission/permission_middleware.rs`（`scoped_mcp_approval`） | 范围非空且无可用绑定来源时，`mcp__*` 有效名或 `ToolInvoke` 指向 MCP 目标一律按需审批；无 broker 时 `PolicyDenied`；空范围不变 | `a_scoped_mcp_call_without_a_binding_enters_approval_instead_of_bypassing`、`a_scoped_mcp_call_without_a_broker_is_policy_denied`、`a_scoped_tool_invoke_targeting_mcp_enters_approval_without_a_binding`、`an_empty_scope_keeps_bypass_unchanged` |
| 4 | 子 agent 委派绕过审批 | `peri-middlewares/src/subagent/*`、`peri-agent/src/session/subagent/types.rs`、`peri-agent/src/session/subagent/factory/context.rs` | 父会话共享模式与 broker 经 `SubagentChainContext` 传入子链；范围非空时装配 `PermissionMiddleware`；空范围不装配；无 broker 时 `PolicyDenied` | `a_subagent_inherits_the_parent_scope_and_asks_for_approval`、`an_empty_parent_scope_keeps_the_subagent_baseline`、`the_bound_subagent_tool_forwards_its_scope_to_the_child_chain` |
| 5 | 硬崩溃 pending 行悬空；502 与待办不一致 | `peri-swarm/src/service/mod.rs`（`settle_orphan_approvals`）、`peri-swarm/src/actor/swarm.rs`（`on_start`）、`peri-swarm/src/actor/reader.rs`（两阶段认领/结算/释放） | 恢复时按 turn 是否打开结算为 `actor_lost`/`turn_ended`；送达成功后才结算，失败释放认领并保留待办；并发送达中返回明确冲突 | `recovery_settles_orphan_approvals_and_replay_stops_projecting_them`、`recovery_marks_orphan_approvals_actor_lost_while_the_turn_is_open`、`a_delivery_failure_release_keeps_the_action_answerable` |

### 1.7 第二轮独立复验返修（2026-11-22）

| # | 问题 | 代码位置 | 返修 | 回归测试 |
| --- | --- | --- | --- | --- |
| B-1 | `peri-acp` 测试目标因 `ApprovalItem.mcp_origin` 缺字段而无法编译 | `peri-acp/src/broker/transport_broker_test.rs`、`peri-acp/src/host/interaction_router_test.rs`、`peri-acp/src/host/mcp_apps_idle_cases_test.rs`、`peri-acp/src/host/requests_session_endpoint_test.rs`、`peri-acp/tests/integration_test.rs` | 全部构造点补 `mcp_origin: None`（非 MCP 通道，语义正确） | `cargo check -p peri-acp --all-targets --offline`、`./scripts/cargo-rmcp-patched.sh test --locked --offline -p peri-acp --lib --no-run` |
| H-1 | 生产 Rivet SSE 路径没有“订阅后当前快照”，也不结算过期待办 | `peri-swarm/src/actor/service.rs`（`subscribe_backlog`）、`peri-swarm/src/actor/swarm.rs`（`events_response`） | 抽出唯一 backlog 装配：先 `settle_expired_approvals`，再 `replay_after`，最后追加 `approval_snapshot_event`；ACP `subscribe` 与生产 `GET /events` 共用 | `a_reconnect_after_the_pending_row_still_receives_the_current_snapshot`、`an_expired_pending_approval_is_settled_before_the_replay_window` |
| M-1 | 结算行持久化失败仍返回 202，回执与耐久事实不一致 | `peri-swarm/src/actor/reader.rs`（`claim_approval`/`complete_approval_claim`/`settle_*`/`persist_approval_settlement`）、`peri-swarm/src/domain/error.rs` | settled 行先落库、再删内存待办；失败返回 `503 required_action_settlement_failed`、释放认领、保留待办；新增 `ApprovalClaim::Failed`/`ApprovalCompletion::Failed`/`ApprovalReply::SettlementFailed` | `a_settlement_persistence_failure_is_a_5xx_and_keeps_the_action_answerable`、`an_expiry_settlement_persistence_failure_fails_the_read_and_is_retryable` |
| M-2 | 502 保留待办只有注册表级测试 | `peri-swarm/src/actor/service_test.rs` | 用内存 ACP transport 在 service 层注入“传输已关闭”；断言 502、待办仍可答复、无 settled 行；开放传输用例覆盖成功结算 | `a_failed_acp_delivery_is_502_and_the_action_stays_answerable`、`a_delivered_approval_settles_and_leaves_no_pending_action` |
| M-3 | 子 agent “有范围、无 broker”装配分支无专门测试 | `peri-middlewares/src/subagent/tool/tool_test/subagent_approval_scope_test.rs` | 覆盖真实 `SubagentChainAssemblerImpl::assemble` 的 scope-only 分支：范围内 `PolicyDenied`，范围外仍放行 | `a_scoped_subagent_without_a_broker_fails_closed` |
| L-1 | 来源不可归属时 `server_label: ""` 与官方禁 null 的语义未写明 | `peri-swarm/src/domain/approval.rs`、`docs/design/peri-swarm.md` §4.2 | 文档化“空串 = 来源未知”，`message` 明示 could not be determined | `an_unattributable_permission_projects_an_empty_server_and_a_null_name` |
| L-2 | 空工具名可能投影成 `name: ""` | `peri-swarm/src/domain/approval.rs`（`required_action_json`）、`peri-swarm/src/actor/reader.rs`（`begin_permission`） | 空 `server`/`tool` 一律按缺失处理：`server_label: ""`、`name: null` | 同上 |
| L-3 | 混合 batch 与未声明 server 同名工具缺直接测试 | `peri-middlewares/src/permission/source_identity_test.rs` | 同批三调用只有声明 server 进审批；同名工具绑定未声明 server 不触发 broker | `a_mixed_batch_only_escalates_the_calls_inside_the_declared_range`、`an_undeclared_server_does_not_trigger_the_broker_for_the_same_tool_name` |
| L-4 | `SettlementReason::TransportClosed` 定义但从不产生 | `peri-swarm/src/domain/approval.rs`、`peri-swarm/src/domain/error.rs`、本文件 §1.3 | 删除该变体；ACP 传输关闭走 502 送达失败，不作为持久结算原因 | 编译期穷尽匹配 + `a_failed_acp_delivery_is_502_and_the_action_stays_answerable` |

## 2. 命令与实际结果

环境：`CARGO_HOME=/tmp/cargo-home-rsproxy`，`./scripts/cargo-rmcp-patched.sh`（离线、
锁定）。构建缓存从主 worktree 复用，`target/` 未入库。

```
$ ./scripts/cargo-rmcp-patched.sh test --locked --offline -p peri-swarm
lib:                   290 passed, 0 failed
main:                  0 passed
actor_runtime_test:    16 passed, 1 ignored
http_contract_test:    25 passed
items_pagination_test: 13 passed
loom_store_test:       0 passed, 3 ignored
mcp_injection_test:    4 passed
openai_hosted_test:    7 passed
rivet_actor_test:      0 passed, 2 ignored
session_resources_test:17 passed
streaming_test:        15 passed
doc-tests:             0 passed
合计：387 passed, 0 failed, 6 ignored（ignored 为既有 Loom/Rivet 外部依赖用例）

定向组：
$ ... test -p peri-swarm --lib -- approval                 29 passed
$ ... test -p peri-swarm --lib -- recovery                  2 passed
$ ... test -p peri-swarm --lib -- replay                   22 passed
$ ... test -p peri-swarm --lib -- actor::service::service_test  6 passed
$ ... test -p peri-middlewares --lib -- permission::        87 passed
$ ... test -p peri-middlewares --lib -- subagent_approval_scope  4 passed

$ ./scripts/cargo-rmcp-patched.sh test --locked --offline -p peri-acp --lib
1501 passed, 1 failed, 2 ignored
已知负载 flake：host::mcp_v4_wire_fixture::wire_fixture_script_logs_wire_and_answers_tools_call
（channel is empty and sending half is closed）；单跑该用例 1 passed。
$ ./scripts/cargo-rmcp-patched.sh test --locked --offline -p peri-acp --lib --no-run
Finished（exit 0）
$ cargo check -p peri-acp --all-targets --offline
Finished（exit 0）

$ cargo clippy -p peri-swarm -p peri-middlewares --all-targets --offline -- -D warnings
Finished（无 warning）

$ cargo fmt -p peri-swarm -p peri-middlewares -p peri-agent -p peri-acp -p peri-acp-types -- --check
（无 diff）

$ MISE_DISABLE_TOOLS=rust npx -y lefthook@1.13.6 run pre-commit
check ✔ / clippy ✔ / feature-catalog ✔ / layer-imports ✔ / tui-components ✔
fmt ✘：`cargo fmt --check` 在未改动的 `mcp-packages/peri-browser-gateway/`
报格式差异；该目录按本轮约束不得触碰，失败非本分支改动引入。
其它 hook 的退出码均为 0；本项不能声称通过。
```

## 3. 逐条验收映射

| 验收口径 | 覆盖方式 | 状态 |
| --- | --- | --- |
| `required_actions` 为官方嵌套形状，`request_id`/`turn_id` 由持久事实派生 | `a_pending_action_matches_the_official_nested_shape_and_keeps_null_fields`、`declared_always_opens_a_pending_approval_and_broadcasts_requires_action`、`a_requires_action_snapshot_is_an_official_session_event`、`replay_folds_approval_rows_and_never_revives_a_settled_action` | 单元级通过（形状按本机 `openai@7.60.0` `agents.d.ts:937-975` 核对） |
| 纯官方 answer body 被接受且字段错误可诊断 | `an_approval_result_event_resolves_to_a_decision`、`an_approval_response_type_must_be_mcp_tool_call`、`an_approval_result_requires_the_official_response_object` | 单元级通过（形状按 `agents.d.ts:1605-1634` 核对） |
| 声明范围内、来源缺失 → 进入审批；无通道 → `PolicyDenied` | `a_scoped_mcp_call_without_a_binding_enters_approval_instead_of_bypassing`、`a_scoped_mcp_call_without_a_broker_is_policy_denied`、`a_scoped_tool_invoke_targeting_mcp_enters_approval_without_a_binding` | 单元级通过 |
| 子 agent 继承父会话审批范围；空范围行为不变 | `a_subagent_inherits_the_parent_scope_and_asks_for_approval`、`an_empty_parent_scope_keeps_the_subagent_baseline`、`the_bound_subagent_tool_forwards_its_scope_to_the_child_chain`、`a_scoped_subagent_without_a_broker_fails_closed` | 子链装配（含 scope-only fail-closed 分支）与 `SubAgentTool→SessionBoundAssembler` 接线通过；真实子 agent 执行链未端到端验证 |
| 硬崩溃 pending 行恢复结算；送达失败保留待办 | `recovery_settles_orphan_approvals_and_replay_stops_projecting_them`、`recovery_marks_orphan_approvals_actor_lost_while_the_turn_is_open`、`a_delivery_failure_release_keeps_the_action_answerable` | 函数/注册表级通过；真实 actor 重启与 ACP child 断连注入未验证 |
| 声明 `require_approval` 的 MCP 工具进入 `requires_action`，事件只发一次，turn 不终止 | `actor::reader::approval_tests::declared_always_opens_a_pending_approval_and_broadcasts_requires_action`；`domain::approval_test` 策略用例 | 单元级通过 |
| `approve` 后工具执行 | 决策写回原 ACP id（`ApprovalReply::Applied` 断言 `acp_request_id == 42`）；工具执行由 host 负责 | 单元级覆盖送达点；真实 host 执行未验证 |
| `deny`/审批 `cancel` 只结算该次调用 | `acp_responses_keep_deny_and_cancel_distinguishable`（cancel 走 ACP `cancelled`，不伪造用户拒绝） | 通过 |
| 串行重复/并发/跨 session/旧代次答复只有一个成功 | `an_approval_is_settled_once_…`、`two_concurrent_answers_have_exactly_one_winner`、`another_session_never_knows_this_approval_id`、`a_turn_terminal_settles_its_pending_approvals` | 通过 |
| cancel 与答复竞争 | 结算在 registry 互斥内完成；竞态失败方得到 `Settled`/`Unknown`。未新增真实 `session/cancel` 竞态测试 | 部分覆盖 |
| 无客户端在线/等待中断线/broker 到期/actor 重启 | 无 SSE 客户端≠无人可答（待办保留并可 retrieve）；actor 丢失 `actor_loss_invalidates_every_pending_approval`；到期 `an_expired_approval_is_not_projected_and_cannot_be_answered`；硬崩溃 pending 行由 `settle_orphan_approvals` 结算 | 通过（语义见 §4；真实 actor 重启未端到端） |
| 旧 ID 不可答复 | 结算记录 + 新代次空 registry → `unknown_required_action` | 通过 |
| 同批免审批工具、不同 server 同名工具不扩大 | `a_declared_never_tool_is_auto_approved_…`、`a_scoped_tool_without_a_rule_waits_and_a_never_server_is_not_escalated`、`only_declaring_servers_are_escalated_…`、`a_mixed_batch_only_escalates_the_calls_inside_the_declared_range`、`an_undeclared_server_does_not_trigger_the_broker_for_the_same_tool_name` | 通过（逐调用判定与绑定来源判定） |
| 同一 turn 多并行待办各自结算 | `a_parallel_approval_survives_answering_another` | 通过 |
| `Idempotency-Key` 重试返回同一回执 | `an_approval_is_settled_once_…`（同键 `Duplicate`） | 通过 |
| 重连读取待办、已结算不复活、答复不建新 turn | `replay_folds_approval_rows_and_never_revives_a_settled_action`；`a_reconnect_after_the_pending_row_still_receives_the_current_snapshot`（生产 actor 共用的 backlog 装配）；`an_expired_pending_approval_is_settled_before_the_replay_window`；答复路径不调用 `run_turn` | 回放/投影/生产装配通过；未做真实 SSE 连接重连测试 |
| 结算持久化失败不返回成功回执 | `a_settlement_persistence_failure_is_a_5xx_and_keeps_the_action_answerable`、`an_expiry_settlement_persistence_failure_fails_the_read_and_is_retryable`（store 注入 append 失败） | 通过（service 层故障注入） |
| 送达失败 502 且待办仍可答复 | `a_failed_acp_delivery_is_502_and_the_action_stays_answerable`（内存 ACP transport 关闭）、`a_delivered_approval_settles_and_leaves_no_pending_action` | 通过（service 层，未注入真实子进程） |
| `peri-acp` 测试目标可编译并运行 | `cargo check -p peri-acp --all-targets --offline`；`test -p peri-acp --lib`（1501 passed / 1 已知负载 flake，单跑通过） | 通过（见 §2） |
| 真实 SDK 探针（挂起/答复前后断线重连） | 未执行 | **未验证** |
| `AskUserQuestion` 不再返回空答案 | 由另一并行任务修复；本轮未改 `transport_broker.rs` 的 elicitation 分支 | 不在本单 |
| `sessions.list` 读取 live 待办 | 实现为对 live runtime 的 `requires_action` 覆盖（`AcpSwarmService::list_sessions`）；无端到端 actor 测试 | 实现，未端到端验证 |
| 环境投影 | `official_sessions_carry_the_recorded_environment`、`a_current_approval_projects_requires_action_…` | 通过 |
| 全量 test/clippy/fmt/lefthook | 见 §2 | 测试/clippy/本单 crate fmt 通过；lefthook 的 fmt 门因 `mcp-packages/peri-browser-gateway/` 既有格式差异未通过（不在本轮允许范围），其余 hook 通过 |

## 4. 未验证项与边界

1. **真实 ACP host 审批回路未端到端验证**：没有启动真实 LLM + 外部 MCP server 让
   host 真正发出 `session/request_permission`。已覆盖的是 read_loop 接线、策略判定、
   持久化、投影与答复送达点；`ApprovalItem.mcp_origin` → `params._meta.peri.mcp`
   的跨进程 wire 形状只经类型/单测保证。
2. **真实官方 SDK 探针未执行**：设计 §验收口径要求的“审批前/挂起/答复后断线重连，
   collector 只在原 turn 终态结束”没有跑。
3. **`sessions.list` live 覆盖、`respond_required_action` 的 actor/Rivet 路径**
   没有端到端测试；Rivet 新增 action `respond_required_action` 只通过编译与既有
   ignored 集成测试的构建。答复的 502/成功路径现在有 service 层故障注入
   （`a_failed_acp_delivery_is_502_and_the_action_stays_answerable`），但没有经过
   真实 Rivet actor 或真实 ACP 子进程。
4. **无客户端在线的语义**：swarm 的交互 endpoint 是 ACP 子进程，始终在线，
   `Unanswered{NoOnlineEndpoint}` 在这个部署里不会发生。没有 SSE/HTTP 客户端
   不等于“无人可答”，待办保留到 turn 结束或 host 期限，并在重连后经
   `sessions.retrieve`/SSE 快照发现；这与设计的“重连能读到待办”一致。真正的
   传输丢失由 `actor_lost` 结算覆盖。
5. **等待超时**用测试注入 deadline（`expire_approval_now`）验证，没有真实等待
   300s；期限常量与 middleware 同源。
6. **官方字段形状已按本机 `openai@7.60.0` 的 `agents.d.ts` 核对**：
   `SessionRequiredActionResourceMcpApprovalRequest`（`937-975`）与
   `SessionInputParamAgentSessionInputMcpApprovalRequestResult`（`1605-1634`）。
   核对方式是类型文件与单元断言，**没有运行官方 SDK 探针**；官方 SDK 的运行时
   反序列化/`checkAction` 行为仍未端到端验证。
7. **审批待办只随 actor 代次存活**：进程/actor 重启后旧公开 id 一律
   `unknown_required_action`；已结算/失效事实通过 `kind=approval` 的 item 行
   留在历史里，不会重新出现在 `required_actions`。
8. **硬崩溃边界已返修**：恢复时会把遗留的 pending 行结算为 `actor_lost` 或
   `turn_ended`，回放/retrieve/list 不再展示不可答复的动作。验证是函数级
   （`settle_orphan_approvals` + 回放折叠），没有在真实进程被 kill 后通过 Rivet
   重启 actor 的端到端测试；`on_start` 的接线只经编译与代码审阅。
9. **送达一致性验证范围**：`claim/release/complete` 在注册表级覆盖了“失败不
   删除待办、成功后结算、并发只有一个认领者”；第二轮返修又用内存 ACP transport
   在 service 层注入“传输关闭”（502）与“结算行写入失败”（503），断言待办仍可答复、
   重试可结算。**没有**注入真实 ACP 子进程的 `respond_permission` 失败，也没有在
   真实 Rivet actor 上做 HTTP 端到端故障注入。
10. **子 agent 继承验证范围**：覆盖子链装配上下文（范围 + broker → 子链
   `PermissionMiddleware`）、`SubAgentTool` → `SessionBoundAssembler` 的接线，
   以及第二轮新增的“有范围、无 broker → `PolicyDenied`”专门分支测试；没有启动
   真实子 agent 执行工具，`SubAgentMiddleware::build_tool` 的父链注入也只经编译
   与代码审阅。
11. `computer_use`、`function_call`、`environment_connection` 仍未映射；
   `AskUserQuestion` 仍未映射成官方动作；token/usage 记账未做。
12. **生产 SSE 快照的验证层级**：`a_reconnect_after_the_pending_row_still_receives_the_current_snapshot`
   与 `an_expired_pending_approval_is_settled_before_the_replay_window` 直接驱动
   `AcpSwarmService::subscribe_backlog`（生产 actor `GET /events` 与 ACP
   `subscribe` 共用的唯一装配），不是真实 Rivet 引擎 + SSE 客户端连接；真实
   kill/restart、真实 SDK 探针与真实 300s 等待仍未执行。
13. **`peri-acp` 全量 lib 测试的已知负载 flake**：
   `host::mcp_v4_wire_fixture::wire_fixture_script_logs_wire_and_answers_tools_call`
   在并行/高负载下偶发 `channel is empty and sending half is closed`；本轮两次全量
   运行均为 1501 passed / 1 failed（同一用例），单跑该用例通过。这是既有测试夹具
   的时间敏感问题，本轮未修改该夹具，不能据此声称全量绿。
14. **lefthook 的 fmt 门未通过**：`cargo fmt --check` 在未改动的
   `mcp-packages/peri-browser-gateway/` 报告格式差异（本轮明确禁止触碰该目录）。
   该失败不是本分支改动引起的；其余 hook（check、clippy、feature-catalog、
   layer-imports、tui-components）均通过。不能声称 lefthook 全绿。
