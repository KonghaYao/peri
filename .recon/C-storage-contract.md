# C — 存储 + 会话资源契约现状（peri-acp-types × peri-resources）

调查范围：worktree `/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`，只读代码，未运行测试。
路径均相对工作区根；行号来自本次阅读。

## Summary

1. 业务侧唯一会话入口是 `peri-acp-types::session_resources::SessionResources`（42 个方法，`session_resources.rs:505-783`），有 5 个同族 trait：`SessionInitialization`(469-477)、`ChildResumeClaim`(484-494)、`SessionStoreShutdownPort`(800-803)；资源内部的私有 seam 是 `SessionDataPort`(peri-resources/src/sessions/data.rs:37-314) 与 `LocalExecutionPort`(peri-resources/src/sessions/local_port.rs:14-71)。
2. 生产实现只有两条后端装配：本机 SQLite（`SqliteSessionData` + `LocalExecution`）与 Turso Cloud 远端（`RemoteSessionData` + `RemoteExecution`），两者共用同一个门面 `SessionResourcesImpl`（resources.rs:257）与同一份 canonical schema（canonical.rs）。
3. 历史读取**没有任何 LIMIT/OFFSET/keyset**：两后端都是「整会话 payload 全量 + `ORDER BY rowid`」。分页只存在于 thread 列表（keyset `(updated_at, id) <`）。
4. 排序键是 `messages` 的**隐式 rowid**（canonical.rs:16-22）；`messages` 表没有 seq/ordinal/snapshot/version 列，唯一版本表达是 `PRAGMA user_version = 11`（canonical.rs:32）与远端 `peri_store_meta`（remote/schema.rs:46,53）。
5. `ThreadMeta.content_size` 是**读取时现算**的相关子查询 `SUM(LENGTH(content))`（row_mapping.rs:10-12、remote/session_sql.rs:29-34），不落列，且位于恢复热路径（`load_session_snapshot` / `load_session_meta`）；当前代码树里没有非测试消费者。
6. 远端传输由 `turso_serverless` 0.1.3 承担（SQL over HTTP /v2/pipeline，Bearer），本 crate 只在 connection.rs 接触 SDK；语句形态是「静态 `&'static str` SQL + 全绑定参数」（sql.rs:15-18），`LIMIT ?n` 绑定已在本仓库验证可用（session_sql.rs:163）。

## Findings

### 1. 完整 trait surface

#### 1.1 业务门面 `SessionResources`（peri-acp-types/src/session_resources.rs:505-783）

| # | 签名（行号） | 语义要点 |
|---|---|---|
| 1 | `fn oauth_credentials(&self) -> Option<Arc<dyn OAuthCredentialPort>>`（506-510，默认 `None`） | 可选能力 |
| 2 | `fn oauth_credentials_for_workspace(&self, WorkspaceId) -> Option<Arc<dyn OAuthCredentialPort>>`（512-517，默认 `None`） | 可选能力 |
| 3 | `async fn inspect_availability(&self, session: Option<&ThreadId>) -> SessionResourceResult<SessionAvailability>`（523-526） | 「本次打开的权限、后端能力面；给定 session 时附带该会话的执行资格」 |
| 4 | `async fn resolve_workspace(&self, cwd: &Path) -> SessionResourceResult<ResolvedWorkspace>`（529） | 解析并登记本机执行目录 |
| 5 | `async fn validate_session(&self, id, workspace: &ResolvedWorkspace) -> SessionResourceResult<()>`（532-536） | 校验绑定，不取得执行所有权 |
| 6 | `async fn finish_close(&self, id) -> SessionResourceResult<()>`（539） | 清除已完成排空的关闭意图 |
| 7 | `async fn close_settlement(&self, id) -> SessionResourceResult<CloseSettlement>`（541） | `Pending/Finished/Unknown`（347-351） |
| 8 | `async fn create_session(&self, input: &NewSession) -> SessionResourceResult<()>`（552） | meta/binding/frozen 完整保存后返回准入；已保存未准入返回 `saved_but_not_admitted` |
| 9 | `async fn abandon_initialization(&self, id) -> SessionResourceResult<()>`（560） | 撤销「本次尚未发布」的完整创建（不修改 source 会话） |
| 10 | `async fn begin_initialization(&self, draft: &NewSessionDraft) -> SessionResourceResult<Arc<dyn SessionInitialization>>`（569-572） | 两阶段创建第一阶段（frozen 暂空） |
| 11 | `async fn discard_incomplete_initialization(&self, id) -> SessionResourceResult<()>`（580） | 崩溃恢复：判据「绑定已成立 且 `frozen IS NULL`」 |
| 12 | `async fn adopt_legacy_session(&self, id, saved_cwd, workspace, frozen) -> SessionResourceResult<()>`（586-592） | legacy 接纳；不借机改绑 |
| 13 | `async fn load_session_snapshot(&self, id) -> SessionResourceResult<SessionSnapshot>`（597） | 一次一致读取：meta/binding/frozen/own payload+flags/inherited（226-235） |
| 14 | `async fn load_session_binding(&self, id) -> SessionResourceResult<BindingState>`（604） | 轻量绑定分类，不加载历史、不启动发现 |
| 15 | `async fn validate_bound_workspace(&self, id, check: BindingRecheck) -> SessionResourceResult<ResolvedWorkspace>`（609-613） | `Full`/`Recorded`（217-222） |
| 16 | `async fn load_session_history(&self, id) -> SessionResourceResult<Vec<PersistedPayload>>`（617-620） | **完整逻辑上下文**：继承区在前、自有 payload 在后；历史回放与 `metadata(history)` 的唯一读取 |
| 17 | `async fn load_session_meta(&self, id) -> SessionResourceResult<ThreadMeta>`（623） | 小型 metadata 投影，不加载历史或大快照 |
| 18 | `async fn session_environment_id(&self, id) -> SessionResourceResult<Option<String>>`（624-629，默认 `Ok(None)`） | 可选能力 |
| 19 | `async fn session_workspace_id(&self, id) -> SessionResourceResult<Option<WorkspaceId>>`（630-635，默认 `Ok(None)`） | 可选能力 |
| 20 | `async fn list_machines() -> Vec<MachineInfo>`（637-641，默认 `Unsupported`） | 远端专属 |
| 21 | `async fn list_workspaces(machine_id)`（643-650，默认 `Unsupported`） | 远端专属 |
| 22 | `async fn rename_machine(machine_id, name)`（652-656，默认 `Unsupported`） | 远端专属 |
| 23 | `async fn list_sessions(&self, query: &ScopedThreadQuery) -> SessionResourceResult<ScopedThreadPage>`（659-662） | 「按 scope/cursor/limit 分页列举；过滤在数据端完成」 |
| 24 | `async fn list_archived_sessions(&self, query)`（664-671，默认 `Unsupported`） | |
| 25 | `async fn set_session_archived(&self, id, archived: bool)`（674-682，默认 `Unsupported`） | |
| 26 | `async fn list_children(&self, parent) -> Vec<ThreadMeta>`（685） | |
| 27 | `async fn list_session_tree(&self, root) -> Vec<ThreadMeta>`（688） | 含自身 |
| 28 | `async fn append_history(&self, id, payloads: &[PersistedPayload]) -> SessionResourceResult<()>`（696-700） | 「id 已存在或批次内重复必须失败，不得静默忽略」 |
| 29 | `async fn append_reminder_if_absent(&self, id, message_id, reminder) -> SessionResourceResult<bool>`（704-709） | 同 ID 不同内容必须失败 |
| 30 | `async fn mark_session_closing(&self, id)`（712） | |
| 31 | `async fn is_session_closing(&self, id) -> bool`（715） | |
| 32 | `async fn save_fork(&self, fork: &ForkSnapshot)`（718） | source 不变 |
| 33 | `async fn save_child(&self, child: &ChildSnapshot)`（725） | 父子关系一致性必须写入前校验 |
| 34 | `async fn claim_child_resume(&self, child, root) -> Box<dyn ChildResumeClaim>`（728-732） | root 执行树内串行认领 |
| 35 | `async fn apply_compaction(&self, id, change: &CompactionChange)`（735-739） | 「摘要、flags、计数与缓存视图全部生效或全部不生效」 |
| 36 | `async fn apply_message_projections(&self, id, updates: &[(MessageId, MessageFlags)])`（742-746） | |
| 37 | `async fn rewind_history(&self, id, boundary: RewindBoundary)`（749-753） | 派生计数与缓存同步更新 |
| 38 | `async fn remove_history_entries(&self, id, ids: &[MessageId])`（756-760） | 按 id 集合精确移除 |
| 39 | `async fn update_session_meta(&self, id, patch: &SessionMetaPatch)`（763-767） | 定向更新，不整份覆盖 |
| 40 | `async fn delete_session_tree(&self, id)`（773） | |
| 41 | `async fn recover_session_persistence(&self, id) -> PersistenceRecovery`（776-779） | `Recovered/StillBlocked`（82-87） |
| 42 | `async fn drain_persistence(&self, id)`（782） | 有界等待 |

方法总数 42，其中 9 个带默认实现（1、2、18-25）。trait 级不变量（同文件 8-15、496-503）：

- 「**不提供 no-op 默认实现**…无法完成的行为必须返回明确失败，不能让调用方把「不支持」当成「没有数据」」（10-11）；但 1/2/18/19 默认 `None`/`Ok(None)` 与此张力并存（见 Constraints）。
- 「mutation 先检查访问权限与未决持久化，再落盘」；`ReadOnly` 下所有 mutation 返回 `ReadOnlyStore`；「结果不确定时返回 `persistence_uncertain`，不得重试后伪装成功」（501-503）。

配套值类型：
- `AccessMode{ReadWrite,ReadOnly}`(38-41)、`DataCapabilities{Complete,HistoryReadOnly}`(45-50)、`ExecutionAvailability{Available,BindingMissing,WorkspaceUnavailable,PersistencePending,ReadOnlyStore,Unsupported}`(57-69)、`SessionAvailability`(73-78)。
- `MutationOutcome{NotApplied,Applied,Unknown}`(280-287)，由 `SessionResourceError::new` 从 kind 派生（353-361）：`PersistenceUncertain→Unknown`、`SavedButNotAdmitted→Applied`、其余 `NotApplied`。
- `SessionResourceErrorKind`(291-330)：`InvalidInput/NotFound/Unsupported/ReadOnlyStore/Workspace/Corrupt/Unavailable/Timeout/PersistenceUncertain/SavedButNotAdmitted/Conflict/Internal`。
- 领域输入：`NewSession`(128-136)、`NewSessionDraft`(145-152，无 frozen)、`ForkSnapshot`(159-164)、`ChildSnapshot`(175-180)、`FrozenSnapshotBytes`(96-110)、`SessionMetaPatch`(262-268)。
- 读取结果：`BindingState{Bound,LegacyConfirmed,ExternalOrUnregistered,Missing}`(189-198)、`FrozenState{Present,LegacyAbsent}`(206-210)、`RewindBoundary`(244-255)。

#### 1.2 句柄 trait

- `SessionInitialization`(469-477)：`fn thread_id(&self) -> &ThreadId`；`async fn commit_frozen(&self, &FrozenSnapshotBytes) -> …`（「write-once CAS」473 与 463-465）；`async fn abandon(self: Arc<Self>)`（476，「只撤销未提交的草稿」）。
- `ChildResumeClaim`(484-494)：`mark_running` / `hand_off_to_background` / `mark_failed` / `mark_terminated`，语义「恢复到认领前的状态，不留 active 残留」（489-493）。
- `SessionStoreShutdownPort`(800-803)：`async fn shutdown(&self)`；文档要求「关闭**不可逆地**停止新写入，但「停止新写入」不等于「已关闭」」（794-798）。关闭权与业务句柄分离的装配见 peri-resources/src/context.rs:29-63（`Resources` 不可 Clone，`SessionStoreShutdownOwner::take` 是 crate 内唯一取权入口）。

#### 1.3 资源内部数据端口 `SessionDataPort`（peri-resources/src/sessions/data.rs:37-314，`pub(crate)`）

共 47 个方法（含 10 个默认实现）；与门面一一对应，多出执行环境与 child-resume 事实读写：

- 关闭/环境：`finish_close`(38)、`close_settlement`(39-42)、`oauth_credentials_for_workspace`(44-49)、`machine_id_of`(50)、`workspace_id_of`(51-56)、`list_machines`/`list_workspaces`/`rename_machine`(57-82)。
- 创建/接纳：`save_new_session`(84)、`save_new_session_in_workspace`(85-91)、`save_new_session_draft`(94-97)、`save_new_session_draft_in_workspace`(98)、`commit_frozen`(109)、`revoke_unpublished_session`(120)、`revoke_unpublished_draft`(129)、`adopt_legacy_session`(132)。
- 读取：`load_snapshot`(141)、`load_binding`(146)、`load_session_history`(149-152)、`load_meta`(155)、`session_exists`(158)、`binding_of`(164)、`binding_discovery_snapshot`(167-172)、`session_root`(177)、`list_sessions`(180-183)、`list_archived_sessions`(185-194)、`set_session_archived`(196-206)、`list_children`(209)、`list_session_tree`(212)。
- 写入：`append_history`(216-220)、`append_reminder_if_absent`(222-227)、`mark_session_closing`(229)、`is_session_closing`(230)、`save_fork`(233)、`save_fork_in_workspace`(234)、`save_child`(246)、`apply_compaction`(262)、`apply_message_projections`(269)、`rewind_history`(276)、`remove_history_entries`(283)、`update_meta`(290)、`delete_tree`(297)。
- 收敛/关闭：`recover_persistence`(304)、`drain`(310)、`close`(313)；child resume 事实 `load_child_resume_record`(249)、`store_child_resume_record`(255)。
- 辅助类型 `ChildResumeRecord{status, claimed}`(26-29)；`ensure_child_relation`(333-348)。

#### 1.4 全部实现者（生产代码）

| trait | 实现 | 位置 |
|---|---|---|
| `SessionResources` | `SessionResourcesImpl` | peri-resources/src/sessions/resources.rs:257 |
| `SessionInitialization` | `DraftInitialization` | resources.rs:64 |
| `ChildResumeClaim` | `ChildResumeClaimHandle` | resources/claim.rs:80 |
| `SessionStoreShutdownPort` | `SessionStoreShutdownOwner` | peri-resources/src/context.rs:62 |
| `SessionDataPort` | `SqliteSessionData` / `RemoteSessionData` | sqlite_store/session_data.rs:74 / remote/session_data.rs:483 |
| `LocalExecutionPort` | `LocalExecution` / `RemoteExecution` | sqlite_store/local.rs:134 / remote/execution.rs:167 |
| `ThreadStore`（迁移桥） | `SqliteThreadStore` / `FilesystemThreadStore` | sqlite_store.rs:121 / filesystem.rs:147 |
| `OAuthCredentialPort` | `SqliteOAuthCredentialStore` / `RemoteOAuthCredentials` / `LifecycleCredentials` | sqlite_store/oauth_credentials.rs:55 / remote/oauth_credentials.rs:143 / resources/oauth_credentials.rs:40 |

`ThreadStore` 本身在 peri-acp-types/src/store/mod.rs:220-464 声明为**迁移桥**：「不为新场景扩展本 trait；新行为加到 `SessionResources`」（store/mod.rs:6-15）。

### 2. 每后端的历史读取（现状 SQL / 调用）

#### 2.1 本机 SQLite（`SqliteSessionData`）

1. 自有 payload（唯一历史读取）：`SELECT message_id, content FROM messages WHERE thread_id = ?1 ORDER BY rowid`（sqlite_store/context.rs:117-136），读后复核行内 ID 与主键一致（130-132）。**无 LIMIT/OFFSET/keyset**。
2. flags：`SELECT message_id, truncated, excluded, projection FROM messages WHERE thread_id = ?1 AND (truncated = 1 OR excluded = 1 OR projection IS NOT NULL)`（sqlite_store/compaction.rs:156-166）。
3. 完整上下文 = 继承区 + 自有：`load_context_payloads_on`（context.rs:138-147）；继承快照是权威值，缺失时沿父链回溯并可用 `WHERE thread_id = ?1 AND rowid <= ?2` 截断父会话（context.rs:149-211，截断 SQL 见 251-263）。
4. `load_snapshot` 在一个 `begin`（DEFERRED）事务里做 5 次读取：meta → binding 分类 → frozen 字节 → payloads → flags → inherited，然后提交（sqlite_store/session_data.rs:323-368）。
5. `load_session_history` = 读事务内的 `load_context_payloads_on`（session_data.rs:395-413）。
6. 迁移桥 `ThreadStore::load_messages` 另有一条 `SELECT content FROM messages WHERE thread_id = ?1 ORDER BY rowid`（sqlite_store.rs:215-229）。
7. `FilesystemThreadStore`（仅 ThreadStore）：历史在 `messages.jsonl`（filesystem.rs:101-102, 233）；`content_size` 直接取文件长度（filesystem.rs:285-290）。

#### 2.2 远端 Turso（`RemoteSessionData`）

1. 消息投影：`SELECT m.message_id, m.content, m.truncated, m.excluded, m.projection FROM messages m WHERE m.thread_id = ?1 ORDER BY m.rowid ASC`（remote/session_sql.rs:132-134），语句构造 `select_messages_statement`（session_sql.rs:217-219）。**无 LIMIT/OFFSET/keyset**。
2. 读取方式：`read_snapshot` 用 `read_pair(select_session_statement, select_messages_statement)`（remote/session_read.rs:34-63，调 mutation.rs:686-698）＝一个 `consistent_read` 批（同一请求内 `BEGIN DEFERRED`…`COMMIT`，connection.rs:224-243）。
3. 行 → payload/flags：`decode_history`（session_read.rs:245-259），默认 flags 不入映射（254-255）。
4. `read_history` = 事实行的 inherited + 自有 payload（session_read.rs:81-95）。
5. 远端 meta 行读：`SELECT_META_SQL`（`FROM threads s WHERE s.id = ?1`，session_sql.rs:107-111），经 `select_meta_statement` + `fetch_row`（session_read.rs:101-105）。
6. 分页（仅 thread 列表）：`... AND (?4 = 0 OR (s.updated_at, s.id) < (?5, ?6)) ORDER BY s.updated_at DESC, s.id DESC LIMIT ?7`（session_sql.rs:153-164；`limit` = `clamp(1,200)+1`，session_sql.rs:260）。本机同形分页 `LIMIT limit+1`（sqlite_store/workspace.rs:514, 558-566）。

结论：历史读取目前**只有全量一种形态**；分页/区间能力仅存在于 thread 列表，且是 keyset（`(updated_at, id)` 严格小于）+ 有界 limit。

### 3. 存储 schema

#### 3.1 版本与形状权威

1. 一份 schema 两个执行器：`sessions::canonical` 是唯一 DDL 来源（canonical.rs:1-7），远端不再有自己的会话表（remote/mod.rs:30-38）。
2. 版本：`CURRENT_SCHEMA_VERSION: i64 = 11`（canonical.rs:32）；本机 `PRAGMA user_version`（sqlite_store/schema.rs:49-53，`0` 走 legacy 列检查 74-111）；远端 `peri_store_meta.schema_version` + contract `"peri.session.store/v5"`（remote/schema.rs:46, 53, 68-84）。
3. 形状判定是声明式的：`schema_shape.rs` 逐表列序列/NOT NULL/主键（表声明见 134-180；`MESSAGES_COLUMNS` 107-115）；不比对索引与类型文本（schema_shape.rs:24-27）。

#### 3.2 表与列（当前形状）

1. `messages`（canonical.rs:230-234）：
   `message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE, role TEXT NOT NULL, content TEXT NOT NULL, truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT`。
   声明：`not_null = ["thread_id","role","content","truncated","excluded"]`，`primary_key = ["message_id"]`（schema_shape.rs:175-180）。
2. `threads`（canonical.rs:187-195）：`id, title, cwd, created_at, updated_at, message_count, parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config, frozen_context, inherited_context, agent_status, workspace_id NOT NULL REFERENCES workspaces(id), archived`。
   - `snapshot_at_message_id` 是 child 继承截止点（context.rs:180-185），不是历史快照序号。
   - `message_count` 是**存储列**且每次写入后重数（见 3.4）。
3. 其它表：`machines`(157-161)、`projects`(170-173)、`workspaces`(176-185)、`session_bindings`(198-204，含 `schema_version`/`discovery_snapshot`/`evidence_origin`)、`mcp_oauth_credentials`(205-213，按 `workspace_id`)、`session_close_intents`(241-245)。
4. 索引（canonical.rs:247-253）：`idx_messages_thread_id ON messages(thread_id)`；`idx_bindings_project`；`idx_bindings_workspace`；`idx_threads_updated`；`idx_threads_workspace_archived`。**没有** `(thread_id, rowid)` 或 flags 相关索引。
5. 远端机制表（不进 canonical）：`peri_store_meta`（单行，schema_version/store_id/contract/created_at，remote/schema.rs:75-84）与 `peri_op_ledger`（operation_id/kind/digest/state/receipt/updated_at，remote/ledger.rs:23-48）。

#### 3.3 排序键 / flags / 版本列

1. 排序键 = **插入顺序 = 隐式 `rowid`**：「读取与 rewind 一律显式 `ORDER BY rowid` / `WHERE rowid > ?`」；远端旧的显式 `ordinal` 列已随统一删除（canonical.rs:16-22）。`rowid` 未被投影进任何跨边界类型。
2. 没有 seq/snapshot/version 列：`messages` 全表无序号列；`revision` 只出现在「压缩前形状」的删除对象允许列表里（schema_shape.rs:293-301）。
3. flags 存储在消息行上：`truncated`/`excluded` 布尔列 + `projection` JSON 文本列（canonical.rs:232-233）；读取时「默认 flags 不入映射」（compaction.rs:152-166、session_read.rs:245-259）。`MessageFlags` 定义在 store/mod.rs:210-218（`projection` 字段 `#[serde(default, skip_serializing_if)]`）。
4. 行内容 = 版本化 JSON envelope：`PERSISTED_PAYLOAD_VERSION: u16 = 1`（store/mod.rs:33-34），写出 `{version, type, ...}`（100-112），读取兼容未包裹的 legacy `BaseMessage`（114-136）。
5. 继承区是 `threads.inherited_context` 的独立 JSON envelope（`version: 1`，store/mod.rs:145-187）。

#### 3.4 compaction 批次如何写

1. 本机（一个 `BEGIN IMMEDIATE` 事务；调用点 `apply_compaction` session_data.rs:729-749）：
   - 逐条 `UPDATE messages SET truncated, excluded, projection WHERE message_id = ?4 AND thread_id = ?5`，并 `ensure!(rows_affected == 1)`（compaction.rs:88-112）；
   - 逐条 `INSERT INTO messages (message_id, thread_id, role, content)`（114-127）；
   - `UPDATE threads SET updated_at = ?, message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?)`（129-139）。
2. 远端（一次端口调用 = 一个托管事务批 + 幂等账本）：`write_compaction_change` 先前置一致读 `missing_session_messages`（不满足则 `Corrupt`），再组 effects = [会话存在守卫, 逐条 flag 更新, 逐条追加, 计数重数]，交 `commit_effects`（remote/session_history.rs:236-287 与 session_write.rs:381-397；守卫常量 45-59）；操作 id 由每次调用铸造（session_write.rs:400-407）。
3. 派生规则：`message_count` 重数落列；`content_size` **不落列**，「读取时由投影现算」（remote/session_history.rs:20-22）。

### 4. 现有区间/定向查询

| 维度 | 现存 | 位置 |
|---|---|---|
| 单条 by id | `WHERE thread_id = ?1 AND message_id = ?2`（取 rowid） | sqlite history.rs:97-103；remote session_history.rs:96-98 |
| 单条 by 全局 id | `SELECT thread_id FROM messages WHERE message_id = ?1`（判归属） | remote session_history.rs:96；sqlite history.rs:150-155 |
| id 集合 | 本机逐条循环查/删；远端每 id 两条语句（foreign 守卫 + DELETE） | sqlite history.rs:137-175；remote session_history.rs:351-371 |
| by seq | **不存在**（无 seq 列） | — |
| by flags | 仅本机有一次线程内 flags 过滤 `(truncated = 1 OR excluded = 1 OR projection IS NOT NULL)` | compaction.rs:160-166 |
| by payload kind | **不存在**（`role` 列只写不筛；取值见 canonical.rs:315-329：user/assistant/system/tool/system_reminder） | — |
| 前缀 by rowid | `WHERE thread_id = ?1 AND rowid <= ?2 ORDER BY rowid`（继承截止点） | sqlite context.rs:251-263 |
| 范围删除 | `rowid > ?` / `rowid >= ?` | sqlite history.rs:106-113；remote session_history.rs:88-89 |
| thread 列表 keyset | `(t.updated_at, t.id) < (?, ?) ORDER BY … LIMIT ?` | sqlite workspace.rs:558-566；remote session_sql.rs:162-163 |

### 5. history-byte-size 聚合的位置与热路径

1. 计算方式：**读取时现算的相关子查询**，两后端逐字相同——
   - 本机：`(SELECT COALESCE(SUM(LENGTH(m.content)), 0) FROM messages m WHERE m.thread_id = t.id) as content_size`（sqlite_store/row_mapping.rs:10-12），列下标 6（row_mapping.rs:42, 51, 64）；
   - 远端：`session_sql.rs` 的 `meta_columns!` 宏内同一子查询（remote/session_sql.rs:29-34），下标 `META_CONTENT_SIZE = 6`（72）。
2. 存储：**没有列**。`threads` DDL 无该列（canonical.rs:187-195），形状声明也无（schema_shape.rs:89-106）；仅 `ThreadMeta.content_size` 作为视图字段存在（peri-acp-types/src/thread/types.rs:158-160，注释写「由 list_threads 查询时计算」——与现状漂移）。
3. 覆盖的读取路径（每次 meta 投影都会跑这条子查询）：
   - `load_meta`：本机 context.rs:104-109、sqlite_store.rs:287-293；远端 session_read.rs:101-105；
   - `load_snapshot`（→ `load_meta_on`）：session_data.rs:336；
   - `load_binding`：session_data.rs:381；
   - `list_children` / `list_session_tree`：context.rs:61 / 78；
   - 迁移桥 `list_threads`：sqlite_store.rs:403-408；
   - 远端分页投影（`page_columns = meta_columns + binding`）：session_sql.rs:56-61、153-164。
   - 反例（不跑该聚合）：本机 `list_scoped_threads` 的投影是轻量的 `t.id, t.title, t.message_count, t.updated_at, …`（workspace.rs:515-517）。
4. 是否在恢复热路径：**是**。ACP `session/load|resume` 第一跳 `load_frozen_bytes` → `load_session_snapshot`（peri-acp/src/host/requests/session_restore.rs:47-64），随后身份/上下文装配 `session_context_payload` → `load_session_meta`（session_restore.rs:344-358），`session/metadata` 也走 `load_session_meta`（437-441）。两条都经过第 3 点的子查询，因此「恢复一个会话」会对该会话全部消息做一次 `SUM(LENGTH(content))`。
5. 消费者：非测试代码里没有任何读取 `ThreadMeta.content_size` 的地方（唯二写入/解码点：filesystem.rs:290 赋值、remote/session_codec.rs:95-107 解码）。

### 6. RewindBoundary 的存储实现

1. 类型：`RewindBoundary::KeepThrough(MessageId) | RemoveFrom(MessageId)` + `message_id()`（session_resources.rs:244-255）；语义「transcript rewind 保留目标本身，ACP 用户 rewind 移除目标及以后；两者语义不可合并」（241-243）。
2. 本机：`rewind_stored_history`（sqlite_store/session_data/history.rs:83-126）——
   `SELECT rowid FROM messages WHERE thread_id = ?1 AND message_id = ?2`；**目标不存在 → 保持无变更语义并成功**（104-105 注释 + 代码）；命中则按边界执行 `DELETE … AND rowid > ?2` / `rowid >= ?2`（106-119），随后 `refresh_history_derivations`（120；定义 session_data_helpers.rs:218-234，重数 `message_count` + 推进 `updated_at`），全程单事务 `BEGIN IMMEDIATE`（90-95），提交失败→`commit_failure`（122-124）。
3. 远端：`write_rewind`（remote/session_history.rs:294-325）——先 `boundary_rowid`（缺失即 `Ok(())`，300-302），再用常量 SQL（88-89）组成 effects + refresh，走 `commit_effects` 托管批；**方向进摘要**，避免 `KeepThrough`/`RemoveFrom` 撞同一操作 id（314-321）。
4. 调用方：transcript rewind 用 `KeepThrough`（peri-agent/src/session/transcript/persistence.rs:321）；ACP 用户 rewind 用 `RemoveFrom`（peri-acp/src/host/requests/rewind.rs:60；另一条路径 peri-acp/src/session/command/rewind.rs:195-200 走 `remove_history_entries`）。
5. 纯变换规则另有一份可复用实现：`rewind_keep_len` / `apply_rewind`（peri-acp-types/src/store/history.rs:131-160）。

### 7. 远端后端传输与「范围查询」表达方式

1. 传输归属：`RemoteTransport` trait 是「本 crate 唯一真正调用 SDK 语句入口的地方」（connection.rs:152-185）：
   - `async fn sql_values(&self, spec: &StatementSpec) -> turso_serverless::Result<Vec<Vec<Value>>>`（163）
   - `async fn managed_batch(&self, statements: Vec<StatementSpec>) -> …<Vec<u64>>`（166-169，`BEGIN IMMEDIATE`，connection.rs:209-222）
   - `async fn consistent_read(&self, statements) -> …<Vec<Vec<Vec<Value>>>>`（172-175，`BEGIN DEFERRED`，224-243）
   - `fn is_autocommit()`（178）、`async fn close()`（184，文档说明 SDK 吞远端关闭错误 138-141）。
2. 协议与依赖：`turso_serverless = "0.1.3"`（peri-resources/Cargo.toml:17）；只读探针记录「`POST /v2/pipeline` 返回 200 且 `results[0].type = ok`：SQL over HTTP 可用、Bearer 认证通过」，参数绑定回环含 64 位整数与 NULL（remote/mod.rs:8-20）；超时由本层 20s 预算兜住（`REQUEST_BUDGET`，connection.rs:26-40）。
3. 语句形态（范围查询的唯一合法表达方式）：`struct StatementSpec { sql: &'static str, params: Vec<Value> }`（remote/sql.rs:15-18），规则「静态 SQL + 全部值走绑定参数；动态内容只能出现在 `params` 里；表名等无法绑定的位置不得由外部输入拼出」（sql.rs:1-5）。`is_read_only()` 只用于离线断言读路径（sql.rs:42-47）。
4. 现有绑定式 LIMIT 的先例：`… LIMIT ?7`（session_sql.rs:163），参数在 `page_statement_for_archive` 组装（session_sql.rs:260-277）。因此「带绑定 limit 的区间读」在语法层已被本仓库验证可行；OFFSET 无先例。
5. 写入形态（若范围查询之后还要配套删除/裁剪）：一次端口调用 = 一个托管批 + `peri_op_ledger` 幂等资格（session_history.rs:1-17；ledger.rs:39-48），批内守卫用 `peri_store_meta` 单行表的 `INSERT … SELECT WHERE …` 制造主键冲突来整批回滚（session_history.rs:44-59）。
6. 请求预算内的失败分类：读批失败不判「未生效」，直接按分类上报（mutation.rs:764-774）；回复结果集数量不符是错误（mutation.rs:671-679）。

### 8. 跨 ACP 边界的类型与版本/能力约定

1. `SessionResources` 及其输入/结果类型是**进程内** Rust trait 面，不是线协议类型；存储错误也不跨边界：ACP 侧 downcast 后转成 `AcpError`（peri-acp/src/host/requests/session_lifecycle.rs:35-38；session_restore.rs:88-93）。
2. 真正 serde 的类型（可上线）：
   - `ThreadMeta`（`#[derive(Debug, Clone, Serialize, Deserialize)]`，thread/types.rs:148-179，多个字段带 `#[serde(default)]`）、`ThreadListEntry`（126-133）。
   - `SessionBinding`/`WorkspaceId`/`MachineInfo`/`WorkspaceInfo`/`ThreadScope`/`ThreadListCursor`/`ScopedThreadQuery`/`ScopedThreadEntry`/`ScopedThreadPage`（workspace.rs:45-163，分页字段 `scope/cursor/limit` 143-147）。
   - `MessageFlags`（store/mod.rs:210-218）与 `MessageId`/`BaseMessage`（messages/message.rs:4-5）。
3. `PersistedPayload` 本身不是 serde：它以**版本化 envelope** 落库与上线，`{version: 1, type, …}`（store/mod.rs:33-136）；ACP 的 `session/metadata(history=true)` 就是把它重新编码成 JSON 数组返回（peri-acp/src/host/requests/session_restore.rs:465-474）。
4. 能力协商约定：`PeriCaps` 从 `InitializeRequest.clientCapabilities._meta` 读 `peri.*` 布尔键，「每个 flag 默认为 false」（peri_caps.rs:5-9、75-96）；会话/工作区相关的是 `session_workspace_v1`（peri_caps.rs:63-65，键 `peri.sessionWorkspaceV1`，95）。
5. fail-closed 用法：`session/context` 未协商即 `-32601`（session_restore.rs:385-394）；archive RPC 未协商即 `-32602`（peri-acp/src/host/requests/storage_v2.rs:41-47）；身份/上下文响应体自带 `"version": 1` 与 `_meta["peri.sessionWorkspaceV1"]`（session_restore.rs:304-312、376-378、406）。
6. 另有一条更早的版本化出口：`PeriCaps.rewind` 门控「Peri 会话回退 RPC（candidates/preview/execute）」，未声明时「三个非标准 RPC 都必须 fail closed」（peri_caps.rs:51-56）。

## Constraints & risks

1. **任何历史区间/分页读都是新契约**：门面与 `SessionDataPort` 都没有 range 参数（session_resources.rs:617-620；data.rs:149-152），两后端 SQL 也无 LIMIT（context.rs:122；session_sql.rs:132-134）。新增就必须同时改：acp-types 契约、两个 adapter、remote 的静态 SQL 常量与投影下标（session_sql.rs:63-92 的常量与测试按列校验）。
2. **没有可移植的序号**：顺序载体是引擎 `rowid`（canonical.rs:16-22），它不出现在任何返回类型里；跨边界唯一身份是 `MessageId`。因此 keyset 游标要么用 `MessageId`（每次现解析 rowid），要么新增 seq 列——后者是表结构变更，按仓库规则需用户明确批准（根 CLAUDE.md 原则 12）。
3. **索引仅 `messages(thread_id)`**（canonical.rs:248）：线程内 `ORDER BY rowid` 与 `rowid > ?` 边界删除目前没有额外索引支撑；范围读的代价特征需要在两端实测（本报告未做性能测量）。
4. **`content_size` 是恢复热路径上的无消费者计算**：`SUM(LENGTH(content))` 内联在 meta 投影（row_mapping.rs:11；remote/session_sql.rs:32），`load_session_snapshot`/`load_session_meta` 都会执行（session_data.rs:336、resources.rs:442-444）；非测试代码无读取方。`ThreadMeta` 的字段注释（types.rs:158）与现状不一致。删除/保留都触及 serde 契约。
5. **默认实现与「不支持 ≠ 没有数据」的张力**：门面 18/19 号方法（`session_environment_id`/`session_workspace_id`）默认 `Ok(None)`（session_resources.rs:624-635），调用方无法区分「不支持」与「确实没有」；新增范围读能力若沿用默认实现风格，会复制同一问题。
6. **迁移桥内已有 no-op 默认实现**：`ThreadStore::delete_messages_since` 默认返回 `Ok(()) // 默认 no-op`（store/mod.rs:456-463），`load_message_flags` 默认返回空 map（445-450）；模块文档只约束「不新增」这类默认实现（11-12）。新能力若走旧桥，会立即踩到「不支持 == 没有数据」。
7. **`DataCapabilities` 只有两档**（Complete/HistoryReadOnly，session_resources.rs:45-50），没有「支持分页读」这类按行为的能力位；远端只读打开的写入口返回 `ReadOnlyStore`（remote/mutation.rs:53-60），而 `Unsupported` 被文档收敛为「只读打开未初始化的 store / 不认识的 store schema / 只读打开还没有会话表」三处（remote/mod.rs:93-95）。
8. **远端 SQL 不能在运行时拼**：`sql: &'static str`（remote/sql.rs:16），范围读必须是新常量；且投影改动会连带解码下标（session_sql.rs:28-92 注释「解码用的列下标与投影同处一个模块」）。
9. **远端「回复不完整」按错误处理**（session_read.rs:1-6；mutation.rs:671-679）：分页读如果依赖结果集数量假设，必须沿用同一 fail-closed 语义。
10. **远端写路径有账本与守卫成本**：若区间读之后伴随裁剪写入，成本是「1 托管批 + 2 条账本语句 + 每条目标 1 守卫」（session_history.rs:44-59、351-371）。
11. **迁移桥 `ThreadStore` 不应扩展**（store/mod.rs:10-11），任何新读取能力要落在 `SessionResources`/`SessionDataPort`。

## Open questions

1. 目标是**消费侧范围读**（例如 transcript 历史分页经 `SessionResources`），还是**存储内部 seam**（`SessionDataPort`）？现有 API 没有任何方向/区间参数，需要先定契约落点。
2. 游标身份用什么：`MessageId`（可跨边界、但要现解析 rowid）还是新增单调 seq 列（需用户批准表结构）？
3. `content_size` 的去留：既无消费者又在恢复热路径上，是否本次一并移除或延迟到首次真正需要时再算？
4. ACP 边界是否新增能力位（仿 `session_workspace_v1` / `rewind`），未协商客户端应 fail closed 还是收到全量回退？
5. 区间读的语义边界：允许「继承区 + 自有 payload」被分段返回吗？现有 `load_session_history` 明确是「完整逻辑上下文」（session_resources.rs:615-620），分页会打破该后置条件。
6. 远端是否接受 `LIMIT/OFFSET` 形式（OFFSET 无先例），还是必须 keyset（`rowid > ?`，which 需要把 rowid 暴露给调用方或由适配器在服务端解游标）？
7. `metadata(history=true)` 的响应形状是否允许分页参数，若允许，版本号/能力位如何表达（当前只有 `"version": 1"` 与 `_meta` 键）？
