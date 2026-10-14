# S3 审查发现（人工复核，供 S4/S5 与后续切片消费）

审查对象：worktree 当前未提交改动（S1+S2 已报告的同一 working tree）。
基线：`origin/main` = `7f0de56e8`。

## 覆盖状态（对照 workflow S4 任务清单）

| 编号 | 内容 | 归属 |
| --- | --- | --- |
| F1 | delivery 去重丢 durable 返回值 | S4 任务 4（明写在 prompt 内，需核对交付） |
| F2 | `has_new_work` 的 `end.min(len)` 只防越界 | S4 任务 1 |
| F5 | `compact_pipeline` `history_matches` 比较全量 | S4 任务 3 |
| F6 | rewind 候选/预览/执行依赖 Host 全量历史 | 后续 rewind 切片（蓝图 S4 / D4），本轮 workflow 不含 |
| F3/F4 | 继承与 `session/load` 仍全量读 | 后续分页切片（蓝图 S5 / D3、D5） |

D1 前提已核实：`MessageTranscript::commit_compaction_lifecycle` 的唯一生产调用者是
`compact_v2/full.rs:122`，因此无条件插入 `shrink_after_full_commit` 不会误伤其他 Compact 形态。

## F1（必须修，S4 范畴）：delivery ID 去重在收缩后丢掉 durable 返回值，会重复投递

**位置**：`peri-agent/src/agent/stages/receive.rs`，delivery 分支
（`let committed = ... resources.append_reminder_if_absent(...)` → `newly_committed = ...transcript.write().mirror_committed_reminder(...)`）。

**事实**：
1. durable 层 `SqliteSessionData::append_terminal_reminder`
   （`peri-resources/src/sessions/sqlite_store/session_data/async_task.rs:6`）已经是三态：
   `Ok(true)` = 新插入、`Ok(false)` = 已存在且内容一致、`Err(invalid_input)` = 冲突。
2. `receive.rs` 丢弃了这个 `bool`，只用「热 transcript 是否命中」决定 `already_present`。
3. 收缩前：已投递 reminder 的 id 仍在 `id_index`（即使 `excluded`），热检查命中 → `already_present = true` → 只消费。
4. 收缩后：`shrink_after_full_commit` 移除了被排除的 reminder 条目 → 热检查不命中 →
   durable 返回 `Ok(false)`（已存在）→ 代码继续执行 `mirror_committed_reminder`，该 id 已不在
   `id_index`，于是返回 `true` → `newly_committed = true` → **再次 emit `ExecutorEvent::SystemReminder`**。

**后果**：
- 与 `docs/code-index/peri-agent.md` 中「已提交或 excluded 的同 ID 提醒只消费不唤醒模型」的既定语义冲突。
- 热态/store 分歧：store 行仍是 `excluded = 1`，热态却新增了一条未排除的同 id 条目；
  之后 `active_payloads()`（热）与 `load_active_payloads_on`（store）结果不一致。
- 正是 REQ-T02「稳定 delivery ID 三态幂等」要防的场景。

**建议修法**（最小）：以 durable 返回值为权威，热 mirror 只作为新增成功后的镜像：

```rust
let inserted = resources.append_reminder_if_absent(&thread_id, delivery_id, reminder).await?;
newly_committed = inserted
    && input.context.session.transcript.write().mirror_committed_reminder(delivery_id, reminder.clone());
```

无持久化端口（`port == None`）时保持现有 `mirror_committed_reminder` 语义不变。
需补一条回归测试：同一 delivery id 在 Full Compact 收缩之后再次投递，断言不产生第二次
`SystemReminder` 事件且热态不出现未排除的同 id 条目。

## F2（S4 已计划，确认不要漏）：`has_new_work` 的 `end.min(len)` 只是防越界，语义未修

**位置**：`peri-agent/src/agent/stages/compact_progress.rs:110`。

`end` 既可能是「收缩前长度」（`compact.rs:193` 的 `before_entries_len`），也可能是
`transcript.len()`；`full_end` 在 `record_full_applied` 里存的是**收缩后**长度
（`transcript.len()`），但 `compact.rs:403` 传进来的 `before_entries_len` 是**收缩前**长度。

收缩后立即出现：`end.min(len) == new_len < full_end` → `has_new_work` 提前返回 `true`
→ 判定「有新工作」→ 允许立刻再次 Full。这与 REQ-T02「重复 Full 保护用稳定标识」相反，
只是被 S3 的 clamp 从 panic 降级成误判。S4 的 watermark（`full_watermark: Option<MessageId>`）
必须真正替换 `usize` 位置比较，而不是保留 `usize` 语义再加 clamp。

## F3（记录，非阻塞）：subagent 继承仍走全量 `load_session_snapshot`

**位置**：`peri-agent/src/session/subagent/factory/spawn.rs`（`load_session_snapshot(parent_id)`
→ `snapshot.inherited.payloads` + `snapshot.payloads` 合并）。

有持久化路径下，为对齐继承区 canonical 而读取父会话**整份**快照（含所有被排除正文），
属于「先全量再取用」。它不在 `session/load` 首屏路径上，但属 REQ-T04「fork 不依赖全量历史」
的同类。S1 已提供有界入口 `load_message_flags`，此处需要的是「按 id 集合取 canonical payload」，
建议在 S6/S8 一并收敛（可能需要 S1 再补一个有界 payload 读取的默认实现）。

## F4（记录，确认边界）：`session_restore.rs` 首屏回放仍是「读全量再过滤」，且失败只 warn

未变（S3 未触及该文件）。`handle_load` → `load_frozen_bytes` 读整份 `SessionSnapshot`，
`state.history_payloads.clone()` 全量回放，回放失败只 warn 后正常响应。属 S6（D3/D5）范围：
REQ-T03 要求直接读有效范围，REQ-H04 要求首屏与实时尾部衔接且失败不得静默。

## F5（必须修，S4 任务 3）：`compact_pipeline` 用全量持久化历史校验收缩后的 Host 快照

**位置**：`peri-agent/src/session/exec/compact_pipeline.rs:157`（`load_session_snapshot`）
与 `:198-206`（`history_matches`）。

**事实**：
1. `history`（`CommandContext.history`）来自 Host 的 `state.history`；拦截层
   （`executor_helpers/intercept.rs:207`）与 RPC 层（`dispatch/execute_command.rs:180`）
   都从 Host 会话状态取。
2. S3 之后 `finish_prompt_turn`（`peri-acp/src/host/prompt.rs:611`）把 Host 快照更新为
   `active_payloads()` —— 收缩后的**有效集合**。
3. 管线仍用 `load_session_snapshot` 取**全量**（含 excluded）再逐条比对 id。

**后果**：同一进程内发生过一次 Full Compact 后，下一次 Compact（手动或自动）必然
`persisted_history.len() != history.len()` → `EarlyReturn` 且
`message = "compact persistence context mismatch"`，**Compact 功能在会话内永久失效**。
现有单测多用全新 store，覆盖不到「同一会话二次 Compact」。

**修法**：既有 thread 分支改为 `load_active_snapshot`（有效 own payload + 有效 flags），
与传入的 Host 有效快照同范围比较；`with_own_payloads` / `set_flags_batch` 随之只喂有效集合。
需补回归测试：同一 store 上连续两次 Full Compact（第二次必须成功）。

## F6（后续 rewind 切片，蓝图 D4）：rewind 三条路径都吃 Host 内存快照

**位置**：`peri-acp/src/host/requests/rewind.rs`：`handle_rewind_candidates:20`（`s.history.clone()`）、
`handle_rewind_preview:41`、`handle_rewind:104` 与 `apply_canonical_rewind:53`。

三条路径都在 Host 的 `state.history_payloads` 上做查找/裁剪。S3 之后该快照只剩有效集合，
于是 Full Compact 之前的目标消息在 `rewind-candidates` 中消失、`rewind-preview` 与
`rewind` 报 `-32602 rewind target not found` 或
`-32603 rewind target missing from canonical history`。

REQ-T04 要求候选/预览/执行按目标从存储读取并重建边界有效视图，因此这是该切片的必做项，
不能作为「已知限制」保留。另需给 `handle_rewind_candidates` 加存储读取（当前是同步函数，
要改成 async 或把读取提到调用方）。

## F7（记录，属分页切片）：`load_session_payloads` 仍是全量，且与进程内快照语义不一致

**位置**：`peri-acp/src/dispatch/session_load.rs:18` → `load_session_history`（全量）。

消费者：
- `session_restore.rs:137-144`：已激活但快照为空的兜底 → 全量；
- `session_restore.rs:220`：`Publish` 分支（新进程首次 `session/load`）→ 全量；
- `session_lifecycle.rs:754` `fork_bound_session` → 子会话快照 = **全量**。

**后果**：同一会话在**同进程**再 load 拿到的是有效集合（走
`state.history_payloads`），**新进程**首次 load 拿到的是全量，fork 出的子会话又是全量。
三者语义不一致，且新进程首屏仍违反 REQ-T03「禁止先全量再过滤」。

## F8（交付注意）：S3 落地后、分页/前端切片落地前，TUI 会出现可见回退

手动 Compact 流程是 `TurnDone` → 强制 `session/load` 重放
（`peri-tui/src/kit/acp_events/turn.rs`），重放内容现在是**有效集合**。
在 REQ-C02（取消强制 reload）与 REQ-C01（滚动分页）落地前，用户看到的是
「Compact 后旧历史整段消失且无法回看」。这是切片顺序的中间态，不是最终缺陷，
但不能单独交付 S3。本轮 workflow 只到 S5（= 蓝图 S1~S3b），rewind 与分页/前端仍在后面。

---

## 复核轮补充（人工修复，run 1 之后）

### 基线推进
分支基线已从 `7f0de56e8` 快进到 `5be107603`（stash → ff → pop，54 文件 / +1895 未变）。
交集仅 3 个文件且三方合并干净：`docs/code-index/peri-agent.md`、`factory/{spawn,resume}.rs`。
对账结论：上游改的是 sync 子任务交接与迟到结果保留，与本轮「读取范围」改动不同区域。

### F9（真实回归，已修）：spawn 改读有效视图后，父会话的 `excluded` 标记不再传给子会话继承区
- 现象：`provenance_test.rs` 断言「模型请求不得含 `old parent excluded`」失败（该断言此前被 F1 循环的失败遮蔽，S5 未看到）。
- 根因：`spawn.rs` 用 `load_active_snapshot(parent_id).flags` 取继承区 flags。有效视图的 flags **只覆盖未排除 id**，
  而继承区仍以正文形式携带父会话已压缩的前缀 → 子会话把父会话已退出有效上下文的原文当成活跃上下文发给模型。
- 修法：正文仍取有效视图（已排除正文不再是继承来源），flags 改为有界读取
  `store.load_message_flags(parent_id, &inherited_ids)`（含 `excluded`），不为此全量加载父历史。
- 教训：`load_active_snapshot` 的 `flags` 字段语义是「活跃 id 的标记」，**不能**当作「该 id 是否被排除」的判据来源；
  需要排除事实时必须走 `load_message_flags`。

### 测试侧更新（旧契约，非实现回归）
1. `requests_legacy_test.rs` 的 `FailingHistoryLoad` 单点故障面从 `load_session_history` 改为 `load_active_snapshot`
   （发布路径现在读后者；夹具经 `cfg.controller` 注入，正落在接纳完成与发布之间）。
2. `provenance_test.rs` 的 flags 逐项还原循环改为只比较有效 id，并补两条断言：
   已排除 own 条目不在恢复热态、但其原文仍可从 `load_session_history` 读到。
