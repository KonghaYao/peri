# B-acp-host — session/load 路径与生命周期路由（peri-acp 现状测绘）

Scope: `peri-acp/src/host/**`、`peri-acp/src/dispatch/**`、`peri-acp-types/src/{peri_caps,session_resources,system_reminder,store}.rs` 及相关读取实现（`peri-resources`）。
Method: 只读仓库，未执行 build/test。所有行号取自本 worktree 当前 HEAD（`7f0de56e8`）。

## Summary

1. `session/load` 不走 spawn 路径：`ServerLoop::run` 只 spawn `session/prompt`、`mcp/message`、`session/input/snapshot`、`peri/mcp/*`，其余（含 `session/load`）在接收循环里就地 `await`（`peri-acp/src/host/server_loop.rs:31-41`）。
2. 就地的 `dispatch_request` 先取 **per-session prompt 锁**（`try_lock_owned`，忙则回 `-32010`），再取 **全局 sessions 锁**，然后才 `handle_request`；**全局 sessions 锁在整个 `handle_request`（含全部存储 I/O 与历史回放）期间一直持有**（`server_loop.rs:376-439`、`456-458`）。
3. `session/load` 的实际工作是 `prepare_existing`（准入 + legacy 接纳 + 环境装配 + 历史装载）→ `reopen_workspace_scope_after_restore` → `replay_persisted_session_history`（回放）→ `AvailableCommandsUpdate` + `ConfigOptionUpdate` → 响应（`session_restore.rs:491-561`）。
4. `load_frozen_bytes` 只取 frozen 字节，但读的是整份 `SessionSnapshot`（meta/binding/frozen/payloads/flags/inherited，一次只读事务）；trait 上没有 frozen-only 读取入口（`session_restore.rs:51-64`、`session_resources.rs:594-635`、`sqlite_store/session_data.rs:323-368`）。
5. Peri 扩展能力集中在 `PeriCaps`（`clientCapabilities._meta` 的 `peri.*` key）+ `SessionManager.pending_caps/caps_registry`；`ConnectionContext` 不持有 caps（`peri_caps.rs:10-98`、`session/caps.rs`、`host/connection.rs:13-20`）。
6. `session/resume` 存在，与 `load` 共用 `prepare_existing`，但**不回放历史**、不返回 modes/configOptions（`session_restore.rs:563-603`）。
7. 可用的“代际/隔离”标识只有 `continuation_epoch: u64`、`UserInputMailbox.generation: String`、`SessionState.closing: bool`，以及每会话 `CancellationToken`；回放通知本身不带任何 request/generation 标识（见 Findings 3、5）。

## Findings

### 1. 入站 `session/load` 完整调用链（按调用顺序）

1. 入口：`peri_acp/src/host/mod.rs:381-391` — `ServerLoop { .. }.run().await`；`run` 定义在 `server_loop.rs:28`。
2. 路由：`server_loop.rs:31-41` — `IncomingMessage::Request { method, .. }` 的 `match method.as_str()`；`session/load` 落到 `_ => self.dispatch_request(id, method, params).await`（`server_loop.rs:40`）。请求在接收循环中**串行处理**（非 spawn）。
3. `dispatch_request`：`server_loop.rs:367`
   - `let sessions = self.sessions;`（`self.sessions: &'a SharedSessions`，`server_loop.rs:19`）
   - 生命周期锁分支：`server_loop.rs:376-439`，`session/load` 在名单内（`376-384`）；`lifecycle_session_id = extract_session_id(&params, "")`（`385`）；锁对象 `self.prompt_locks.lock().await.entry(id).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()`（`396-402`）；非 close/delete 分支用 `lock.try_lock_owned()`（`421`），失败即回 `AcpError::new(-32010, "Session is executing; retry after the current turn")` 并 `return`（`423-434`）。
   - 全局 sessions 锁 + 派发：`server_loop.rs:455-458` — `let mut sessions = sessions.lock().await; handle_request(&method, &params, cfg, &mut sessions, transport).await`（guard 在该 block 结束时 drop，位于 `459` 之前）。
   - `drop(lifecycle_lock)`：`server_loop.rs:459`。
   - 响应后置：`diagnostics.send(...)`（`483`）；`session_setup()`（`575-588`）对 `session/load` 返回请求里的 `sessionId`，据此触发 `requests::acp_mcp::attach_session_servers(local, transport, &params, &session_id)`（`485-499`）。
4. `handle_request`：`peri-acp/src/host/requests.rs:31-37` 签名
   `pub(crate) async fn handle_request(method: &str, params: &Value, cfg: &AcpServerConfig, sessions: &mut HashMap<String, SessionState>, transport: &Arc<dyn crate::transport::AcpTransport>) -> Result<Value, AcpError>`
   - `lifecycle = matches!(method, "session/new" | "session/load" | "session/resume" | "session/fork" | "session/close" | "session/delete")` → 这些方法用 host 级 `cfg`，不用会话环境 cfg（`42-58`）。
   - 分派：`"session/load" => session_lifecycle::handle_load(params, cfg, sessions, transport).await`（`95`）。
   - 响应成功后绑定会话任务流：`146-165`（`bind_session_tasks`，`169-…`）。
5. `handle_load`：`session_restore.rs:491-561`
   - `reject_incomplete_local_close(params, sessions)?`（`497`；定义 `605-621`，会话 `closing` → `-32010 "Session restore incomplete: prior runtime cleanup is pending"`）
   - `already_loaded = sessions.get(id).is_some()`（`498-502`）
   - `prepare_existing(params, cfg, sessions).await?`（`503`）
   - `reopen_workspace_scope_after_restore(...)`（`504`；定义 `623-671`）
   - 取 `state.environment` → 覆盖 `cfg`（`507-509`）；`history_payloads = state.history_payloads.clone()`（`510`）；`caps = cfg.session_manager.ensure_session_caps(id)`（`511`）
   - **回放**（先于响应）：`dispatch::replay_persisted_session_history(req_session_id, &history_payloads, &replay_sender, &caps).await`（`514-526`），失败只 `warn!` 继续
   - `send_config_option_update`（`530`）；`LoadSessionResponse::new().modes(modes).config_options(config_options)`（`532-540`）
   - `send_available_commands_update(...)`（`543-550`）；`prewarm_session_mcp_discovery`（`556`）
   - `identity_response(serde_json::to_value(resp)?, identity)`（`557-560`；`_meta["peri.sessionWorkspaceV1"]`，`304-312`）
6. `prepare_existing`：`session_restore.rs:72-272`（签名 `72-76`）
   - `reject_closing_session(params, cfg).await?`（`77`；定义 `28-45`，经 `session_resources.is_session_closing`）
   - `inspect_availability(Some(id))`：`AccessMode::ReadOnly` → `ReadOnlyStore` 错误（`82-94`）；`execution` 只接受 `Available | BindingMissing`（`95-103`）
   - `drain_persistence(id)`（`104-107`）
   - `session_mcp_servers(params)` 定格候选 servers（`113`）
   - `legacy_session::prepare_for_restore(cfg, id, None, effective_servers)`（`115-116`；实现在 `host/requests/legacy_session.rs:87-…`）
   - 核心块 `123-241`：
     - `workspace::validate_expected(cfg, id, params["cwd"])`（`124-129`）
     - `load_frozen_bytes(cfg, id)`（`130`）+ `decode_frozen_snapshot`（`131`）
     - `response_identity(cfg, id)`（`132`）
     - **已在 live 表**：补历史（`history_payloads.is_empty()` 时 `dispatch::load_session_payloads`）→ `RestoreOutcome::AlreadyLive`（`133-146`）
     - **冷路径**：`load_session_payloads(cfg.controller.as_ref(), id)`（`147`）→ `PreparedSessionInputs::prepare_restore(cfg, &cwd, &persisted)`（`155-158`）→ `SessionEnvironment::assemble_prepared`（`166-173`）→ `session_manager.ensure_session_with_task_manager(id, &cwd, task_manager)`（`178-182`）→ `ensure_session_caps(id)`（`183`）→ workflow chain + `attach_session_workflow`（`190-206`）→ `RestoreOutcome::Publish(SessionState { .. continuation_armed: false, continuation_epoch: 0, .. })`（`207-239`）
   - 结果处理：`Publish` → `environment.activate()` + `sessions.insert(id, state)`（`244-254`）；`AlreadyLive` → 排空候选环境（`255-269`）；`Err` → `shutdown_bootstrapped_environment`（`270`）
7. 历史读取：`dispatch::load_session_payloads` → `peri-acp/src/dispatch/session_load.rs:18-27`
   `controller.sessions().load_session_history(&ThreadId::from(thread_id.to_string()))`，错误映射 `-32603 "session history load failed: {error}"`。
8. 回放投射：`dispatch::replay_persisted_session_history`（`dispatch/session_replay.rs:25-45`）→ `replay_session_history`（`58-206`）；sender 为 `TuiReplaySender`（`session_restore.rs:673-737`），`send` 走 `transport.send_notification("session/update", payload)`（`684-691`）。
9. 响应：`ResponseDiagnostics::send`（`host/diagnostics.rs:41-45`）→ `transport.send_response(id, result)`（`:65`）。
10. 能力检查点：`session/load` 本身**没有**“未协商即拒绝”的 caps 门；caps 只影响 (a) `_meta.peri.sessionWorkspaceV1` 是否写入（`response_identity`：`session_workspace_v1` 为 false → 返回 None，`314-327`）与 (b) 回放的 `periReplay` 标记（见 Findings 5）。

### 2. `load_frozen_bytes`

- 签名与实现（`session_restore.rs:47-64`）：
  ```rust
  async fn load_frozen_bytes(cfg: &AcpServerConfig, session_id: &str) -> Result<String, AcpError>
  ```
  读：`cfg.session_resources.load_session_snapshot(&session_id.to_owned())`（`52-56`），`map_err(crate::host::workspace::resource_error)`。
  返回：`FrozenState::Present(bytes) => Ok(bytes.into_string())`（`58`）；`FrozenState::LegacyAbsent => Err(-32603 "Bound session has no frozen snapshot")`（`59-62`）。
  注释明说两类都不是“可重建”情形（`47-50`）。
- 读的是整份 `SessionSnapshot`：`pub struct SessionSnapshot { meta, binding, frozen, payloads, flags, inherited }`（`peri-acp-types/src/session_resources.rs:228-235`）；实现 `peri-resources/src/sessions/resources.rs:416-420` → `gate.data().load_snapshot(id)`；SQLite 实现在**一次只读事务**里读 meta + binding + frozen + `context::load_payloads_on`（= 该会话自有完整历史）+ compaction flags + inherited context（`peri-resources/src/sessions/sqlite_store/session_data.rs:323-368`）。
- “只取 frozen 却付全量读”的原因（可得的代码证据）：`SessionResources` trait 的读取面只有 `load_session_snapshot` / `load_session_binding` / `validate_bound_workspace` / `load_session_history` / `load_session_meta` / `session_environment_id` / `session_workspace_id`（`session_resources.rs:594-635`），**没有 frozen-only 读取**；`load_session_meta` 是“小型 metadata 投影；不加载历史或大快照”，但不含 frozen（`session_resources.rs:622-623`）。
- 返回值是**装配输入**而非“配置”：调用点 `session_restore.rs:130` 之后立刻 `decode_frozen_snapshot(&persisted)`（`131`，实现 `peri-acp/src/session/frozen_snapshot.rs:147`）并交给 `PreparedSessionInputs::prepare_restore(cfg, &cwd, persisted_snapshot)`（`host/prepared.rs:128-139`）。解码产物 `FrozenSessionData` 承载 system prompt / CLAUDE.md / claude.local.md / skill summary / date / beta flags / runtime env（`peri-agent/src/session/exec/executor/context.rs:28-90`；envelope 字段见 `frozen_snapshot.rs:100-145`）。
- 全部调用者：`load_frozen_bytes` 在仓库中**只有一处调用** —— `session_restore.rs:130`（同文件定义）。近亲（不是本函数）：`legacy_session::persisted_frozen_bytes`（`host/requests/legacy_session.rs:44-59`，调用点 `:118`）也走 `load_session_snapshot`；`legacy_session::prepare_for_restore` 自身再读一次 `load_session_snapshot`（`:95-98`）。
- 由此，一次冷 `session/load`（legacy 未绑定路径）至少发生：`inspect_availability`（`session_restore.rs:82-86`）→ `prepare_for_restore` 的 snapshot 读（`legacy_session.rs:95-98`）→ 可能的 `persisted_frozen_bytes` 二次 snapshot 读（`legacy_session.rs:118`）→ `load_frozen_bytes` 的 snapshot 读（`session_restore.rs:130`）→ `load_session_payloads`（`:147`）→ `response_identity` 里的 `load_session_binding` + `load_session_meta`（`:349-358`）。

### 3. 现有请求隔离 / 代际标识（可用于回放竞态）

| 名称 | 类型 / 定义 | 语义与使用点 |
| --- | --- | --- |
| `PromptLocks` | `pub(crate) type PromptLocks = Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>`（`host/mod.rs:275`） | 每会话执行序列化；load/resume/fork/rewind/close/delete 与 prompt/continuation 共用（见 Findings 4） |
| `SessionState.closing` | `bool`（`host/mod.rs:144`） | 关闭/拒绝准入；load 前 `reject_incomplete_local_close`（`session_restore.rs:605-621`）、`prepare_existing` 内 `state.closing` 检查（`:133-136`） |
| `SessionState.cancel_token` | `Option<CancellationToken>`（`host/mod.rs:148`） | 每 turn 设置（`host/prompt.rs:336`）、失败时清空（`prompt_dispatch.rs:256-262`）；`session/close|delete` 会 cancel（`server_loop.rs:386-393`） |
| `SessionState.continuation_epoch` | `u64`（`host/mod.rs:150`） | 用户新 prompt 递增，作废已排队 continuation（`prompt_dispatch.rs:97-104`）；校验 `continuation_still_valid(state, epoch) -> bool`（`host/continuation.rs:121-122`）；load 发布时固定 `0`（`session_restore.rs:222`） |
| `UserInputMailbox::generation()` | `&str`（`peri-agent/src/session/user_input_mailbox.rs:135-137`，构造为 `uuid::Uuid::now_v7().to_string()`，`:112`） | 队列实例代际；过期队列入队报 `UserInputQueueError::StaleSession`（`:39-40`）；事件转发按 generation 过滤（`host/user_input.rs:162,175`） |
| `UserInputRunTicket` | `{ pub id: String }`（`user_input_mailbox.rs:26-28`） | 单次执行身份，“失效 ticket 不能启动或结算后来的一次执行”（`:24-28`） |
| `TaskSnapshot.revision` / `TaskChange.revision` | `u64`（`peri-acp-types/src/tasks.rs:59,80`） | 任务流订阅/去重（`requests.rs:191`） |
| `AcpServerConfig.host_task_spawner` + `HostTaskOwnerKind::Session` | `task_scope`（`host/task_scope.rs`） | 任务归属隔离（`server_loop.rs:60-63, 123-126`） |
| `connection_cancellation: CancellationToken` | `server_loop.rs:24` | 连接级取消，spawn 路径的 `select!` 用（`server_loop.rs:72-76`） |

缺口（可核对的事实）：`SessionState` 没有“live 实例代际/版本”字段（`host/mod.rs:136-167` 全字段列表）；回放发出的 `session/update` 只带 `sessionId` + `message_id`（`session_replay.rs:81-83, 101-104, 217-219`），不带 request id / generation；`session/load` 也不递增 `continuation_epoch` 或重置 mailbox generation（`session_restore.rs:207-239`）。

### 4. 执行生命周期锁

- 类型：`host/mod.rs:275`（见上表）。初始化：`host/mod.rs:331`，随 `ServerLoop` 传入（`host/mod.rs:385`）。
- `session/load` 的获取：`server_loop.rs:396-402`（取该会话的 `Arc<Mutex<()>>`）→ `server_loop.rs:421` `lock.try_lock_owned()`；失败回 `-32010`（`423-434`）。释放：`server_loop.rs:459` `drop(lifecycle_lock)`（**在全局 sessions 锁释放之后**）。
- 同一把锁还守：`session/resume` / `session/fork` / `session/rewind`（`try_lock`，`376-384, 421-435`）、`session/close` / `session/delete`（带 5s 超时的阻塞获取，`403-419`；超时回 `-32010 "Session close incomplete: active execution has not stopped"`）、用户 prompt 与 continuation（`prompt_dispatch.rs:135-145` `prompt_lock.lock().await`；continuation 经同一 `dispatch_prompt_turn`，见 `host/continuation.rs:429-430` 注释）、关闭时清空整表（`host/shutdown.rs:152`）。
- 全局 sessions 锁：`type SharedSessions = Arc<tokio::sync::Mutex<HashMap<String, SessionState>>>`（`host/mod.rs:272`）。`dispatch_request` 在 `server_loop.rs:456` 获取，guard 在 `458` 所在 block 结束处 drop；`handle_request(...).await`（`457`）**整个期间持锁**。
- 是否跨存储 I/O 持锁：**是**。`handle_load` 的全部 await 都在该 guard 内执行，包括 `inspect_availability` / `drain_persistence` / `prepare_for_restore`（含 snapshot 读）/ `load_session_payloads` / `load_frozen_bytes` / 环境装配 / **历史回放逐条 `send_notification`**（`session_restore.rs:503-526`）。同一时间段内其它请求、通知（含 `session/cancel`）都排在接收循环之后（只有 `session/prompt` 等被 spawn 的路径不等待）。
- 相关但不同的“短锁”用法：`spawn_session_io` 只在 prepare 阶段短暂持锁（`server_loop.rs:65-68`，`441-444`）；`schedule_mailbox` 在锁外（`460-471`）。

### 5. 回放投射 / 受众过滤 / 全历史迭代位置

- 迭代位置 1：`replay_persisted_session_history`（`session_replay.rs:25-45`）`for payload in history` 全量遍历 `Vec<PersistedPayload>`：
  - `PersistedPayload::Message(m)` → `replay_session_history(..., std::slice::from_ref(m), ...)`（`:33-36`）
  - `PersistedPayload::SystemReminder { reminder, .. }` → `sender.send_system_reminder(session_id, reminder.as_reminder(), caps)`（`:37-41`）
- 迭代位置 2：`replay_session_history`（`:58-206`）`for msg in history.iter().filter(|m| !m.is_system())`（`:64`；`is_system` = `matches!(self, Self::System { .. })`，`peri-acp-types/src/messages/message.rs:292-294`）：
  - `Human`：先 `legacy_compact_reminders(msg)` 抽取 legacy compact 提醒（`:67-74`，定义 `peri-acp-types/src/compact_reminder.rs:12-…`），命中则改走 reminder 通道并 `continue`；否则 `SessionUpdate::UserMessageChunk`（`:76-84`）
  - `Ai`：`Text` → `AgentMessageChunk`；`Blocks` 中 `Reasoning` → `AgentThoughtChunk`、`Text` → `AgentMessageChunk`、`ToolUse` → `project_tool_start` + `ToolCall`（`:94-174`；`Raw(_)` / Image / Document / Unknown 跳过，`:119, 158-159`）
  - `Tool` → `ToolOutput { text, execution }.projected_text(None)` + `project_tool_completion(...)` → `ToolCallUpdate`（`:176-201`）；其余 `_ => continue`（`:202`）
- caps 门控：`replay_chunk` / `replay_tool` / `replay_tool_update` 仅在 `caps.replay` 为 true 时打 `_meta.periReplay = true`（`:208-226, 229-236, 238-246`）。
- 受众过滤：`TuiReplaySender::send_system_reminder`（`session_restore.rs:693-736`）先 `reminder_egress_allowed(reminder, ReminderAudience::Tui)`，不通过则仅 debug 日志、不发（`:699-713`）；通过后按 `caps.system_reminder` 选 `"system-reminder"`（带 `{"reminder":..,"replay":true}`）或 `"system-reminder-fallback"`（`{"text": summary|body, "replay":true, "legacy":true}`），经 `peri/unstable_event` 通知发出（`:714-735`）。
- 受众模型：`ReminderAudience { Model, Tui, Diagnostics, Automation }`（`peri-acp-types/src/system_reminder.rs:58-65`）、`ReminderAudiences(Vec<..>)`（`:67-75`）、判定 `reminder_egress_allowed`（`:473-479`，注释要求模型投影 / ACP live+replay / stdio / fallback 出口都先过此判定）。
- live 侧与 replay 同规则：`session/event_sink.rs:166-168`、`session/event_sink/stdio.rs:106-108` 用同一 `reminder_egress_allowed(.., ReminderAudience::Tui)`。
- 另一条“全历史遍历”出口（非回放）：`handle_metadata(history = true)` 把 `load_session_history` 的 payload 全量序列化进响应 `payloads`（`session_restore.rs:458-487`）。

### 6. 能力协商（Peri 扩展）

- 声明结构：`peri-acp-types/src/peri_caps.rs:10-73` `pub struct PeriCaps`，字段全部 `#[serde(default)]`（默认 false / 空），包括 `rewind`（`:51-56`，注释明确“默认必须为 false…三个非标准 RPC 都必须 fail closed”）、`system_reminder`（`:57-59`）、`session_workspace_v1`（`:63-65`）、`ui_commands: Vec<UiCommandSpec>`（`:66-72`）。
- 解析（per-connection client caps 的**唯一**读入口）：`handle_initialize`（`host/requests/session_lifecycle.rs:124-144`）读 `params["clientCapabilities"]["_meta"]` → `PeriCaps::from_client_meta`（`:131-137`，实现 `peri_caps.rs:77-98`，键名如 `peri.tokenStats` / `peri.replay` / `peri.rewind` / `peri.systemReminder` / `peri.userInputQueue` / `peri.sessionWorkspaceV1`；`peri.uiCommands` 两态解析见 `:105-122`）→ `cfg.session_manager.set_pending_caps(peri_caps.clone())`（`:140`），并 `dispatch::build_initialize_response(&peri_caps)` 回显（`:142`）。
- 服务端回显：`to_agent_meta()`（`peri_caps.rs:124-167`）由 `build_initialize_response` 写入 `agentCapabilities._meta`（`dispatch/init.rs:38`），同时声明 `load_session(true)` / `sessionCapabilities.list|close|resume|fork|delete` / `mcpCapabilities.acp(true).http(true)`（`dispatch/init.rs:26-37`）。
- 存储与读取：`SessionManager.pending_caps`（每连接/进程一份）+ `caps_registry: DashMap<String, PeriCaps>`（每会话）——`session/caps.rs`：
  - `set_pending_caps`（`:12-14`）、`pending_caps_was_set`（`:18-20`）、`negotiated_caps`（`:25-27`）、`effective_host_caps`（`:31-37`，未 initialize 时 `PeriCaps::all_enabled()`）、`consume_pending_caps`（`:45-51`，session/new 用，未协商回退全 false）、`get_caps(session_id)`（`:55-61`，未注册回退默认全 false）、`caps_registry()`（`:65-67`）、`ensure_session_caps(session_id)`（`:82-98`，幂等；已协商用协商值，未协商回退 `all_enabled()`）。
- 典型检查点：rewind 三个 RPC 用 `cfg.session_manager.get_caps(session_id)` + `require_rewind_cap`（`host/requests/rewind.rs:23, 42, 83, 140-149`，错误 `-32601 "peri.rewind capability not negotiated"`）；`peri/session_context` / `session/metadata` / `peri/session_history` 用 `effective_host_caps().session_workspace_v1`（`session_restore.rs:385-394, 420-429`；`host/requests/storage_v2.rs:40`）；`session/load` 只用它决定身份 `_meta`（`session_restore.rs:314-327`）。
- 新增一处 cap 的注册面（全部为手工同步点，无编译期穷尽检查）：`PeriCaps` 字段（`peri_caps.rs:10-73`）→ `from_client_meta` 解析（`:77-98`）→ `to_agent_meta` 回显（`:124-167`）→ `all_enabled`（`:171-189`）→ 消费点（`get_caps` / `ensure_session_caps` / `effective_host_caps`）。
- 作用域事实：caps 不在 `ConnectionContext` 里——`ConnectionContext` 只有 `id/lifecycle/initialized/apps_enabled/app_sessions/cancellation`（`host/connection.rs:13-20`）；`initialize` 只 `commit_initialize()`（`server_loop.rs:472-474`，`connection.rs:34-38`）。caps 的“连接级”实际由 `SessionManager.pending_caps` 承担（`session/caps.rs:22-37` 注释：进程级快照），per-session 投影在 `caps_registry`。

### 7. `session/resume`

- 存在，分派点 `requests.rs:119-121` → `session_lifecycle::handle_resume`（`session_restore.rs:563-603`）。
- 与 `session/load` 的共同点：`reject_incomplete_local_close`（`569`）、`already_loaded`（`570-574`）、`prepare_existing`（`575`）、`reopen_workspace_scope_after_restore`（`576`）、`ensure_session_caps`（`582`）、`send_available_commands_update`（`588-595`）、`prewarm_session_mcp_discovery`（`596`）、`identity_response`（`599-602`）。
- 差异：`handle_resume` **不回放历史**（无 `replay_persisted_session_history`）、不读 `history_payloads`、不构造 `LoadSessionResponse`（无 modes/configOptions），也不发 `send_config_option_update`；响应为 `ResumeSessionResponse::new()`（`598`）。`handle_load` 独有的还有 `state.environment` 覆写 cfg 之后的 replay/config-options（`507-540`）。
- 协议侧能力声明含 resume：`SessionResumeCapabilities`（`dispatch/init.rs:33`）。

### 8. ACP rewind 路径读写与校验

- 三个 RPC 分派：`requests.rs:133-135`（`session/rewind-candidates` / `-preview` / `session/rewind`）；实现 `host/requests/rewind.rs`。
- cap 校验：三处都先 `require_rewind_cap(&cfg.session_manager.get_caps(session_id))`，未协商 `-32601 "peri.rewind capability not negotiated"`（`rewind.rs:23, 42, 83, 140-149`）。
- `rewind-candidates`：**不读存储**，只读内存 `SessionState.history`（`rewind.rs:24-28`）；会话不存在 → `-32602 "session not found"`。
- `rewind-preview`：读内存 `(s.cwd, s.history)`（`rewind.rs:43-46`）→ `dispatch::rewind_preview(params, &history, &cwd, &session_id)`（`dispatch/rewind.rs:52-92`）：解析 `RewindArgs { target_message_id, revert_files(默认 true), preview_fingerprint }`（`:29-43`）；按 `MessageId` 定位目标（未找到 `-32602`，`:61-74`）；`preview_material` 计算 `file_changes`（≤64 条，`MAX_FILE_CHANGES`/`MAX_SAFE_PATH_BYTES`，`:94-95`）与指纹（`PreviewFingerprint { schema, session_id, target_message_id, revert_files, removed_messages, file_changes }`，`:103-111`）。只读、零事件（`:45-51` 注释）。
- `session/rewind`（执行）：读会话内存 `(cwd, history, workspace_pool)`（`rewind.rs:84-97`），并拒绝 `s.closing`（`:88-90`）；定位目标（`:98-106`）；构造 `TransportEventSink`（`:107-111`）与 `PeriConfig` 快照（`:112`）→ `dispatch::rewind_execute(...)`（`:113-131`）：
  - `rewind_execute`（`dispatch/rewind.rs:215-344`）先解析参数（`:237-238`），再自行定位目标与**重算指纹**（`:247-262`），要求请求携带 `preview_fingerprint`（`:263-268`，缺失 `-32602`；不匹配 `-32602 "rewind preview is stale; request a new preview before executing"`，`:269-274`）。
  - 存储访问：`ctx.session_resources = Some(controller.sessions())`（`:305`，存储访问经 Controller 门面），`execute_rewind(ctx, target_message_id, revert_files)`（`:314`，`crate::session::command::execute_rewind`）——本文件未展开其 SQL 细节；返回后 `emit_command_feedback`（`:323`）、`push_done`（`:328`），`Cancelled` → `-32603 "rewind cancelled"`（`:330-332`），feedback error → `-32603`（`:333-335`），响应 `{"status":"executed","history":[...]}`（`:337-343`）。
  - 回写内存：`apply_canonical_rewind(&mut state.history_payloads, &mut state.history, target_id)`（`rewind.rs:134-136`；实现 `:52-69`）——canonical payload 权威，用 `peri_acp_types::store::history::apply_rewind(..., RewindBoundary::RemoveFrom(target_id))` 截断（“用户 rewind 移除目标本身及以后”，`:57-62`），`history` 由 payloads 单向派生（`:63-67`）。
- 锁：`session/rewind` 走 `dispatch_request` 的 lifecycle 分支（`server_loop.rs:381`），即持该会话 prompt 锁 `try_lock` 且全程持全局 sessions 锁。

## Constraints & risks

1. **锁顺序 + 长持锁**：`prompt_lock`（per-session）→ `sessions`（全局）；`session/load` 的存储 I/O 与**逐条回放通知**都在全局 sessions 锁内完成（`server_loop.rs:456-458`、`session_restore.rs:517-526`）。任何“回放期间允许其它请求/通知/取消介入”的方案都必须先改变这一持锁结构，而不仅是加标识。
2. **接收循环阻塞**：`session/load` 未 spawn（`server_loop.rs:31-41`），回放期间 `session/cancel` 等通知不被处理。若引入“流式恢复/中断”，需要与 `session/prompt` 同列 spawn（`server_loop.rs:32`）或改写循环。
3. **无回放代际**：可用的隔离只有 `continuation_epoch` / `mailbox.generation` / `closing` / `CancellationToken`；回放通知不带 request/generation（`session_replay.rs:81-83, 217-219`），客户端无法区分“本次 load 的回放”与并发 live 事件。load 亦不递增 `continuation_epoch`、不重建 mailbox（`session_restore.rs:207-239`）。
4. **prompt 与 load 的竞态窗口**：prompt 的锁在 spawn 之后才获取（`server_loop.rs:123-137` spawn → `prompt_dispatch.rs:135-145` 取锁），而 load 用 `try_lock`。若 load 在 prompt 取锁前抢先获取，prompt 会继续执行，并在轮次开始时读取（可能已被 load 改写/补齐的）`state.history_payloads`（`host/prompt.rs:332-347`），期间仅检查 `state.closing` 与运行时 `cancel_token`（`prompt_dispatch.rs:146-161`）。
5. **frozen 读取成本**：`load_frozen_bytes` 付一次全量 snapshot 读（含历史 payload）；legacy 冷路径上同类读取出现 2–3 次（`legacy_session.rs:95-98, 118`；`session_restore.rs:130`）。trait 无 frozen-only 入口（`session_resources.rs:594-635`）。
6. **回放失败被吞**：`replay_persisted_session_history` 出错只 `warn!` 后继续并正常响应（`session_restore.rs:525-526`），客户端不会知道历史不完整。
7. **caps 的“连接级”实现是进程级 pending 快照**（`session/caps.rs:22-37`）：多连接场景下 `set_pending_caps` 会互相覆盖；per-session 值在 `ensure_session_caps` 才被固化（幂等，`:82-98`）。
8. **新增 cap 无编译期护栏**：`PeriCaps` 的字段 / `from_client_meta` / `to_agent_meta` / `all_enabled` 四处手工同步（`peri_caps.rs`），漏一处不会编译失败。
9. `session/load` 不做 caps 门（除 `_meta` 身份与 `periReplay` 标记），因此“老客户端 + 新回放语义”不会 fail-closed；对比 rewind 是显式 fail-closed（`host/requests/rewind.rs:140-149`）。

## Open questions

1. 任务描述里“load 只需要 config”一节的前提未能从代码验证：`load_frozen_bytes` 的返回值是 frozen 装配输入（system prompt/CLAUDE.md/skills/date/beta flags），不是 `PeriConfig`；`PeriConfig` 在装配处另行定格（`host/prepared.rs:141-198`）。请确认“只需要 config”指的是哪种读取（是否希望有 `load_frozen` 轻量入口，或希望 load 阶段跳过 payload 装载）。
2. 回放竞态的目标语义未定：是“load 期间冻结其它写入”，还是“给回放打代际标记让客户端丢弃过期 chunk”？这决定是改锁结构还是加标识（当前两者都缺）。
3. `session/load` 是否允许与在途 prompt 并发（现状是 `try_lock` 直接 `-32010` 拒绝，`server_loop.rs:421-435`）？若允许，需要定义 load 与 in-flight turn 的 history 归属（谁赢 `state.history_payloads`）。
4. 回放是否需要“按 payload 类型裁剪”的既有契约（例如 `PersistedPayload::SystemReminder` 只投 `Tui` 受众；`BaseMessage::System` 一律跳过，`session_replay.rs:64, 37-41`）——新增 transcript 来源时是否沿用同一过滤规则，未在代码中找到统一权威描述。
5. `handle_metadata(history=true)` 直接对客户端暴露全量 payload 序列（`session_restore.rs:458-487`），与 `session/load` 回放是两条独立出口；是否需要统一投影规则未在本目录找到约束（可能在 `architecture-contracts.md`，本次未读）。
