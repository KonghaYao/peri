# 实现蓝图：Full Compact 后 Transcript 收缩与 ACP 历史分页

状态：实现中。基线 `7f0de56e8`（worktree `.worktrees/peri-transcript-history`，分支 `feat/transcript-shrink-history-pagination`）。
需求源：`/Users/konghayao/Desktop/peri-transcript-history-requirements.md`。
现状侦察报告：`.recon/A-core-transcript.md`、`B-acp-host.md`、`C-storage-contract.md`、`D-test-infra.md`、`E-tui-sdk.md`（所有行号基于基线 commit）。

## 1. 目标

Full Compact 成功后，核心热态 Transcript 真正释放已退出有效上下文的 own 历史；恢复路径只读取有效范围；ACP `session/load` 支持 cursor 分页（Peri 协商扩展）。

## 2. 设计决策

### D1 收缩语义（REQ-T01/T02）

- 位置：`MessageTranscript::commit_compaction_lifecycle`（`peri-agent/src/session/transcript.rs:720`）。唯一构造者与调用者是 Full（`compact_v2/full.rs:122`），因此收缩直接落在该方法的「内存应用」之后（`transcript.rs:769` 之后同函数内），不新增第二个提交入口。
- 顺序不变：`mark_pending → flush_persistence → store.apply_compaction → apply_compaction_lifecycle_memory → [新增收缩] → mark_committed`。收缩只在 store 确认提交后执行。
- 收缩规则：
  - 保留 `entries[..ancestor_len]`（祖先区，语义与容量均不保证，不属本次范围）。
  - 保留未 `excluded` 的 own 条目（含 System、Micro 投影条目）。
  - 保留本次 `appended_messages`（摘要）。
  - 移除所有 `excluded` 的 own 条目；同步从 `id_index`、`flags` 移除；被移除条目不得留下占位或 ID 集合。
  - `staged` 不动（staging 未入 entries，天然不被收缩）。
  - 以保留数重建 `entries`/`id_index`/`flags` 三个容器（新分配，释放旧容量）。
- 后果：`MessageTranscript::persisted_payloads()`（`transcript.rs:391`）的产出在收缩后不再包含 excluded 条目。该函数改名/拆分为显式语义：
  - `active_payloads()`：热态当前有效集合（供跨 turn 快照）。
  - 完整历史读取一律走存储（`SessionResources::load_session_history`），不再从热态导出。
- 同步调整的消费者（沿用 `active_payloads` 语义，见切片 S3b）。

### D2 进度与重复 Full 保护使用稳定身份（REQ-T02）

- `CompactBudgetRecovery.full_end: usize`（`peri-agent/src/agent/stages/compact_progress.rs:26`）改为 `full_watermark: Option<MessageId>`（Full 提交后末尾摘要消息的 id）。
- `has_new_work`（`compact_progress.rs:109`）：
  - watermark 存在于 `id_index` → 检查其后条目（语义等价原 `full_end..len`，对收缩鲁棒）。
  - watermark 不存在（rewind/重建）→ 视为有新工作（等价原 `end < full_end → true`）。
- `record_full_applied`/`begin_request` 的调用点同步改签名（`compact.rs:193` 采集的 `before_entries_len` 语义一并核对）。

### D3 有效范围读取（REQ-T03）

- 新增类型契约（`peri-acp-types/src/session_resources.rs`）：
  ```rust
  pub struct ActiveSessionSnapshot {
      pub meta: ThreadMeta,
      pub binding: BindingState,
      pub frozen: FrozenState,
      pub inherited: InheritedContext,
      pub payloads: Vec<PersistedPayload>,          // 仅未排除的 own
      pub flags: HashMap<MessageId, MessageFlags>,  // 仅活跃 id 的非默认 flags
  }

  // trait 新增
  async fn load_active_snapshot(&self, id: &ThreadId) -> SessionResourceResult<ActiveSessionSnapshot>;
  ```
- 存储实现（`messages.excluded` 与消息同表，可在 SQL 层过滤，**不新增列/表**）：
  - SQLite：`SELECT message_id, content FROM messages WHERE thread_id = ?1 AND excluded = 0 ORDER BY rowid` + flags 同条件。
  - Remote：新静态 SQL 常量（`sql: &'static str` 约束）。
- 消费者切换：
  - `peri-agent/src/session/exec/executor_helpers/v2_execute.rs:150`：`load_session_snapshot` → `load_active_snapshot`（当前只用 `inherited`/`flags`，却全量加载正文）。
  - `peri-acp/src/host/requests/session_restore.rs:51` `load_frozen_bytes`：不再读整份 snapshot 的 payloads。
  - `peri-agent/src/session/exec/compact_pipeline.rs:198` `history_matches`：有效集合比较。
  - `peri-acp/src/host/prompt.rs:611`：`state.history_payloads` 语义改为有效集合。
- `content_size`（`SUM(LENGTH(content))`，`row_mapping.rs:11`、`remote/session_sql.rs:32`）是恢复热路径上的全历史聚合且无生产消费者：移出热路径（保留列，改为按需/移除计算，见 S2 说明）。

### D4 rewind 有效视图重建（REQ-T04）

- rewind 候选/预览/执行改为按目标从存储读取（不依赖热态全量历史）。
- 边界重建：determine 该边界仍成立的 Compact 结果 → 移除含未来信息的摘要 → 恢复应重新有效的原文 → 热态与冷恢复一致。
- 破坏历史范围的操作推进结构版本（供 cursor 失效），与 Compact 提交一致维护最小元数据。

### D5 ACP 分页契约（REQ-H01~H04）

- 请求入口在进入生命周期路由前解析模式：`InitialLoad` / `HistoryPage`（字段：`cursor`、`limit`）。
- `HistoryPage` 不取执行生命周期锁、不做任务绑定/MCP attach/命令广播/continuation；不跨存储 I/O 持有全局 sessions 锁。
- 游标身份：`MessageId`（存储适配器内部解析 `rowid` 实现 keyset，不暴露物理键）。
- 能力按**当前连接**协商（`PeriCaps` 现状为进程级 pending，多连接互相覆盖 → 绑定到连接）。
- 初始回放失败必须使本次 load 失败（现状 `session_restore.rs:525` 仅 warn）。

### D6 前端（REQ-C01~C03）

- 取消「手动 Compact 后强制完整 `session/load`」依赖（TUI `acp_events/turn.rs` + SDK）。
- 分页合并按稳定逻辑位置，不整段替换；历史投影与实时状态隔离。

## 3. 实现切片

| 切片 | 范围 | 主要文件 |
| --- | --- | --- |
| S1 | 契约层 | `peri-acp-types/src/session_resources.rs`、`store/history.rs` |
| S2 | 存储实现 | `peri-resources/src/sessions/{sqlite_store/context.rs,session_data.rs,compaction.rs}`、`remote/session_sql.rs`、`sessions/data.rs`、`resources.rs` |
| S3a | 核心收缩 | `peri-agent/src/session/transcript.rs`、`agent/compact_v2/full.rs` |
| S3b | 进度/消费者 | `agent/stages/compact_progress.rs`、`compact.rs`、`session/exec/executor_helpers/v2_execute.rs`、`session/exec/compact_pipeline.rs`、`agent/stages/receive.rs`、`agent/async_tasks/handoff.rs`、`peri-acp/src/host/prompt.rs` |
| S4 | rewind | `peri-acp/src/host/requests/rewind.rs`、`session/command/rewind.rs`、`peri-resources` rewind 实现 |
| S5 | ACP 分页 | `peri-acp/src/host/{server_loop.rs,requests.rs,requests/session_restore.rs}`、`dispatch/session_load.rs`、`session_replay.rs`、`peri-acp-types/src/peri_caps.rs` |
| S6 | 前端 | `peri-tui/src/kit/**`、`npm-packages/@peri-sdk/src/**` |

## 4. 不变量（每切片必须保持）

1. Full 提交成功前不释放任何原文；提交失败/不确定时保留现有失效与重载语义。
2. 收缩不得删除未排除条目、不得破坏工具配对（同一 projection 封装整体保留/移除）。
3. 收缩前后同一已提交有效视图的模型请求语义等价。
4. 祖先区与冻结继承不被父会话收缩改写。
5. 查询计划证据：恢复路径上被排除 own 正文读取为 0 条；禁止先全量再过滤。

## 5. 验证

- 每切片 `./scripts/cargo-rmcp-patched.sh check --locked -p <crate>`；关键切片跑相关单测。
- 验收映射见需求 §9；未覆盖项在报告中显式列出。

## 6. 后续切片（本轮范围外，设计已定）

### S6 ACP 历史分页（REQ-H01~H04）

- 契约（`peri-acp-types`）：
  ```rust
  pub struct HistoryCursor {          // 不透明值，客户端不得解释
      pub thread_id: ThreadId,
      pub snapshot_token: String,     // 浏览快照身份（投影版本 + 字节预算固定绑定）
      pub anchor: MessageId,          // 下一读取位置（更早方向）
      pub structure_version: u64,     // rewind 等破坏性操作递增，使旧 cursor 失效
  }
  pub struct HistoryPage {
      pub items: Vec<HistoryPageItem>,   // 页内历史正序
      pub next_cursor: Option<String>,   // None = 没有更早可展示内容
      pub oversize: bool,
  }
  // trait 新增（默认实现返回 Unsupported，未协商/未实现后端 fail closed）
  async fn load_history_page(&self, id: &ThreadId, cursor: &HistoryCursor, limit: usize)
      -> SessionResourceResult<HistoryPage>;
  ```
- 存储：keyset 分页，`WHERE thread_id = ?1 AND rowid < (SELECT rowid FROM messages WHERE message_id = ?2) ORDER BY rowid DESC LIMIT ?3`，返回后反转为正序。**不过滤 excluded**（归档区正是被压缩的原文），但过滤不可展示条目时位置仍须推进。有界读取：字节预算约束在读取阶段，不批量物化。
- 初始 cursor：从有效视图最早 own 条目（摘要）与压缩覆盖边界派生，不取首屏第一条消息 ID。
- ACP 分流：在 `dispatch_request`（`peri-acp/src/host/server_loop.rs:367`）中于生命周期锁分支之前解析 `session/load` 模式；`HistoryPage` 复用 `session_io` 的「短锁 prepare + 锁外 handle」模式（`server_loop.rs:440-454`），不取执行锁、不做绑定/attach/广播，不跨存储 I/O 持全局 sessions 锁。
- 能力：新增 `PeriCaps::history_pagination`（`peri.historyPagination`，四处同步），未协商时提供 cursor 字段或非法 limit 一律明确拒绝，不降级为初始 load；能力按**当前连接**生效（现 `session/caps.rs` 为进程级 pending，多连接互相覆盖，需绑定连接）。
- 回放：初始回放带 `loadGeneration` 隔离；回放失败使本次 load 失败（现状 `session_restore.rs:525` 仅 warn）。

### S7 前端（REQ-C01~C03）

- TUI：向上滚动触发 cursor load；区间合并（不整段替换、不全局头插）；稳定锚点；翻页 loading 与执行状态分离；删除「手动 Compact 后强制完整 session/load」依赖（`acp_events/turn.rs`）。
- SDK：分页合并与去重、Compact 与 rewind 分流（`state/chat-projection.ts` 的 `replaceHistory` 全量清空需改为增量合并）、历史投影不推进实时状态。

### S8 rewind 有效视图（REQ-T04）

- 候选/预览/执行改为按目标从存储读取；边界重建：移除含未来信息的摘要、恢复应重新有效的原文；推进 `structure_version` 使旧 cursor 失效。

## 7. 执行状态与决策记录（2026-10-13 晚）

### 已落地（worktree 内未提交，分支 `feat/transcript-shrink-history-pagination`）

1. **S1/S2 契约与存储**：`ActiveSessionSnapshot` / `load_active_snapshot` / `load_message_flags`；两后端有效范围读取（过滤在存储侧完成），恢复路径移除 `SUM(LENGTH(content))` 聚合。
2. **S3a/S3b 核心收缩与消费者**：`shrink_after_full_commit` 只在 durable 提交成功后释放已排除 own 条目；Compact 进度改用 `full_watermark: Option<MessageId>`；delivery 三态幂等改按持久事实判定；手动 Compact 身份校验改为有效集合对称比较；subagent 继承区 flags 走 `load_message_flags`（含 `excluded`）。
3. **S4/S8 rewind**：边界规则纯函数（`rewind_clear_excluded_ids`）+ 唯一原子存储入口（SQLite 单事务 / 远端单批）；ACP 三条路径改为以存储为事实源，提交成功后才重建有效视图。
4. **S5/S6 ACP 分页出口**：`HistoryCursor`/`HistoryPage`/`assemble_history_page`/`history_page_bounds`；`session/load` 模式分流在生命周期锁之前；连接级能力 `peri.historyPagination`；结构版本 `SessionState.structure_version`（内存、无 DDL）在 rewind 成功分支恰好递增一次；回放失败使本次 load 失败；`loadGeneration` 贯通响应与重放通知。
5. **验证**：`.recon/R4-verification.md`、`.recon/S6-verification.md`；两次独立复核报告 `.recon/S3-review-findings.md`、`.recon/rewind-review-findings.md`。

### 决策记录

- **D7 标准（未协商）客户端完整回放 —— 依需求 §5.3 补齐。**
  §5.3 原文要求：未协商的通用 ACP 客户端仍使用标准完整回放投影，不得被误告知已完成标准全量回放；标准路径可以通过有界读取流式输出，不要求核心重新持有完整历史。
  现状：`session/load` 对所有连接只回放 `SessionState.history_payloads`（有效集合），标准客户端会拿到被截断的历史却收到成功响应 → 违背 §5.3。
  处置：协商连接 = 有效视图 + 首游标（现状不变）；未协商连接 = 完整 canonical 回放（继承区 + 全部自有行，含 Full 归档原文），经**向前有界读取**分页流式投递，核心不持有全量。存储需补一个向前 keyset 读取（与向后读取同构，不新增列、不新增依赖）。
- **D8 rewind 到排除段内部时的「原文与摘要同时有效」= 已知偏差，本轮不修。**
  两轮以上 Full 后回退到前缀内部位置时，`excluded` 连续段无法从现有 flags 判定压缩批次边界（`MessageFlags` 只有 `truncated`/`excluded`/`projection`，后者是 Micro 投影指令）。实现采保守策略：整段清零（内容不丢，但可能旧摘要与其覆盖原文同时有效），与 A19「无原文／摘要重复覆盖」字面不符。
  精确修复需新增「覆盖批次」最小元数据（属 DDL）；按仓库规则（根 CLAUDE.md 原则 12）须用户明确批准，批准前不新增列。记录为已知偏差，授权后另开切片；`peri-acp-types/src/store/history.rs` 的模块文档与 `.recon/rewind-review-findings.md` 已注明。
- **D9 手动态 Compact 读取 canonical 快照 —— 保留，不视为缺陷。**
  `compact_pipeline.rs` 从完整快照重建临时 transcript（含归档正文）用于 Compact 执行；Full 的摘要输入取有效视图、assembled 输出取有效视图，归档正文既不进摘要也不回写热态，属读取成本而非语义泄漏（REQ-T03 约束的是恢复路径）。
- **D10 恢复路径的存储触达行数仍是 O(线程行数) —— 实测记录，修复需索引（未授权）。**
  三段实测（10k / 100k / 1M 归档行，`.recon/perf-evidence.md` §4.1/§8）：恢复 cold p50 = 11.109 ms / 98.182 ms / 1081.927 ms，
  解码正文恒为 196 971 B（排除正文解码 0 条）。即「内存与解码字节有界」成立，「工作量不随归档条数线性增长」（A05）**未达成**。
  机制：`ACTIVE_PAYLOADS_SQL` / `ACTIVE_FLAGS_SQL` / `ACTIVE_ENTRY_POSITIONS_SQL` 的计划都是
  `SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)`，而该索引不覆盖 `excluded`/`content`，
  于是逐行回表判定 —— 触达上界 = 该线程总行数（行数探针 10 003 / 100 003 / 1 000 003）。
  分页（更早方向）是 keyset 定位，不在此列（1M 上每页 2–3.6 ms）。
- **D11 `periLoadGeneration` 门控维持现状 —— 与分页能力无关（判读记录）。**
  S9b 的任务文本把 `periLoadGeneration` 写为「未协商连接不得出现」的字段，与既有冻结契约冲突：该字段的既有门控是 `caps.replay`（`peri.replay`），只声明 `peri.replay` 的连接仍会收到它；
  §5.3 约束的是**分页私有字段**（`periEntryId`/`position`/`nextCursor`/`snapshotToken`）不得出现在未协商回放中，二者不同源。
  处置：不改门控（改动会破坏 4 处既有断言与协议文档已冻结形状）；未协商回放的字段集合已由 `.recon/S9b-acp-streaming-replay.md` 的用例断言。
  *后续修正见 D12 —— 独立验证把这条判读推翻了：门虽然按连接判定分页准入，但回放标记本身读的是会话注册表。*
- **D12 回放标记的协商判据改为「当前连接」（S9c，独立验证发现后修复）。**
  独立复核 `.recon/S9-verification.md` §7.1 实测复现：会话级 caps 注册表（`session/caps.rs:82` 的 `ensure_session_caps`）被先前的 peri 连接写过之后，同会话上的未协商标准 ACP 连接会收到 `periReplay`（4 条通知）与 `periLoadGeneration`（响应 + 通知）—— 违背需求 §5.3 第 153 行「能力版本以**当前连接**的协商结果为准，不能继承会话内保存的旧客户端能力」（A25「重连能力变化」）。分页四项字段未泄漏（它们只由连接事实决定）；`dispatch/session_replay.rs:25` 的模块文档本就要求按当前连接，实现与自家契约矛盾。
  处置：把回放标记（含响应 `_meta` 戳、reminder 戳）的 caps 来源改为 `ConnectionContext::caps()`（本连接协商值；未 `initialize` 的进程内路径保持 `all_enabled` 的 TUI 历史语义），分页准入的失败关闭不变，`send_available_commands_update` 保持会话级语义并加注释；被 `#[ignore]` 的反例探针转成回归用例。产物：`.recon/S9c-caps-gate-fix.md`、`.recon/S9c-verification.md`。
  复核结论（`.recon/S9c-verification.md` 判定摘要 6/6 PASS）：六个私有回放／分页字段（`periReplay`、`periLoadGeneration`、`nextCursor`、`snapshotToken`、`periEntryId`、`periPosition`）对未协商连接**无残留**；协商连接与进程内路径行为不变（TUI/MPSC 仍 `all_enabled`；SDK 已声明 `peri.replay`，stdio 路径无回归）。负面控制证明门被钉在两个方向上（未协商不泄漏、协商不丢失）。
  可见行为差异（须知）：未协商连接在「历史里有 client-audience reminder」时收到 `system-reminder-fallback`（纯文本 + `legacy: true`）而非结构化 `system-reminder` —— 同属回放相关的私有形状，与 D12 同向。
  复核点名的范围外边界（本轮不修，均**不是回放字段**）：① `_meta.peri.subagentFailure` 在回放与 live 两条路都**无门控**（`event/tool_projection.rs:33-37`，前序切片既有行为）；② 命令广播面（`send_available_commands_update`、`session/resume`）仍读会话级注册表；③ live 通道 reminder 形状仍读会话级注册表（`session/event_sink.rs:180-197`，回放出口已改）。三者与 §5.3 同类形状但超出本需求点名的回放字段范围。
- **D13 S7 前端复核结果与缺陷处置（TUI P1 + SDK P2，S7c 修复轮）。**
  TUI（`.recon/S7-tui-verification.md`）：REQ-C02（Compact 不再强制 reload）PASS、区间合并/并发合并/历史投影模式/不触碰执行状态 PASS；**REQ-C01-1 FAIL** —— `request_older_page_if_due` 只挂在渲染 `use_effect` 的 7 个 deps（`message_area/mod.rs:415-423`）上，deps 不含滚动偏移/follow_bottom，而框架在 deps 相等时不执行 → **空闲会话里滚到顶端不会取更早页**，失败重试同样无触发源（运行中的 turn 掩盖了它）。另有 D2（rewind 不失效浏览状态，P2）、D3/D5/D8（区间兜底槽位、页 RPC 无超时、断言可能真空）、D6（锚点用行数增量近似，REQ-C01-3 字面未满足）、D4/D7（工具终点错配、文档未同步）。
  SDK（`.recon/S7-sdk-verification.md`）：能力声明、A22 代次暂存/一次安装/失败丢弃、合并主体、架构未扩大 PASS；**A12「完成／失败状态正确」FAIL** —— 手动 Compact 失败（`command_feedback` error/cancel）后 SDK 无失败展示且 `compactionStatus` 停在 `running`（切片用例用 turn 失败冒充 Compact 失败掩盖了缺口）；另有风险项：初始回放不再进 `session.stream()`（行为变化未记录）、页/回放两条路径的块身份不一致（`test.failing`）、`nextCursor === null` 与「未协商」共用 reason、`historyOversize`/`AWAITING_RESULT_STATUS` 无公开出口。
  处置：S7c 修复轮（TUI 8 条 + SDK 5 条，含「必须能失败的测试」要求），随后双路独立复核。产品级「仍 PARTIAL」条目（如锚点机制）在 S7c 报告与 S10 验收报告里如实标注，不假装达成。
  S7c 复核结果：**TUI**（`.recon/S7c-tui-verification.md`）D1/D2/D3/D4/D5/D8 由 FAIL 转 PASS（每条都有证伪对照：把修复点退回去，用例即 FAIL），D6 锚点为「已量化 + 已计价」的 PARTIAL（插入点在视口上方时同单元屏幕行不变；插入点在视口下方时会整体推动 = 插入高度），D7 部分完成（code-index 已同步，集成用例未登记）；全量 lib 1807 passed / 0 failed。**SDK**（`.recon/S7c-sdk-verification.md`）8/8 PASS（Compact 失败收敛走真实 `command_feedback` 载体、`test.failing` 转正、三种 reason 可区分、导出与 README/code-index 一致）；残留 P3：`compact_started` 之前的真实 Compact 失败在 SDK 侧仍无展示（非本轮引入）。
  流程记录：S7c 的 TUI 修复 agent 在 200 轮迭代上限被中断（未落报告，末次编辑一度使 lib test 不可编译），由独立复核者复现并做了一处最小测试修复（保留原意的显式失败，见复核报告 §10）；TUI 侧权威记录因此是**复核者报告**（`.recon/S7c-tui-fix.md` 不存在）。
  集成用例（D7）已在 S7d 补齐：`docs/verification/tui-integration-cases/06-transcript-ui.md` 新增 `P0-81`（滚动加载更早历史 + 迟到页/游标），README 执行顺序同步；复核判 PASS（编号/章节/链接/索引四项约定）。**编号变更（2026-10-14，追平 origin/main 时发现冲突）**：登记时编号为 `P0-77`，上游 PR #193/#194 已把 `P0-77～P0-80` 分配给 `16-clipboard-transport.md`，故本用例改号为 `P0-81`；`docs/code-index/peri-tui.md` 已同步，`.recon/` 内既有报告保留当时的 `P0-77` 写法（历史记录不改写）。
- **D14 锚点补偿不得跨层使用槽位坐标（S7d 引入 → S7e 重做）。**
  S7d 把补偿规则从「按总行数增量」改成「只补偿视口上方的插入高度」：几何判定经证伪证明有效，但实现**把桥接侧槽位坐标当成渲染侧槽位坐标**——`HISTORY_PREPEND_MARK` 存的是 `state.committed` 下标（`acp_events/history_page.rs:78`），消费方却用它索引发布后的 `snapshot.items`（`transcript.rs:117`）；发布前「相邻成功工具卡压成折叠组」（`render.rs:140/461/658`）会让槽位数变少（复核实测：桥接 4 槽 vs 渲染 3 槽），于是两个方向都会出错：过补偿（包络终点 == 渲染槽数 → 取整窗视觉总行数 → 视口被推到窗口底部）或不补偿（终点越界 → `None`）。
  处置（S7e）：改按**渲染侧身份重定位**——记录视口顶部可见单元的稳定身份 + 单元内位移，并合后用同一身份在新索引重查并令 `new_offset = new_visual_start - within_unit_offset`；身份消失则不补偿（安全方向）。跨层槽位包络语义从 `HISTORY_PREPEND_MARK` 移除。验收必须覆盖「插入把原有相邻工具卡合并成折叠组」形态（复核者的反例构造在 `.recon/S7d-verify-probe.rs.txt`）。产物：`.recon/S7d-verification.md`（根因判定）、`.recon/S7e-anchor-relocation.md`、`.recon/S7e-verification.md`。
  S7e 复核结论（`.recon/S7e-verification.md` 判据 1~8 全 PASS）：折叠组反例、拆组、场景 A/B 四形态锚点屏幕行均不变；跨层槽位路径在代码中已不存在（全量 grep + 逐调用点判定）；证伪成立（短路重定位后三条用例 FAIL，分别跳 1 / 3 / 4 行）；全量 lib 1817 passed / 0 failed。**已声明边界**：锚点单元**自身**被折叠或拆开时其身份在新窗口不存在 → 按设计不补偿（实测下方位移 0 行 / +4 行，模块文档显式声明；「锚点内容被压组」与「上方重排时下方不动」不可同时满足，属设计取舍）；另有静态残留风险：窗口内出现两枚**内容完全相同**的同类单元时判歧义 → 不补偿（未构造端到端反例）。
- **S9 两项独立验证的结论（`.recon/S9-verification.md`、`.recon/S9-boundedness.md`）**
  1. D7 三条主判据（完整回放 / 失败关闭 / 协商路径不变）实测达成；向前分段读取的有界性（索引定位、行触达 ≈ `limit+1`、段字节 = 写入字节、候选行正文不出库）实测成立。
  2. 离群项只有 D12（已修）。
  3. 有界性报告反驳/收紧了四类表述 —— 均不改语义，只改文档与记录：
     - **R1（实测反驳）** 回放中途 rewind 删掉 `tail` 行后 SQLite 复用其 rowid，新行落进 `(after, tail]` 并被本次回放交付、被删行静默消失 →「回放期间行集合同源」不成立。触发需同会话并发（一个连接流式 load、另一个 rewind）。修法（回放游标带结构版本 / 每段校验行集合同源）属设计变更，另开切片；doc 已按事实收紧。
     - **R2（措辞收紧）** `length(CAST(content AS BLOB))` 关掉 SQLite 的 length-arg 优化：阶段一仍会把候选行正文读进寄存器（不越过 SQL 边界、不进解码器）。
     - **R3** 分页反向读取的**索引项**触达随深度线性增长（表行仍有界）；`perf-evidence.md` §8「分页不受影响」指的是表行。
     - **R4** 恢复路径 O(线程行数) 未变（即 D10，需索引 = DDL 未授权）。

- **D15 初始游标锚点改为「最旧可展示条目」（S11；S10 审计发现 F1 后修复）。**
  S10 的对抗审计发现（F1，P1）：`dispatch::initial_history_cursor` 取「有效视图最旧一条 own payload」当锚点，**不检查可展示性**。subagent / fork / bg 子会话的自有区首条是身份 System 条目（`session/subagent/factory/spawn.rs:348-352`，全仓唯一 append System 的生产路径），它既不被 Full 排除（`compact_v2/full.rs:86-101`）、又永不进入客户端渲染 ⇒ 这类会话在一次 Full 之后 `nextCursor = null`（谎称没有更早历史），被压缩的原文在界面上完全不可达（受影响：A15、A20、A06）。
  修复（S11）：锚点改为 `payloads.iter().find(PersistedPayload::displayable_to_client)`——判据与分页出口装配 `items` 的过滤**同源**；有效视图里一条可展示条目都没有时退到**最新**一条自有 payload（同样不在首屏展示，故不重不漏），是否收尾仍由 `range_head` 判定。**不涉及 DDL、不改 JSON 形状、不改错误码与 caps 门控。**
  回归用例（经真实 `dispatch::initial_history_cursor` / `load_history_page`）：`the_initial_cursor_anchors_past_non_displayable_own_rows`（own = [System 身份, h1, h2] + 一次 Full → 锚点 = 摘要、h1/h2 随页链按历史正序交付、与首屏零重叠、读到范围起点收尾）、`the_fallback_anchor_keeps_an_archived_row_reachable`（有效视图零可展示条目 + 更老的归档可展示行 → 兜底锚点仍让那行可交付；独立复核建议后补）、`an_all_non_displayable_own_region_reports_no_earlier_page`（只有身份 System → `nextCursor = null`）。证伪：把实现临时改回 `payloads.first()` 时前两条 FAILED；独立复核者另用 7 个探针复现，旧实现下 5 红、修复后 7/7 绿。
  第一版的两层审计探针处置：wire 层那条已转正为上述回归用例；存储层 `peri-resources/tests/zz_audit_history_anchor_probe.rs` **已删除**（它复刻的是 ACP 侧判据，修复后会变成过期副本），同等覆盖由 ACP 侧真实 API 用例承担。
  **独立复核（`.recon/S11-verification.md`）：该修复可以交付**——判据 1/2/3/4/6/7 PASS、判据 5 PARTIAL（非阻断，已按建议补用例）；无生产可达的反例；不变量「被排除的可展示行 ⊂ 最旧那一段」经唯一写入者 `full.rs:96` + 探针断言验证。
  **本轮登记的既有边界（复核者 §10，与本修复无关）**：页链是「严格早于锚点」的单向链——若将来出现**非前缀排除**（只归档一条中间消息并追加摘要），比锚点更晚的归档行会不可达；当前写入者规则保证形状不可达，修复需另开切片（页链改成按位置区间浏览）。另有 `peri-resources` 冷开锁 5s 抖动（测试基建，预先存在，见蓝图 R8 同类记录）。实施记录：`.recon/S11-f1-fix.md`。

### 待办切片

| 切片 | 内容 | 依赖 |
| --- | --- | --- |
| R1-R4 | rewind 完整性修复（边界未应用不得发布、远端批内边界守卫、只读存储、`RewindCompleted` 事件改有效视图）、分页真有界读取、可比位置、A04/A05/A07 证据 | 完成（R4b 复核 9 项全 PASS） |
| S9 | 标准客户端完整回放（D7）：向前有界读取 + 流式回放 | 已落地（S9a/S9b），独立验证完成（主判据 PASS，1 项 PARTIAL → S9c） |
| S9c | 回放标记判据改为当前连接（D12）+ 验证报告的 R1/R2/R8/R9 文档与测试修正 | 完成（修复 + 独立复核 6/6 PASS） |
| S7 | 前端（REQ-C01~C03、REQ-H04 客户端侧）：TUI 取消 Compact 强制 reload、滚动分页与区间合并；SDK 增量合并、Compact/rewind 分流、按 `loadGeneration` 暂存与丢弃 | 已落地（S7a/S7b），双路复核完成（TUI 1 FAIL + SDK 1 FAIL → S7c） |
| S7c | 前端缺陷修复轮（D13）：TUI 触发接线/rewind 失效/超时/区间/锚点/文档；SDK Compact 失败收敛、stream 行为、块身份、错误语义、公开出口 | 完成（TUI 6 PASS + 2 PARTIAL；SDK 8/8 PASS；见 D13） |
| S7d | 锚点补偿改为「只补偿视口上方的插入高度」（A20 场景 B）+ 登记 TUI 集成用例（P0-81，登记时编号 P0-77，冲突后改号） | 完成：用例登记 PASS；锚点改动因跨层槽位坐标缺陷被复核判 FAIL（D14）→ S7e |
| S7e | 锚点改按渲染侧身份重定位（去跨层坐标），验收须覆盖折叠组形态 | 完成（复核判据 1~8 全 PASS；A20 成立，含已声明边界） |
| S10 | 终局测试矩阵 + 对抗审计 + A01~A25 验收报告 + 报告复核 | 完成（workflow `01a1377b`）；报告复核判定「需修订后可作验收依据」，已按 M1~M4 + S1~S5 出第二版 |
| S11 | 修复审计新发现的 F1（P1，初始游标锚点落在不可展示条目）+ 回归用例 + 独立复核 | 完成（见 D15；`.recon/S11-verification.md`） |

### 编排脚本（ultracode）

| 脚本 | 阶段 | 产物 |
| --- | --- | --- |
| `.recon/workflows/s9-standard-replay.mjs` | S9a 存储契约（`load_history_segment` + 首段继承区 + `tail` 上界）→ S9b ACP 未协商分支流式 canonical 回放 → 双路独立验证 | `.recon/S9a-storage-contract.md`、`.recon/S9b-acp-streaming-replay.md`、`.recon/S9-verification.md`、`.recon/S9-boundedness.md` |
| `.recon/workflows/s9c-caps-gate-fix.mjs` | S9c 回放标记门改按当前连接（D12）+ R1/R2/R8/R9 收尾 → 独立复核 | `.recon/S9c-caps-gate-fix.md`、`.recon/S9c-verification.md` |
| `.recon/workflows/s7-frontend-paging-and-compact.mjs` | S7a TUI ∥ S7b SDK → 双路独立验证 | `.recon/S7a-tui.md`、`.recon/S7b-sdk.md`、`.recon/S7-tui-verification.md`、`.recon/S7-sdk-verification.md` |
| `.recon/workflows/s7c-frontend-defect-fix.mjs` | S7c TUI ∥ SDK 缺陷修复 → 双路独立复核 | `.recon/S7c-sdk-fix.md`、`.recon/S7c-tui-verification.md`、`.recon/S7c-sdk-verification.md`（TUI 修复侧因 agent 迭代上限中断未落报告，权威记录为复核报告） |
| `.recon/workflows/s7d-anchor-and-integration-case.mjs` | S7d 锚点补偿（A20 场景 B）+ TUI 集成用例登记 → 独立复核 | `.recon/S7d-tui-anchor-and-cases.md`、`.recon/S7d-verification.md` |
| `.recon/workflows/s7e-anchor-identity-relocation.mjs` | S7e 锚点改按渲染侧身份重定位 → 独立复核（含折叠组反例与证伪） | `.recon/S7e-anchor-relocation.md`、`.recon/S7e-verification.md` |
| `.recon/workflows/s10-final-verification-and-report.mjs` | 终局测试矩阵 → 对抗审计 → 验收报告 A01~A25 → 报告复核 | `.recon/final-test-matrix.md`、`.recon/final-adversarial-audit.md`、`.recon/A-acceptance-report.md`、报告复核 |

D7 的存储契约要点（S9a 落地，供复核对照）：`HistoryForwardCursor{after,tail}` + `HistorySegment{payloads,next_cursor}`；首段返回全部继承区 payload + 最旧若干自有行并同源确定 `tail`；后续段 `rowid > after AND rowid <= tail` 升序推进；不过滤 `excluded`/System；两阶段读取（阶段一 id+rowid+length，阶段二只取本段正文）；锚点消失失败关闭；后端缺席返回 `Unsupported`，ACP 侧显式回退到 `load_session_history` 全量读取并记日志。
