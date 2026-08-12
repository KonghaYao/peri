# acp-hub 核心链路 ACP 协议面与官方 v1 冲突（权限/tool 状态/长 turn 超时），存在断链与误判风险

**状态**：Open
**优先级**：高
**创建日期**：2026-08-12

## 问题描述

按 ACP 官方协议（agent-client-protocol，schema v1）审核 acp-hub 核心链路 fe → server yjs → acp stdio，发现 7 处问题：权限机制与官方 v1 双端不符（出站 `permission.resolve` 非官方方法、入站 `session/request_permission` 未实现）、`tool_call_update` 官方 `failed` 状态被误判为 started、L3 30s 硬超时对长流式 turn 误判 delivery_unknown，另有 DocId 前缀解析不一致、binding 校验漏杀无 sessionId 业务帧、turn_id 死字段与多余非官方字段。期望：acp-hub 的 ACP 协议面以官方 v1 形态为准（仅使用官方形态），同时 **peri 相关代码一律不更改**。

## 症状详情

### #1 [P1] 权限机制与官方 ACP v1 双端不符

- **出站**（translator.rs:128 `permission.resolve`）：官方 ACP v1 方法表（meta.json）无此方法。官方权限机制是 agent → client 的 `session/request_permission` **request**（带 id），client 以 `{outcome: "selected", optionId}` 或 `{outcome: "cancelled"}` 响应。
- **入站**（acp_channel.rs:242 normalize_json_rpc）：`session/request_permission` 落到 `UnsupportedFrame` 被丢弃，**且不回 JSON-RPC 响应**——对接标准 agent 时其权限请求挂起等待应答。
- 现状依赖 peri 私有原始形态 `{type:"permission_request", payload}`（map_raw:475 支持）。若 peri 走官方 `session/request_permission`，权限流完全断裂。
- **期望**：acp-hub 入站实现官方 `session/request_permission`（request 带 id，须回响应）；出站以官方形态应答；不更改 peri。

### #2 [P1] `tool_call_update` 官方 `failed` 状态误判为 started

- map_acp_update（acp_channel.rs:310-341）只识别 `completed/complete/done` 与 `error`；官方 `ToolCallStatus` = `pending/in_progress/completed/failed`。
- **`failed` 落默认分支被投影为 `ToolCallStarted`**——工具已失败却显示运行中。
- 原始形态分支（map_raw:457）已识别 `failed`，包裹格式（官方形态）漏了。
- **期望**：包裹格式按官方 `pending/in_progress/completed/failed` 全映射。

### #3 [P1] L3 30s 硬超时对长流式 turn 误判 delivery_unknown

- prompt 的终态信号是 L3 响应（command_coordinator.rs:1700）；标准 ACP 的 `session/prompt` 响应**只在 turn 结束时才回**。
- `L3_TIMEOUT = 30s`（command_coordinator.rs:61）：工具链长 turn（>30s）必然触发路径 B——清 active_turn、回前端 error，但 agent 仍在运行、增量继续到达，聚合器失去归位锚点，内容投影错乱。
- **期望**：超时语义改为「turn 活跃期间不超时 / 仅对无增量窗口计时」，或与流式 turn 时长解耦。

### #4 [P2] DocId 前缀面不一致（`session:` 缺注册、`control:` 残留）

- `DocId::session()` 生成 `session:{cid}`（proto/src/conn.rs:85）；`FromStr` 只放行 `chat/control/hub`（conn.rs:117）——`session:` 解析必失败（当前无生产路径走 FromStr，属潜在坑）。
- `doc_cid()`（gateway.rs:790）只认 `chat:`/`control:` 前缀：订阅 `session:{cid}` **不触发 open_chat**，单独订阅 control doc 的客户端静默拿不到快照（快照 None 后无补推机制）。
- 术语表（terminology.md:50）仍写 `DocId::control → control:{chat_id}`，与代码 `session:` 前缀冲突，文档滞后。
- **期望**：前缀面统一（`session:` 入 FromStr、doc_cid 认 `session:`、术语表同步），`control:` 残留清理。

### #5 [P2] binding 校验漏杀无 sessionId 业务帧

- relay（relay_event_handler.rs:169-193）要求帧内可提取 sessionId，否则只放行 `RpcResponse`——`agent/status`、原始形态 `session_list` 等无 sessionId 帧被 `binding_missing` 丢弃。
- **期望**：对无 sessionId 但业务必需的帧（agent/status 等）提供合法化路径（如按信封 session_id 关联）。

### #6 [P3] `OutboundCtx.turn_id` 是死字段

- translator prompt 构造（translator.rs:92-111）未把 turn_id 写入 params，注释（:46）声称会下发；聚合器按 active_turn 归位兜底，一致性无碍，但注释与实现不符。
- **期望**：删除死字段或让注释与实现一致。

### #7 [P3] 出站携带非官方字段

- `session/prompt` 携带 `cwd`/`effort`、`session/cancel` 携带 `cwd`、`initialize` 携带 `cwd`——官方 schema 均无（JSON-RPC 宽容，serde 忽略未知字段，低危）。
- `session/list` 出站只有 `{cwd}`（command_coordinator.rs:896），官方 params 形态未确认。
- **期望**：以官方字段清单为准清理多余字段，`session/list` params 对照官方确认。

## 复现条件

- **复现频率**：对接标准 ACP agent 时 #1/#2 必现；长 turn（>30s）时 #3 必现；#4/#5 为潜伏一致性缺陷，当前无生产路径触发；#6/#7 为代码/文档质量问题。
- **触发步骤**：
  1. 用标准 ACP agent（如实现 `session/request_permission` 的 agent）发起权限请求 → 权限流挂起（#1）
  2. 工具执行失败（官方 `failed` 状态）→ 前端显示 started（#2）
  3. 发起 >30s 的工具链 turn → 前端 error、增量错乱（#3）
- **环境**：acp-hub 当前对接 peri 私有形态时未复现（#1/#2 依赖 peri 实际协议面，未验证）；约束：不更改 peri。

## 涉及文件

- `acp-hub/server/src/protocol/translator.rs` —— 出站翻译（#1 出站、#6、#7）
- `acp-hub/server/src/protocol/acp_channel.rs` —— 入站规范化（#1 入站、#2）
- `acp-hub/server/src/channel/command_coordinator.rs` —— L3 超时与 turn 终态（#3）、session/list（#7）
- `acp-hub/server/src/channel/relay_event_handler.rs` —— binding 校验（#5）
- `acp-hub/server/src/channel/gateway.rs` —— doc_cid 前缀判定（#4）
- `acp-hub/proto/src/conn.rs` —— DocId FromStr 前缀面（#4）
- `acp-hub/docs/terminology.md` —— 术语表 DocId::control（#4）
- `acp-hub/instance/src/child.rs`、`acp-hub/instance/src/hub.rs` —— 链路已验证正确，勿动（参考）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-12 | — | Open | agent | 创建（核心链路协议审核产出） |
| 2026-08-12 | Open | Fixed（已验证） | agent | devflow（max）3 slice 修复完成，独立验证 VERIFIED（见修复记录） |

## 修复记录

### 2026-08-12 修复（max devflow，3 slice 串行 + 只读并发）

**Slice 1 协议面**（评审 APPROVED）：
- #1 入站：`NormalizeOutcome::PermissionRequest` 变体 + `normalize_request_permission`（官方 `session/request_permission`：sessionId/toolCallId/options 校验、server 生成 uuid permission_id、request_id 保留 string/number）
- #1 出站：`translator::permission_response_rpc`（`result.outcome.{outcome,optionId}`，id 原样回显）；exec_resolve 双轨——官方请求（relay `pending_permissions` 表 take 命中）→ 官方响应帧无 L3（mark_dispatched→forward→confirmed→committed）；原始形态 → 旧轨 translate+register_rpc+L3 原样保留（不改 peri）
- #1 支撑：`forward_rpc` id 提取放宽（string/number）；duplicate 分支清理 pending 表项；断链/进程退出按 chat 清理
- #2：`map_acp_update` 官方 `failed` 并入 error 终态分支（`Some("failed") | Some("error")`）

**Slice 2 状态与字段**（评审 APPROVED）：
- #6：`OutboundCtx.turn_id` 死字段删除（coordinator 4 处构造同步清理；turn_id 其余引用保留）
- #7：`session/prompt` 删 cwd/effort、`session/cancel` 删 cwd、`initialize` 删 cwd（均对齐官方 params；session/new、session/load、session/list 的 cwd 为官方字段保留）
- 评审 P2-1：空 options Allow 分支回落 cancelled（消除 `optionId:null` 契约违例）；P2-2：实时路径 PermissionRequest 分支补 recover_from_gap

**Slice 3 时序与一致性**（评审 APPROVED）：
- #3：`active_turns` 表扩为 `{turn_id, last_activity: Instant}`；relay submit 成功统一 touch（buffer_sync 同构）；exec_prompt L3 改增量窗口循环（idle > 窗口才 delivery_unknown；表清理并入终态主体，无命令悬挂）
- #4：`DocId::from_str` 白名单 control→session；`doc_cid` 认 `session:`（删 control: 死分支）；terminology.md/update_log.rs 注释同步；e2e-flow.mjs/ws-verify.mjs 前缀修复
- #5：child.rs 与 relay 对称「有 jsonrpc 键」放行（原始形态双端仍拒）；relay None 分支改双前置检查（jsonrpc 键 + 信封 chat 存在）后并入正常投递路径

### 验证证据（独立 verification，VERIFIED）
- build/clippy（-D warnings 独立全量重跑）零警告
- 测试：server 342 / proto 34 / instance 42 / child_test 7 全过
- 回归锚点 17/17（含原始轨保留：resolve_duplicate_and_unknown、prompt_l3_stop_reason_maps_turn_terminal、binding_missing_dropped）
- e2e-flow.mjs 12/12 PASS（plan 基线「已知红」→ 转绿）
- 非阻塞：child_test `test_stdout_events_and_dropped_no_sid` 既有低频 flaky（`--crash-after 100` 时序竞态，非本次引入）

### 遗留（不阻塞）
- P2-3：`take_pending_permission` 先于 entry 检查（极小窗口，断链并发）
- P2-4：pending_permissions 表 submit 失败时表项滞留至断链清理（无害）
- 注释残留 `control:`：`server/src/control/hub.rs:328/332`、`docs/architecture.md:364`（纯文档）
- relay C2 方法面帧 entry 检查无直接单测（防御性分支）
- 两处 `extract_session_id` 实现未合并（child.rs error.rs vs acp_channel.rs）
- devflow 全流程文档：`.peri/plans/acp-hub-core-chain-acp-protocol-conflicts/`（00-context → 05-verification）
