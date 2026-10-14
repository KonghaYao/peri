# A-core-transcript — 核心 Transcript 与 Full Compact 现状基线

范围：`peri-agent` 的 `MessageTranscript`（`session/transcript.rs` + `transcript/persistence.rs`）、`agent/compact_v2/*`、`agent/stages/compact*.rs`、`session/exec/compact_pipeline.rs`、`agent/stages/receive.rs`、`agent/async_tasks/handoff.rs`。
HEAD：`7f0de56e8`。所有行号基于此 commit。

## Summary

1. Transcript 是**只追加数组 + id→下标索引 + 旁路 flags 表**：历史增长直接体现为 `entries`/`id_index`/`flags` 三个容器的线性增长；删除只发生在 `rebuild`（生产无调用者）与 `rewind_to`（生产无调用者，仅测试）。
2. Compact 不删消息，只改标记：Micro/Smart 写 `flags.projection`（并 `truncated=true`），Full 把 owner 区全部非 System 条目 `excluded=true` 后追加一条 Human 摘要；**`set_excluded` 在生产代码中没有任何调用者**。
3. Full Compact 的持久化提交点唯一：`MessageTranscript::commit_compaction_lifecycle` 内的 `store.apply_compaction(...)`（`transcript.rs:764`）；内存更新在其后（`transcript.rs:769`），二者之间由 `CompactionCommitState` 的 `pending/committed` 三态表达「结果不确定」。
4. 三处按**旧 Vec 长度/旧下标**推理的代码是 shrink 重构的直接爆点：`compact_progress.rs:114` 的位置切片、`compact_progress.rs:111` 的 `end < full_end`、以及 `compact.rs:193` 采集的 `before_entries_len` 语义。
5. 交付去重同时查热 transcript（`receive.rs:113`）与持久化事实（`append_reminder_if_absent` → sqlite `messages.message_id` 查重，`sqlite_store/session_data/async_task.rs:29-45`）。

## Findings

### 1. MessageTranscript：字段、增长面、边界与全部公开方法（Q1）

**结构体定义** `peri-agent/src/session/transcript.rs:202-231`：

```rust
pub struct MessageTranscript {
    entries: Vec<TranscriptEntry>,        // 204
    id_index: HashMap<MessageId, usize>,  // 206
    ancestor_len: usize,                  // 208
    staged: Option<StagedData>,           // 210
    flags: HashMap<MessageId, MessageFlags>, // 212
    persist_tx: Option<Arc<tokio::sync::mpsc::UnboundedSender<PersistOp>>>, // 214
    persist_handle: Option<tokio::task::AbortHandle>,  // 216
    thread_id: Option<ThreadId>,          // 218
    full_compaction_committed: bool,      // 223
    compaction_commit_state: CompactionCommitState, // 224
    session_resources: Option<Arc<dyn SessionResources>>, // 228
    budget: Option<Arc<PersistenceBudget>>, // 230
}
```

`TranscriptEntry` 是 `Message(BaseMessage)` / `Reminder { id, reminder }` 二选一枚举（`transcript.rs:67-73`）；`BaseMessage` 本体在 `entries` 内是**owned 克隆**，`visible_snapshot()` 也逐条深拷贝（`transcript.rs:477-479`）。

**随历史增长的字段**：

| 字段 | 增长规律 | 关键位置 |
|---|---|---|
| `entries: Vec<TranscriptEntry>` | 每次 `append`/`append_batch`/`commit_staged`/追加摘要 +1..n；仅 `rewind_to` 截断、`rebuild` 整体替换 | push：`transcript.rs:560`、`573`、`626-635`、`783`；truncate：`854` |
| `id_index: HashMap<MessageId, usize>` | 与 entries 同步 insert/remove，一条消息一项；rewind 后逐条 `remove` | `285`、`559`、`624`、`633`、`782`、`858` |
| `flags: HashMap<MessageId, MessageFlags>` | 只对「被标记过的 id」插入，但 `set_truncated`/`set_excluded` 用 `entry().or_default()`，即使写 `false` 也会插入默认项 | `667`、`677`；批量路径只插非默认（`711-713`）；Full 会把 owner 区**每条**非 System 条目写进 `apply_flag_updates`（`777`） |
| `Vec` 容量 | `rebuild` 用 `Vec::with_capacity(entries.len())` 精确分配（`794`）；`rewind_to` 只 `truncate`，**不释放容量**（`854`） | — |
| `staged: Option<StagedData>` | 有界：单个 AI 消息 + 其 ToolResult 列表（`154-157`、`595-609`） | — |

**ancestor 机制**：`ancestor_len` 把 `entries[..ancestor_len]` 定义为只读祖先区（`207-208`）。
- 设置入口只有两个：`with_ancestor`（`315-324`，`ancestor_len = len`）、`with_ancestor_payloads`（`304-310`，push 完后 `ancestor_len = entries.len()`）。
- 普通会话走 `with_own_payloads`（`293-298`），**不移动** `ancestor_len`（注释 `290-292`：普通 session 跨 turn 历史仍属当前 Agent，Full Compact 必须能排除它）。
- 三层保护：flag 变更守卫 `can_update_own_flags`（`651-660`，`index >= ancestor_len`）、`commit_compaction_lifecycle` 校验（`741-745`）、`rewind_to` 拒绝回退进祖先区（`839-845`）。

**staged 两阶段写入**：`stage_ai_message` → `stage_tool_result` → `commit_staged`（`595-637`），提交顺序 AI → ToolResults，逐条触发持久化；`discard_staged`（`640-642`）丢弃。

**excluded / truncated 标记的设置与变更点**（全部在这几处）：
- `set_truncated(id, bool)` `663-670`
- `set_excluded(id, bool)` `673-680`（**生产代码零调用者**：全库仅 `transcript_test.rs` 调用，见 Q7）
- `set_flags_projection(id, directive)` `687-695` → 走 `history::flags_with_projection`（`peri-acp-types/src/store/history.rs:88-96`）：设 `truncated = true` 且保留既有 `excluded`
- `clear_flags(id)` `698-704`
- `set_flags_batch(batch)` `709-715`（仅非默认，不持久化）
- Full Compact 的实际路径：`apply_compaction_lifecycle_memory` → `history::apply_flag_updates`（`777`；规则见 `history.rs:101-112`，默认值即删除）

**全部公开方法签名**（`impl MessageTranscript`，`transcript.rs:259-974`）：

```rust
pub fn new() -> Self                                                          // 261
pub fn with_own_payloads(mut self, payloads: Vec<PersistedPayload>) -> Self    // 293
pub fn with_ancestor_payloads(mut self, payloads: Vec<PersistedPayload>) -> Self // 304
pub fn with_ancestor(mut self, messages: Vec<BaseMessage>) -> Self             // 315
pub fn with_persistence(self, store: Arc<dyn SessionResources>, thread_id: ThreadId) -> Self // 332
pub fn with_persistence_budget(self, store: Arc<dyn SessionResources>, thread_id: ThreadId, budget: Arc<PersistenceBudget>) -> Self // 338
pub fn persistence_failure(&self) -> Option<String>                            // 369
pub fn has_persistence_failure(&self) -> bool                                  // 374
pub fn persistence_budget(&self) -> Option<Arc<PersistenceBudget>>             // 379
pub fn entries(&self) -> &[TranscriptEntry]                                    // 386
pub fn persisted_payloads(&self) -> Vec<PersistedPayload>                      // 391
pub fn visible_messages(&self) -> Vec<&BaseMessage>                            // 405
pub fn visible_model_messages(&self) -> anyhow::Result<Vec<BaseMessage>>       // 417
pub fn append_system_reminder(&mut self, reminder: TrustedSystemReminder) -> MessageId // 426
pub fn mirror_committed_reminder(&mut self, id: MessageId, reminder: TrustedSystemReminder) -> bool // 439
pub fn mirror_committed_payload(&mut self, payload: PersistedPayload) -> bool  // 454
pub fn idempotent_reminder_port(&self) -> Option<IdempotentReminderPort>       // 462
pub fn visible_snapshot(&self) -> Arc<Vec<BaseMessage>>                        // 480
pub fn with_compaction_commit_state(mut self, state: CompactionCommitState) -> Self // 497
pub fn compaction_commit_state(&self) -> CompactionCommitState                 // 502
pub fn full_compaction_committed(&self) -> bool                                // 507
pub fn mark_full_compaction_committed(&mut self)                               // 512
pub fn get(&self, id: MessageId) -> Option<&TranscriptEntry>                   // 517
pub fn flags(&self, id: MessageId) -> MessageFlags                             // 522
pub fn get_flags(&self, id: MessageId) -> Option<MessageFlags>                 // 530
pub fn ancestor_len(&self) -> usize                                            // 536
pub fn len(&self) -> usize                                                     // 541
pub fn is_empty(&self) -> bool                                                 // 546
pub fn append(&mut self, message: BaseMessage) -> MessageId                    // 556
pub fn append_batch(&mut self, messages: Vec<BaseMessage>) -> Vec<MessageId>   // 567
pub fn replace_by_id(&mut self, message: BaseMessage)                          // 584
pub fn stage_ai_message(&mut self, ai_message: BaseMessage)                    // 595
pub fn stage_tool_result(&mut self, tool_result: BaseMessage)                  // 605
pub fn commit_staged(&mut self)                                                // 615
pub fn discard_staged(&mut self)                                               // 640
pub fn has_staged(&self) -> bool                                               // 645
pub fn set_truncated(&mut self, id: MessageId, value: bool)                    // 663
pub fn set_excluded(&mut self, id: MessageId, value: bool)                     // 673
pub fn set_flags_projection(&mut self, id: MessageId, directive: MessageProjectionDirective) // 687
pub fn clear_flags(&mut self, id: MessageId)                                   // 698
pub fn set_flags_batch(&mut self, batch: std::collections::HashMap<MessageId, MessageFlags>) // 709
pub async fn commit_compaction_lifecycle(&mut self, lifecycle: crate::thread::CompactionChange) -> anyhow::Result<()> // 720
pub fn rebuild(mut self, entries: Vec<(BaseMessage, MessageFlags)>) -> Self    // 793
pub fn rewind_to(&mut self, id: MessageId) -> Result<(), anyhow::Error>        // 830
pub async fn flush_persistence(&self) -> anyhow::Result<()>                    // 876
pub fn persist_tx_handle(&self) -> Option<Arc<tokio::sync::mpsc::UnboundedSender<PersistOp>>> // 889
pub async fn flush_via_tx(tx: &tokio::sync::mpsc::UnboundedSender<PersistOp>) -> anyhow::Result<()> // 894
pub fn shutdown_persistence(&self)                                             // 966
```

附带公开项：`pub const DEFAULT_PENDING_MAX_ITEMS: usize = 1024`（`35`）、`DEFAULT_PENDING_MAX_BYTES = 16*1024*1024`（`38`）、`pub type IdempotentReminderPort = (Arc<dyn SessionResources>, ThreadId, Option<Arc<UnboundedSender<PersistOp>>>)`（`253-257`）、`pub enum PersistOp`（`166-192`）、`pub struct StagedData`（`154-157`）、`impl Default`（`246-250`）、`impl Drop`（`976-980`，Drop 只发 Shutdown 不 await）。
`CompactionCommitState` 公开方法：`is_uncertain()`（`46`）、`has_committed()`（`50`）；`mark_pending`/`mark_committed` 是私有（`54`、`58`）。

### 2. Effective（有效视图）vs Full（全量历史）（Q2）

**全量历史出口**：
- `entries() -> &[TranscriptEntry]`（`386-388`）：含 excluded、含 Reminder，按时间线原始顺序。
- `persisted_payloads() -> Vec<PersistedPayload>`（`391-402`）：**对全部 entries 逐条 clone**，只做判别式映射 `Message(m) → PersistedPayload::Message(m)`、`Reminder{id,r} → PersistedPayload::SystemReminder{id,r}`；**不过滤 excluded、不含 staged、不含 flags**。返回 owned Vec，调用方拿到的就是当时的内存快照。
- `len()` / `is_empty()`（`541-548`）：基于 `entries.len()`，即全量长度。

**有效（排除 excluded + 出口过滤）出口**：
- `visible_messages() -> Vec<&BaseMessage>`（`405-411`）：过滤 `flags.excluded`，并 `filter_map(as_message)` 丢掉 Reminder（借用视图，调用方需自行 clone）。
- `visible_model_messages() -> anyhow::Result<Vec<BaseMessage>>`（`417-424`）：排除 excluded + 按 `ReminderAudience::Model` 判定；Reminder 在此处才编码（`project_message`，`118-144`）。
- `visible_snapshot() -> Arc<Vec<BaseMessage>>`（`480-495`）：TUI 出口，排除 excluded + `ReminderAudience::Tui`，逐条深拷贝后包 Arc。
- `history::apply_flag_updates` 的写入侧语义：写回默认 `MessageFlags` 等于「从 flags 表删除」（`history.rs:101-112`），即「恢复可见」的唯一手段是移除标记。

**读取的权威视图函数**（不是 transcript 方法，但决定模型实际看到什么）：`projection::render_persisted_llm_view`（`peri-agent/src/agent/compact_v2/projection.rs:289-304`）——有合法 directive 时在 `visible_messages()` 之上投影，否则回落 `visible_model_messages()`，最后统一 `repair_model_tool_pairing`。Reason（`stages/reason.rs:80`）、Full Compact（`compact_v2/full.rs:79`）、上下文压力估算（`stages/context_pressure.rs:9`）三处共用它。

### 3. Full Compact 提交序列（Q3）

`full_compact_inner`（`compact_v2/full.rs:70-154`）的确切顺序：

1. **冻结输入视图**：`render_persisted_llm_view(transcript, AgentModelBridge::projection_capabilities(llm))` → `visible`，`before_visible_len = visible.len()`（`79-83`）。
2. **冻结本次快照的 own ID 集合**：遍历 `entries().skip(ancestor_len())`，过滤 `!flags.excluded`、过滤非 `System`，映射成 `MessageFlags { excluded: true, ..default }`（`86-101`）。注释明确「await 期间到达 inbox 的新结果不属于本次摘要」（`84-85`）——即 ID 集合与摘要输入在 LLM 调用前固定。
3. `affected_count = flag_updates.len()`（`102`）；`has_history` 判定（`104-107`）。
4. **摘要生成**（可长耗时、可被取消）：`SummaryBudget::resolve` + `complete_summary`（`108-115`；主循环 `290-384`），只有全部校验通过的非空摘要才返回 `Ok(text)`。
5. **原子提交**：`transcript.commit_compaction_lifecycle(CompactionChange { flag_updates, appended_messages: vec![build_summary_message(&summary)] }).await?`（`121-126`）。
6. **内存成功标记**：`transcript.mark_full_compaction_committed()`（`127`，仅写 `full_compaction_committed = true`，`transcript.rs:512-514`）。
7. 统计与返回：`after_visible` 计数（`129-133`）、构造 `CompactResult { outcome: CompactOutcome::FullApplied, .. }`（`140-153`）。

`commit_compaction_lifecycle`（`transcript.rs:720-773`）内部序列：

| 步骤 | 行 | 说明 |
|---|---|---|
| 拒绝不确定态重提交 | `724-728` | `is_uncertain()` → `Err("compact persistence outcome is unknown; reload the session")` |
| 要求持久化绑定 | `730-733` | 无 store/thread 直接失败 |
| flag 目标校验 | `735-746` | 目标 id 必须存在于 `id_index` 且 `index >= ancestor_len` |
| 追加 id 唯一性校验 | `750-754` | `history::ensure_distinct_ids`，`is_known = id_index.contains_key` |
| **标记 pending** | `762` | `mark_pending()` → 状态 1（此后任何返回错误都留下「不确定」） |
| flush 既有积压 | `763` | `self.flush_persistence().await?`（Barrier 语义，`876-903`） |
| **durable commit 点** | `764` | `store.apply_compaction(&thread_id, &lifecycle).await` — 唯一落盘点 |
| **内存更新点** | `769` | `apply_compaction_lifecycle_memory(&lifecycle)`：`apply_flag_updates` 改 flags（`777`）+ 追加消息进 entries/id_index（`779-784`） |
| 标记 committed | `770` | `mark_committed()` → 状态 2 |

`CompactionCommitState` 的角色（`transcript.rs:42-61`）：`Arc<AtomicU8>`，0=未动/1=pending(uncertain)/2=committed，可跨任务 clone。
- 在命令路径上由 interceptor 建在 `DependencyBag` 里（`executor_helpers/intercept.rs:195-200`），再由 `/compact` pipeline 取回注入临时 transcript（`session/exec/compact_pipeline.rs:77-80`、`178`）。
- interceptor 在命令返回后读它决定语义：`is_uncertain()` → `persistence_inconsistent = true`（`intercept.rs:258`）并让 host 驱逐会话（`peri-acp/src/host/prompt.rs:595-597`：`sessions.remove(session_id)`）；`has_committed()` → 从持久化重载 canonical context 并置 `history_replaced_by_compaction = true`（`intercept.rs:261-278`）。
- 注释解释动机：命令 future 被取消后 transcript 被 drop，但持久化结果必须仍可判定（`transcript.rs:40-41`、`756-761`）。

**发布（publish）**：自动路径 emit `ObserveEvent::MessagesCompacted`（`stages/compact.rs:411-437`），失败/无变更 emit `CompactEnded`（`438-448`）；手动路径 emit `CompactStarted`/`CompactCompleted`（`compact_pipeline.rs:222`、`259-271`）。

### 4. Compact progress 与重复 Full 防护（Q4）

**状态结构** `CompactBudgetRecovery`（`stages/compact_progress.rs:22-28`）：

```rust
pub(crate) struct CompactBudgetRecovery {
    full_generation: u64,
    request_generation: u64,
    pending_generation: Option<u64>,
    full_end: usize,              // Full 提交后的 transcript.len()
    unrecovered_observations: u32,
}
```

- `record_full_applied(transcript, before_entries_len)`：先判 `has_new_work(transcript, before_entries_len, false)`，若为真则清零观察计数；然后 **`self.full_end = transcript.len()`**、generation+1（`31-44`）。调用点 `stages/compact.rs:397-404`，实参 `before_entries_len` 是 **run_compact 之前**采集的 `transcript_owned.len()`（`compact.rs:193`）。
- `begin_request(transcript)`：请求前若 `has_new_work(transcript, transcript.len(), true)`（只看可见）则视为「该请求已不代表 Full 结果」，清计数、`pending_generation=None`、`full_end = len`（`46-62`）。调用点 `stages/reason.rs:144-150`。
- `observe_response(probe, usage, config, budget)`：generation 不匹配直接跳过；缺 usage 或 `input_tokens==0` 不证明恢复；比例 < `auto_compact_threshold` → 清零；否则累加并在 **`>= 2`** 时返回 `Err(AgentError::CompactBudgetUnrecovered)`（`64-107`，错误在 `102-106`）。调用点 `stages/reason.rs:296-303`。

**no-new-work 判定**（唯一实现，`compact_progress.rs:109-124`）：

```rust
fn has_new_work(&self, transcript: &MessageTranscript, end: usize, visible_only: bool) -> bool {
    if end < self.full_end { return true; }                                   // 111-113
    transcript.entries()[self.full_end.max(transcript.ancestor_len()).min(end)..end] // 114
        .iter()
        .filter(|entry| !visible_only || !transcript.flags(entry.id()).excluded) // 116
        .filter_map(|entry| entry.as_message())                                  // 117
        .any(|message| matches!(message, BaseMessage::Human { .. } | BaseMessage::Tool { .. })
             && !message.message_content().is_empty())                            // 118-123
}
```

语义：只有「非空 Human 或 Tool 消息」才算新工作；AI 消息、Reminder、Full 自己追加的摘要都**不算**——正是回归测试覆盖的点（`compact_progress_test.rs:60-95`：新 summary/AI/Reminder 不得使同一内容无限重获 Full 机会；`test_budget_recovery_below_full_threshold_resets_attempts` `97-108`）。

**shrink 后确切会坏的代码**：

1. `compact_progress.rs:114` —— **位置切片 + 旧长度做 end**。`before_entries_len` 在 `compact.rs:193` 采集于 run_compact 之前；若 Full 改成「删除/收缩条目」而非只打标记，则 commit 之后 `transcript.len() < before_entries_len`，而 `record_full_applied` 仍以 `end = before_entries_len` 调用（`compact.rs:403`）→ 切片区间上界超出 slice 长度，**panic**（`range end index .. out of range`）。同时 `.min(end)` 只在 `end` 合法时才起保护作用。
2. `compact_progress.rs:111` —— `end < self.full_end` 被当成「Rewind/rebuild 后旧边界失效 → 有新工作」。任何条目数减少都会踩中，把「同一份未恢复预算的内容」误判为新工作，从而**清除 unrecovered_observations**，使 `CompactBudgetUnrecovered` 防线（`92-106`）失效。
3. `stages/compact.rs:183-193 / 210-216` —— `pre_compact_flagged` 与 `before_visible_len`、`before_entries_len` 都在 run_compact 前采集，只在「条目只增不减」假设下才与事后读数可比。差值的减法（`compact.rs:244-256`）目前被 `post_compact_flagged > pre_compact_flagged` 守卫（`228`），shrink 只会让该分支不再触发（静默丢掉 cancel-after-commit 的 `InterruptedAfterCommit` 信号），不会 panic。
4. `stages/compact.rs:404` 处传的是写回后的 `ctx.session.transcript.read()`，与 `before_entries_len` 来自不同时刻的同一对象——shrink 会放大这两者的时间差。

### 5. rewind 在 transcript 层（Q5）

`rewind_to(id)`（`transcript.rs:830-868`）：
1. 取 `entries` 全部 id（`831`），用 `history::rewind_keep_len(&ids, RewindBoundary::KeepThrough(id))` 求 `keep_len`，目标不在历史则报错（`834-835`）；`target_idx = keep_len - 1`（`836`）。
2. ancestor 保护：`target_idx < ancestor_len` → 拒绝（`839-845`）。
3. `staged = None`（`848`）。
4. `remove_ids = ids[keep_len..]`（`851`）→ `entries.truncate(keep_len)`（`854`，**不释放容量**）→ 对每个 removed id 做 `id_index.remove` + **`flags.remove`**（`857-860`）。
5. 异步持久化 `PersistOp::RewindTo { id, reserved }`（`863-865`），writer 侧落到 `store.rewind_history(&tid, RewindBoundary::KeepThrough(id))`（`transcript/persistence.rs:318-323`）。

**RewindBoundary 语义**（`peri-acp-types/src/session_resources.rs:239-255`）：`KeepThrough(MessageId)` 保留目标本身、移除其后；`RemoveFrom(MessageId)` 移除目标及之后。两者共享唯一规则实现 `history::rewind_keep_len`（`peri-acp-types/src/store/history.rs:131-145`）与 `history::apply_rewind`（`147-160`）。

**excluded 标记是否被清除**：**不会**。`rewind_to` 只对**被移除的 id** 执行 `flags.remove`（`857-860`）；保留下来的条目（包括目标之前早已被 Full 标 `excluded` 的消息）的标记原样保留，因此有效视图在 rewind 后仍会跳过它们。没有任何「按区间清除 excluded」的代码路径。

**生产可达性**：`transcript.rewind_to` 在全库**只有测试调用**（`transcript_test.rs:184/621/636/647/657`）；用户 rewind 走的是 ACP 路径 —— `peri-acp/src/host/requests/rewind.rs:52-69` `apply_canonical_rewind`（`RewindBoundary::RemoveFrom`，直接裁 `state.history_payloads`）+ `peri-acp/src/session/command/rewind.rs:195-210` `store.remove_history_entries(tid, &removed_ids)`。sqlite 侧 `remove_stored_history_entries` 按 `message_id` 删行（`peri-resources/src/sessions/sqlite_store/session_data/history.rs:128-175`），flags 与消息同表同列（`sqlite_store/compaction.rs:58-100`、`canonical.rs:123`），故**删除即连带删除 flags**；`rewind_stored_history` 按 rowid 删除（`session_data/history.rs:105-121`）。

### 6. 交付去重（Q6）

**稳定交付 ID**：`QueuedMessage.delivery_id: Option<MessageId>` 是终态提醒的稳定身份；`append_messages_to_transcript` 用 `debug_assert!(msg.delivery_id.is_none())` 强制「带稳定 ID 的终态提醒必须走 Receive 的持久化路径」（`stages/queue_to_transcript.rs:12-16`）。

**Receive 的两级查重**（`stages/receive.rs:102-162`）：
1. **热 transcript 查重**：`transcript.get(delivery_id)`（`113`）；命中且是 `TranscriptEntry::Reminder` 且 `stored.as_reminder() == reminder.as_reminder()` → `already_present = true`；命中但类型/内容不符 → 整批回推队列并返回 `"conflicting canonical terminal delivery ID"` 错误（`119-129`）。
2. **持久化事实查重**：未命中时走 `idempotent_reminder_port()`（`131`，`transcript.rs:462-468`），先 `flush_via_tx(&writer)` 保证此前 Append 已落盘（`139`），再 `resources.append_reminder_if_absent(&thread_id, delivery_id, reminder)`（`142`）。trait 契约：同 ID 同内容返回 false、同 ID 不同内容必须失败（`peri-acp-types/src/session_resources.rs:702-709`）。sqlite 实现直接 `SELECT thread_id, content FROM messages WHERE message_id = ?1` 对比（`peri-resources/src/sessions/sqlite_store/session_data/async_task.rs:29-45`）——即**查的是持久化事实，不只是热 transcript**。
3. 成功后 `mirror_committed_reminder(delivery_id, reminder)` 写回内存（`156-162`），返回值即 `newly_committed`；`false` 时 `continue` 跳过事件发射（`167-169`）。

**handoff 同构**（`async_tasks/handoff.rs:113-160`）：`idempotent_reminder_port()`（`120`）→ `flush_via_tx`（`133-135`）→ `append_reminder_if_absent`（`136-139`）→ `mirror_committed_reminder`（`144-147`）。delivery id 由 pending 任务集合的 SHA-256 派生（`214-231`，`b"bounded-wait-handoff"` + 每个 task_id/owner 的长度前缀），同一 pending 集合重复 handoff 收敛为一条。

风险提示（shrink 相关）：若收缩从热 transcript 中丢弃已交付的 reminder 条目，`receive.rs:113` 的一级查重会失效，只能靠二级持久化查重兜住；此时 `mirror_committed_reminder` 会把它重新插回内存（`transcript.rs:439-452` 仅查 `id_index` 不存在才插），语义上等于「重放了一次已交付提醒」。

### 7. 假定「全量历史」的消费者清单（Q7）

**`persisted_payloads()`**（全量 canonical payload，不排除 excluded）：
| file:line | 假设 |
|---|---|
| `peri-agent/src/session/exec/executor_helpers/v2_execute.rs:513` | 把全量 payload 当作会话权威快照交给 `PromptResult`（无持久化失败时） |
| `peri-agent/src/session/exec/executor_helpers/v2_execute.rs:499-516` | 返回三元组 `(messages=visible, persisted_payloads=全部, history_replaced_by_compaction)` —— 刻意保持「可见视图 ≠ 权威快照」 |
| `peri-agent/src/session/exec/executor_helpers/collect.rs:39` | 原样透传到 `PromptResult` |
| `peri-agent/src/session/exec/executor_helpers/intercept.rs:305-318` | 以 `req.history_payloads` 为基线、按 `existing_ids` 去重后追加命令新增消息 —— 假设基线是全量历史 |
| `peri-acp/src/host/prompt.rs:611-614` | `state.history_payloads = result.persisted_payloads` 并据此派生 legacy `state.history` —— host 跨 turn 保留含 excluded 的原文 |
| `peri-agent/src/session/subagent/factory/spawn.rs:209-215` | 无持久化路径时用父 transcript 全量 payload 对齐 canonical |
| `peri-agent/tests/compact_authority_projection_test.rs:185-195` | 断言 reminder 条目身份需按全量 payload 计数 |

**`.entries()`（全量 + 位置下标）**：
| file:line | 假设 |
|---|---|
| `peri-agent/src/agent/stages/compact_progress.rs:114` | 用「旧长度」`full_end`/`end` 作为切片边界 —— 位置在时间线上稳定且单调增长 |
| `peri-agent/src/agent/compact_v2/full.rs:86-101`、`129-133` | owner 区条目集合与 excluded 计数按全量 entries 遍历 |
| `peri-agent/src/agent/compact_v2/planner.rs:115-156` | `TurnGroup::collect` 用 `enumerate()` 的位置 `i` 与 `ancestor_len` 比较，并把 `(usize, TranscriptEntry)` 存进 `TurnGroup`/`ToolExchange`（`95-108`）——下标即身份 |
| `peri-agent/src/agent/compact_v2/projection.rs:219-264` | `estimate_projection_chars` 全量遍历 entries 估算字符数 |
| `peri-agent/src/agent/stages/compact.rs:182-216` | 前后 flag 计数对比（`pre_compact_flagged` / `post_compact_flagged`） |
| `peri-agent/src/session/subagent/factory/spawn.rs:146-152` | 在 owner 区内按 System 内容查找父身份消息并剔除 |
| 测试：`peri-agent/src/agent/compact_v2/_test.rs:413/470`、`micro_test.rs:96-367`、`smart.rs:132-281`、`transcript_test.rs:273-834`、`compact_pressure_adversarial_test.rs:409`、`full_test.rs:443-621` | 直接按下标取值断言 |

**`id_index`**：全部出现在 `transcript.rs` 内部（`206/285/319/429/444/448/455/518/531/559/572/585/624/633/653/736/741/752/782/810/858`），无外部消费者；`transcript_test.rs:382` 只验证 O(1) 查找。

**`excluded`**（非测试消费者）：
| file:line | 假设 |
|---|---|
| `peri-agent/src/agent/compact_v2/full.rs:90-101` | Full 的排除集合 = owner 区所有非 excluded、非 System 条目 |
| `peri-agent/src/agent/compact_v2/planner.rs:120` | Micro/Smart 候选跳过 excluded |
| `peri-agent/src/agent/compact_v2/projection.rs:227` | 投影字符估算跳过 excluded |
| `peri-agent/src/agent/stages/compact_progress.rs:116` | 「新工作」可见性判定 |
| `peri-agent/src/session/exec/compact_pipeline.rs:157-158`、`peri-acp-types/src/store/mod.rs:207-213`、`peri-resources/src/sessions/sqlite_store/compaction.rs:161-169` | 持久化层把 excluded 当独立列/独立事实保存并回读 |

**`MessageFlags`**：定义 `peri-acp-types/src/store/mod.rs:210-220`；规则唯一实现 `peri-acp-types/src/store/history.rs:88-112`（`flags_with_projection` 保留 excluded、`apply_flag_updates` 默认值即删除）；持久化列 `sqlite_store/compaction.rs:58-100`、`canonical.rs:123/233`；远程/文件后端 `peri-resources/src/sessions/remote/session_codec.rs:74/202/220`、`filesystem.rs:449-451`（明确声明本地 filesystem 后端不持久化这些标记）。

**`ancestor_len`**（非测试消费者）：
| file:line | 假设 |
|---|---|
| `peri-agent/src/agent/compact_v2/full.rs:89` | Full 只处理 owner 区 |
| `peri-agent/src/agent/compact_v2/planner.rs:120` | Micro/Smart 跳过祖先区 |
| `peri-agent/src/agent/stages/compact_progress.rs:114` | 切片下界 `max(full_end, ancestor_len)` |
| `peri-agent/src/session/subagent/factory/spawn.rs:148` | 只在 owner 区查找父身份 System |
| `peri-agent/src/session/subagent/provenance_test.rs:278`、`executor_provenance_test.rs:114`、`executor_test.rs:853` | 断言继承后被划入只读区 |

**`CompactionCommitState`**：定义与读写 `transcript.rs:42-61/224/272/497-504`；生产装配与消费 `executor_helpers/intercept.rs:18/195-200/258-278`、`session/exec/compact_pipeline.rs:42/77-80/178`；守卫 `transcript.rs:724`。测试替身：`executor_helpers/compact_cancel_test.rs`。

**`RewindBoundary`**：定义 `peri-acp-types/src/session_resources.rs:243-255`；规则 `peri-acp-types/src/store/history.rs:135-160`；transcript 生产消费 `transcript/persistence.rs:321`（writer 转 `store.rewind_history`，`session_resources.rs:749-753`）；用户侧 `peri-acp/src/host/requests/rewind.rs:58-61`；adapter 实现 `peri-resources/src/sessions/sqlite_store/session_data/history.rs:83-126`、`peri-resources/src/sessions/resources.rs:633-641`、`remote/session_data.rs:783-791`。

**其他全量历史相关**：`rebuild(Vec<(BaseMessage, MessageFlags)>)`（`transcript.rs:793-822`）**生产零调用者**，仅 `transcript_test.rs:277/682/700/712/834`；`middleware_runner.rs:16` 记录了「不再需要 restore_from_agent_state.rebuild()（消除 O(n) 全量 entries 重建）」这一已作的决定。`replace_by_id`（`584-588`）与 `mirror_committed_payload`（`454-460`）同样只被 `push_loaded_payload` 家族/测试覆盖。

## Constraints & risks

1. **`persisted_payloads()` 是 host 侧跨 turn 的权威快照**（`peri-acp/src/host/prompt.rs:611`）。任何收缩只要改变这个函数的产出（例如不再回放 excluded 条目），就会同步改变 ACP 层的 `state.history_payloads`、`/compact` 的 `history_matches` 校验（`compact_pipeline.rs:200-212`）以及 rewind 的 canonical 基线（`rewind.rs:52-69`）——这是一条硬约束链。
2. **`before_entries_len`/`full_end` 是长度而非身份**：`compact_progress.rs:114` 用旧长度切当前 Vec，是目前唯一会因为「条目数减少」直接 panic 的位置（前提是 `end` 大于当前 `len`）。
3. **`rewind_to` 不释放 `entries` 容量**（`transcript.rs:854`），`rebuild` 才用 `with_capacity` 重分配（`794-796`）；`transcript.rs:477-479` 明确记录 `visible_snapshot()` 仍逐条深拷贝。即「收缩」在内存占用上不会自动发生。
4. **`set_excluded` / `rebuild` / `rewind_to` 是生产死代码**（Q7 表）。Full 的排除走 `commit_compaction_lifecycle` + `apply_flag_updates`，rebuild 路径已被 `middleware_runner.rs:16` 声明淘汰——重构时可优先核对这三处的去留。
5. **flags 与消息同表**：sqlite 删行即删 flags（`sqlite_store/compaction.rs:58-100`、`canonical.rs:123`），remote/filesystem 后端能力不同（`filesystem.rs:449-451` 声明不持久化标记；`compact_pipeline.rs:132-154` 要求 backend `DataCapabilities::Complete` 才允许 `/compact`）。
6. **持久化失败是 sticky 且不可回滚**：`PersistenceBudget::mark_failed` 首次锁定（`persistence.rs:123-130`），写入侧不重试可能部分成功的批次（`persistence.rs:149-152`），transcript 侧唯一出口是让会话走冷重载（`transcript.rs:365-371`；`v2_execute.rs:480-495`）。

## Open questions

1. 收缩的目标语义是哪一种？(a) 从 `entries` 真删已 `excluded` 条目、(b) 新增独立的「压缩快照 + 尾部增量」结构、(c) 仅改存储格式。三者对 `persisted_payloads()`/host `history_payloads`（`prompt.rs:611`）与 `compact_progress.rs:114` 的影响完全不同；本报告只能给出三处会坏的位置，无法判断该保哪个不变量。
2. `full_end` 应该改成「身份水位（MessageId）」还是「generation 计数」？`CompactBudgetRecovery` 目前同时持有 `full_generation`/`full_end` 两种表达（`compact_progress.rs:22-28`），我未找到说明二者分工的文档或注释。
3. 用户 rewind（`RemoveFrom`）之后热 transcript 如何与磁盘对齐？我只确认 ACP host 更新 `state.history_payloads`（`rewind.rs:134-136`）与 `store.remove_history_entries`（`command/rewind.rs:195-210`），未在本轮读到「热 transcript 被驱逐/重载」的确切代码——若不存在，`compact_progress.full_end` 与 `flags` 表在 rewind 后可能长期陈旧。
4. `append_compaction` 的具体事务边界（摘要、flags、计数、缓存视图是否同一事务）我只读到 trait 契约（`session_resources.rs:734-739`）与调用点，未读 sqlite `compaction.rs` 全文；提交点结论（`transcript.rs:764`）依赖该契约成立。
