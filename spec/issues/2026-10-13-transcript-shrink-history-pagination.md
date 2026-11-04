# 实现蓝图：Full Compact 后 Transcript 收缩与 ACP 历史分页

状态：原 REQ-T/H/C 范围交付完成（验收报告 `.recon/A-acceptance-report.md`）；两项待裁决已经对抗裁决结案、A05 索引经用户授权并实施、D8 已闭合（取代关系落在既有 `projection` 载荷，无 DDL；见 D17）。2026-10-17 起 S4–S8 扩展见 §8：窗口协议默认关闭，S7.3 详情与 S7.4 inherited 已交付（限制见 §8.4），S6.3/S6.4/S6.5 与 remote/emscripten 仍未完成。基线 `7f0de56e8`（worktree `.worktrees/peri-transcript-history`，分支 `feat/transcript-shrink-history-pagination`）。

> **历史文档**：本文描述的 `history-window-v1` beta 开关与 `peri.historyWindowV1`
> 私有窗口协议已随 History V2 删除；History V2 是本地 TUI 的唯一历史路径，不存在
> “关闭 beta 回滚旧协议”的路径。以下涉及该键的内容均为历史记录，不代表当前行为。

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

### 已落地（核心三 commit 已提交：`95c1e07d7`/`9b96f456a`/`3ccf75864`，分支已追平 `origin/main` `19215d3a1`；S12/S13 收尾见 D16，由第四次提交落地）

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
  精确修复需新增「覆盖批次」最小元数据（属 DDL）；按仓库规则（根 CLAUDE.md 原则 12）须用户明确批准，批准前不新增列。记录为已知偏差，授权后另开切片；`peri-acp-types/src/store/history.rs` 的模块文档与 `.recon/rewind-review-findings.md` 已注明。→ **第五版已闭合，且未使用任何 DDL，见 D17.5。**
- **D9 手动 Compact 读取 canonical 快照 —— 已被 2026-10-17 的 S4/S8 取代。**
  原判读：`compact_pipeline.rs` 从完整快照重建临时 transcript（含归档正文）用于 Compact 执行；Full 的摘要输入取有效视图、assembled 输出取有效视图，归档正文既不进摘要也不回写热态，属读取成本而非语义泄漏（REQ-T03 约束的是恢复路径）。**现行实现**：`compact_pipeline` 已改用 `load_active_snapshot`，只读有效 own payload + 冻结 inherited 区；归档正文不进入本次读取/解码。调用方仍带回完整历史时，多出的 id 只在有界 flags 确认 `excluded` 后忽略；未知 id/读取失败拒绝。有效集为空用 flags 区分全新 thread 与全归档。旧“读取成本”描述不再成立，保留此条仅作决策历史。
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
  集成用例（D7）已在 S7d 补齐：`docs/verification/tui-integration-cases/06-transcript-ui.md` 新增 `P0-82`（滚动加载更早历史 + 迟到页/游标），README 执行顺序同步；复核判 PASS（编号/章节/链接/索引四项约定）。**编号变更（2026-10-14，追平 origin/main 时发现冲突）**：登记时编号为 `P0-77`，上游 PR #193/#194 已把 `P0-77～P0-80` 分配给 `16-clipboard-transport.md`，故本用例改号为 `P0-81`；**再改号（2026-10-16，合并最新 main 时）**：main 的 #230 已把 `P0-81` 分配给 `17-setup-wizard.md`（首次配置向导），故本用例再改号为 `P0-82`；`docs/code-index/peri-tui.md` 已同步，`.recon/` 内既有报告保留当时的 `P0-77` 写法（历史记录不改写）。
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

- **D16 S12/S13 收尾轮的决策与处置（2026-10-14）。**
  1. **拒绝分支持久化**：第一轮的临时探针（未跟踪）转为持久 wire 用例（`history_page_rejection_test.rs` 的 R6/R9/R10，真实 `ServerLoop` + MPSC transport + `dispatch_request`）；拒绝/失败路径**类别覆盖 11/11**（不是参数变体穷尽）；存储侧断言只比较条数，**等长改写不检出**（已披露，不列为 PASS 依据）。
  2. **A14 最小直接用例**：`a_parent_full_after_fork_leaves_the_child_snapshot_and_pages_intact`（`peri-acp/src/dispatch/session_fork_test.rs:332`，走真实 `apply_compaction`，注入父压缩目标为 `fork_id` 后用例红）；A14 由「未验证」降为 **PARTIAL**。覆盖边界：3 条父自有消息、首页一次读尽（`next_cursor = None`）、无「fork 前 Full」、无多页游标链、无冻结祖先。旧记录的「第二次 Full」更正为「fork 后单次 `apply_compaction`」。
  3. **锁回归重做**：显式握手建立确定顺序；四处真实释放探测统一走 `probe_initialization_lock_release`（`LOCK_RELEASE_PROBE_BUDGET = 250ms`，预算唯一来源），回退该入口则用例 3/3 红。**残留缺口（独立复核实测，A1 判 FAIL）**：四处调用点绕过共享入口各自内联零等待时用例仍绿 ⇒ 守卫面是「预算的唯一来源」，调用点级绕过为**已声明局限**；250 ms 为容忍窗口（外部持锁 100 ms 绿 / 350 ms 红）；生产锁路径未改。
  4. **规模治理（STD-SIZE-001）**：本交付触及且超 1000 行的 10 个文件按职责拆分（`peri-acp` 3 个测试文件 → 父 + 子模块；`peri-acp-types`/`peri-agent` 契约与 transcript；`peri-tui`/`peri-resources` 4 个），断言与公开路径零改动。独立复核：183 个非 `.recon` 文件全部 ≤1000（最大 990），拆分/挂载/断言三项 PASS；「含 `.recon` 证据全集」口径的 FAIL 由本节作者裁决为「证据归档不适用 STD-SIZE-001」——存留的 6 个 `.rs.orig` 副本经 sha256 与拆分前提交 `3ccf75864` 逐字节一致（无独有信息），两份超限 `.rs` 副本已删除；`git diff --check` 已修至 exit 0。
  5. **WASM 基线修复保留**：`langfuse-client/src/batcher/recovery.rs:256` 无线程目标就地编码（修基线 `7f0de56e8` 引入的 telemetry panic）+ SDK 夹具按 gzip magic 解压（产品契约未动）；现状 SDK `bun test` **254 pass / 0 fail**、native `langfuse-client --lib` 292/0/1。**红→绿链不可独立复核**（三次构建与红灯日志已删除；复核判 NOT-VERIFIED，两方一致建议保留）。
  6. **A05 仍 FAIL（待用户裁决）**：收窄需索引 = DDL 未授权；S13 只读实验（VM 指令计数）证明「固定 rowid 后缀下界」在 rewind 后**丢行**，不可采用。批准索引前 A05 保持 FAIL。→ **已裁决并实施，见 D17.1。**
  7. **`peri.sessionWorkspaceV1` 第四处同类门控差异（待用户裁决，未改）**：5 处读取走进程级 `SessionManager.pending_caps`（`session/caps.rs:12`、`effective_host_caps:31-37`），形状为「连接 A `initialize` 声明过 → 连接 B 从未 `initialize` 也拿到 workspace 字段」。最小修复 = 改读连接级 `ConnectionFacts`（失败关闭，同 `historyPagination`），但会使 TUI 进程内路径丢 workspace 字段 = **行为变更**，须单独批准。→ **已裁决（维持实现），见 D17.2；该条对 TUI 的成本描述经两方对抗复核后推翻。**
  8. **DDL 越界如实披露**：S12 侦察沿用前序 A2 的一次性实验脚本时，曾在**副本库**（`.recon/a2-raw/probe-ab.db` 等，非仓库路径、非任何 Peri 会话库）执行 `CREATE INDEX idx_active_b ON messages(thread_id) WHERE excluded = 0`（`index-probe.sql:16/21/24` 的 CREATE/DROP 序列；`S12-verification.md` §2.7 观察到残留）。用户约束为「不授权直接执行数据库 DDL」，**无实验副本豁免** ⇒ 记为越界；此后全部实验只读。生产 schema 在本条记录时点**未新增任何列/表/索引**（2026-10-14 用户显式授权 A05 索引后另计，见 D17.1）。
  9. **流程边界**：复核发现旧记录低估（`S12-gap-tests.md` 的「9/11」实为 **8/11**；A14「第二次 Full」表述不成立），已按 S13 修订记录更正；本蓝图不复述错误口径。
- **D17 两项待裁决的对抗裁决与 A05 索引授权（2026-10-14，用户指令：提交代码、A05 新增索引、第 2/3 项派 subagent 对抗决策、推送远程）。**
  1. **A05 索引：用户显式授权，已实施。** 形状与 A2 实验同形：`(thread_id, excluded)`。落点是 canonical 单一来源——`peri-resources/src/sessions/canonical.rs` 的 `CREATE_INDEXES` 追加 `CREATE INDEX IF NOT EXISTS idx_messages_active ON messages(thread_id, excluded)`；本机 `sqlite_store/schema.rs` 的 **Current 分支**补一次幂等索引重放（形状判定只覆盖表与列，否则上一版构建建出的库永远拿不到新索引）；新建、v11 迁移终点、远端初始化与远端升级批次全部经同一清单，无需各自改动。新增用例 `current_shape_open_backfills_a_missing_canonical_index`（删索引 → 重开 → 补回 + 版本不变）。证据：`.recon/a05-raw/perf-rerun.out`、`.recon/perf-evidence.md` §8 复跑与验收报告 §5.18.3。**授权范围仅此一条索引**，其余 DDL 仍未授权。
  2. **`peri.sessionWorkspaceV1` 门控：裁决「实现零改动 + 文档订正」**（两立场对抗，依据见验收报告 §5.18.1）。两方独立推翻侦察阶段前提「TUI 不 initialize」（TUI 在 `kit/entry.rs:417` 无条件 await `register_ui_commands` ⇒ 发 initialize）；跨连接污染无可达生产路径（正方自认只有夹具可达）；改连接级 `caps()` 对「未 initialize 的连接」零行为差别。订正两处文档：`docs/design/peri-acp-protocol.md` 写明门控事实源是进程级协商值及其与分页按连接判定的差异理由；`peri-tui/src/acp_client/client/requests.rs` 的过期注释。**再开启条件**：同进程多连接生产形态出现、外部客户端跳过 initialize 仍被证实真实消费扩展、TUI initialize 常态失败、需求方要求按连接判定（届时与 `oauth` 家族一次改）。
  3. **锁守卫面：裁决「采纳独立断言下界、拒绝结构性闭合、保留调用点级局限」**（依据见验收报告 §5.18.2）。实测：预算调到 20 ms 时原判别断言仍**绿**（参照值与阈值同源 ⇒ 判别力可被一个数字静默取消）；改用独立下界 `PROBE_WAIT_FLOOR = 200ms` 后同一注入实验**红**（`waited 22.289458ms < 200ms (budget 20ms)`），复原 250 ms **绿**。拒绝正方方案 A/C：代价是移动 production 模块私有原语 + 18 处机械改写，收益仅把「改一个词」抬成「改结构 + diff 可见」，且反方指出的残余（新增入口 / `Duration::from_nanos(1)` / `std::fs` 原生 API）不可消除；同文件内合法零等待与绕过写法逐字同形，文本守卫需行号白名单（本仓无源码扫描守卫先例）。危害评估采纳反方：绕过方向是「更敏感」（fork 继承窗口报假红）而非「更沉默」，真实泄漏在零等待下同样必红。独立复核的硬判据「真实四调用点回退可被检出」**仍未满足**，如实保留 PARTIAL；入口注释已写明「内联零等待不在守卫覆盖内」。
  4. **推送**：A05 索引与两项裁决由第六次提交 `5e691a2ac` 落地，报告第四版与哈希补记随后提交并在同一次推送（新建 upstream）发到 `origin/feat/transcript-shrink-history-pagination`（见验收报告 §0.1b）。
  5. **D8 闭合：取代关系落进既有 `projection` 载荷，不新增列、不推进 schema 代数（2026-10-14，用户指令：授权其余 DDL 用于闭合 D8、完成工作后新建 PR）。** 新列路线经两条独立侦察否决：①已存在的 Current 库会在只读 probe 阶段被拒（`schema_shape.rs::check_strict` 等长逐位同名 + `sqlite_store/schema.rs` 的 Current 判定失败后走 input 判定 ⇒ fail-closed），且按仓库自述「形状变化一次推进一代」需同时推进本机第 12 代与远端 v6（同步点 30+ 处）；②库变新代后**旧二进制**会把多余列读成漂移并拒绝打开，回归不可消除。改法：`MessageProjectionDirective` 新增 `#[serde(default, skip_serializing_if = "Option::is_none")] superseded_by: Option<MessageId>`（写入时 `entries` 为空 ⇒ 无渲染动作，只在 `excluded` 为真时被读取）——该列已在本机与远端两侧的 SELECT/INSERT/UPDATE 与解码链上贯通，新字段读写零改造、两向兼容。实现四面：Full 归档写取代关系（摘要 id 提交前分配、追加消息用同一 id）、`rewind_clear_excluded_ids` 逐条精确判定（无记录旧行回退）、本机 `clear_rewind_excluded` / 远端 `CLEAR_EXCLUDED_SQL` 清零时连带清掉整条载荷、fork 重映射含 `superseded_by`。证据与残留见验收报告 §5.1；**用户授权的其余 DDL 本轮未被使用**。

### 待办切片

| 切片 | 内容 | 依赖 |
| --- | --- | --- |
| R1-R4 | rewind 完整性修复（边界未应用不得发布、远端批内边界守卫、只读存储、`RewindCompleted` 事件改有效视图）、分页真有界读取、可比位置、A04/A05/A07 证据 | 完成（R4b 复核 9 项全 PASS） |
| S9 | 标准客户端完整回放（D7）：向前有界读取 + 流式回放 | 已落地（S9a/S9b），独立验证完成（主判据 PASS，1 项 PARTIAL → S9c） |
| S9c | 回放标记判据改为当前连接（D12）+ 验证报告的 R1/R2/R8/R9 文档与测试修正 | 完成（修复 + 独立复核 6/6 PASS） |
| S7 | 前端（REQ-C01~C03、REQ-H04 客户端侧）：TUI 取消 Compact 强制 reload、滚动分页与区间合并；SDK 增量合并、Compact/rewind 分流、按 `loadGeneration` 暂存与丢弃 | 已落地（S7a/S7b），双路复核完成（TUI 1 FAIL + SDK 1 FAIL → S7c） |
| S7c | 前端缺陷修复轮（D13）：TUI 触发接线/rewind 失效/超时/区间/锚点/文档；SDK Compact 失败收敛、stream 行为、块身份、错误语义、公开出口 | 完成（TUI 6 PASS + 2 PARTIAL；SDK 8/8 PASS；见 D13） |
| S7d | 锚点补偿改为「只补偿视口上方的插入高度」（A20 场景 B）+ 登记 TUI 集成用例（P0-82，登记时编号 P0-77，两次冲突后改号） | 完成：用例登记 PASS；锚点改动因跨层槽位坐标缺陷被复核判 FAIL（D14）→ S7e |
| S7e | 锚点改按渲染侧身份重定位（去跨层坐标），验收须覆盖折叠组形态 | 完成（复核判据 1~8 全 PASS；A20 成立，含已声明边界） |
| S10 | 终局测试矩阵 + 对抗审计 + A01~A25 验收报告 + 报告复核 | 完成（workflow `01a1377b`）；报告复核判定「需修订后可作验收依据」，已按 M1~M4 + S1~S5 出第二版 |
| S11 | 修复审计新发现的 F1（P1，初始游标锚点落在不可展示条目）+ 回归用例 + 独立复核 | 完成（见 D15；`.recon/S11-verification.md`） |
| S12 | 收尾轮：拒绝分支转持久用例、A14 最小用例、冷开锁回归初查、WASM 基线修复、A05 侦察、冒烟与基线 | 完成（workflow `01a137f4`）；独立复核 `S12-verification.md` 判 NEEDS-REVISION → S13 |
| S13 | 修订轮：A14 复核、拒绝持久用例（11/11 类）、锁回归重做、规模治理（10 文件按职责拆分）、WASM 闭环补做、两份独立复核 | 完成（workflow `01a13888`）；残留见 D16：锁调用点级缺口、WASM 红→绿不可独立复核、A05 与 workspace 门控待裁决 |

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
| `.recon/workflows/s12-remaining-items.mjs` | S12 收尾（拒绝持久化、A14 用例、锁初查、WASM 基线修复、A05 侦察、冒烟与基线）→ 独立复核 | `.recon/S12-*.md`（8 份，含 `S12-verification.md`） |
| `.recon/workflows/s13-revision-round.mjs` | S13 修订（wire 拆分与拒绝持久用例、锁重做、A14 复核、规模治理、WASM 闭环）→ 两份独立复核 | `.recon/S13-*.md`（9 份，含 `S13-verification-size.md`、`S13-verification-behavior.md`） |

D7 的存储契约要点（S9a 落地，供复核对照）：`HistoryForwardCursor{after,tail}` + `HistorySegment{payloads,next_cursor}`；首段返回全部继承区 payload + 最旧若干自有行并同源确定 `tail`；后续段 `rowid > after AND rowid <= tail` 升序推进；不过滤 `excluded`/System；两阶段读取（阶段一 id+rowid+length，阶段二只取本段正文）；锚点消失失败关闭；后端缺席返回 `Unsupported`，ACP 侧显式回退到 `load_session_history` 全量读取并记日志。

## 8. S4–S8 窗口协议与默认关闭（2026-10-17）

### 8.1 状态

| 切片 | 状态 | 说明 |
| --- | --- | --- |
| S4 窗口 wire/coordinator | 已实现（默认关闭） | `peri.historyWindowV1`、open/page/seek、body 分块、ticket、`peri/history_committed`；定向验证见 `.peri/plans/s4-verification-round3.md`（PASS，含残余覆盖缺口） |
| S5/S6 TUI 窗口/预算/释放 | 已实现（默认关闭） | 段模型、页预留、接纳/淘汰、`ReleasePlan`、统一释放、`structural_epoch`/render ack、8/6 MiB 缓存；S6.3/S6.4/S6.5 未完全达成 |
| S7.1/S7.2 描述符/分块 | 已实现（本机 SQLite） | 服务端 descriptor 与有界 body 分块；TUI 详情面板已交付（S7.3） |
| S7.4 冻结继承 | 已实现（本机 SQLite，协议默认关闭） | inherited source/冻结读取/父变更隔离；TUI 继承详情已交付；remote/emscripten 未实现 |
| S7.3 TUI 详情/超大条目/继承 | 已交付（默认关闭） | `PanelKind::HistoryDetail`/`HistoryDetailPanel`、占位 note Enter 打开、Esc/q 关闭、滚动补块、`e` 复制已加载正文、`session/history/body` 分块与 4 MiB 预算、inherited 详情走冻结 snapshot；交互限制见 §8.4 |
| 配置入口 | 已删除（历史记录：当时默认 false） | settings beta `history-window-v1` 曾在启动读取一次并写 `HISTORY_WINDOW_V1`；该键与 `peri.historyWindowV1` 协议已随 History V2 删除，不存在关闭 beta 回滚旧协议的路径 |
| S8 文档与契约同步 | 本次完成 | 只改 `docs/**`、`spec/**`；不涉及代码、Cargo、`.peri/**` 或子模块 |

### 8.2 已落地范围

1. **所有权收敛**：Host/Executor 单方持有有效上下文。`SessionState.history` 已删除，保留 `history_payloads` + `ContextOwnership::{Resident,HandedOff,ReloadRequired}`；`HandedOff` 的空集合不是空会话，`ReloadRequired` 从存储冷恢复，`finish_prompt_turn` 不再因 turn 结束移除 session。
2. **手动 compact 只读有效集**：`compact_pipeline` 使用 `load_active_snapshot`；身份按 inherited+own 有序消息投影校验并拒绝重叠；额外已归档 id 仅在 flags 确认后忽略；有效集为空区分全新 thread 与全归档；提交失败/不确定进入 `ReloadRequired`；磁盘已提交前缀是唯一权威。
3. **窗口协议**：`session/load` 的 `history`（`open/page/seek`）与旧顶层 `cursor/limit/loadGeneration/cwd/mcpServers` 互斥；响应含 generation/structure/snapshot/tail/checkpoint/readTicket/双向 cursor/items/inherited/rewindInvalidation；oversize 只在 `items[].body` 内联 descriptor；`session/history/body` 有界分块；`peri/history_committed` 私有通知按 checkpointId 幂等且只发持票连接；ticket per-connection、单活动票据、新 open 成功才原子替换、失败保留旧票据；page/seek/body 二次校验；rewind durable 成功恰好 `structureVersion+1` 并撤销 ticket；继承使用 `threads.inherited_context` 冻结列与边界派生 snapshotId。
4. **TUI 窗口**：窗口模型/预算/接纳/淘汰（`ReleasePlan` 不直接删正文）、段窗口合并进 `committed`、双向滚动/gap、`HistoryCommitted` 封印 durable 段、统一释放事务（DisplayLedger/HistorySpans/ResidentIndex/WINDOW_UNIT_KEYS/FoldedHistory/渲染缓存/pending tool results）、`structural_epoch` + 渲染 ack、8/6 MiB 缓存、对象级 Weak 断言。
5. **默认关闭**：`PeriCaps::history_window_v1` 与 TUI `HistoryWindowConfig::history_window_v1` / `HISTORY_WINDOW_V1` 默认 false；关闭时旧 `HISTORY_PAGING` 路径行为不变。TUI 的 initialize 仍以 `PeriCaps::all_enabled` 声明能力，但不等于启用窗口模型/淘汰。
6. **S7.3 详情与配置入口**：TUI 交付 `PanelKind::HistoryDetail` + `HistoryDetailPanel`，占位 note 命中后 Enter 打开，Esc/q 关闭，Up/Down/PageUp/PageDown 滚动并在缺块时请求，`e` 复制已加载正文；`load_history_body` 走 `session/history/body`，支持 UTF-8 跨块重组、4 MiB 预算、关闭释放 chunks。settings beta `history-window-v1`（默认 false，已随 History V2 删除）曾在 TUI 启动读取一次并写 `HISTORY_WINDOW_V1`，无热重载；服务端 `peri.historyWindowV1` 同时为真的旧协议已删除。
7. **inherited 对齐**：`HistorySource::Inherited{frozen_snapshot_id, ancestor_thread_id}`、open 响应 `inherited` 对象、inherited 条目 `position:null`、inherited 段固定不释放、详情走冻结 snapshot；`ancestor_thread_id` 来自 `parent_thread_id`（旧数据 None），snapshotId 按边界 SHA-256 派生；`inheritedUnsupported` 显式提示。服务端 SQLite 使用 `json_each` keyset 分页/seek 与 `substr` 分块。

### 8.3 验收映射

对应关系（按 S5/S7 计划）：A12 ← TUI 预算/接纳/淘汰与释放；A13 ← 双向页/seek；A14 ← 二次校验/tail/提交通知/段封印；A15 ← 迟到页；A16 ← rewind；A17 ← 锚点身份重定位；A20 ← 释放/渲染 ack；A22 ← 描述符/分块；A26 ← 继承。A17 的既有验收来自历史分页（`viewport_anchor_e2e_test.rs`），本轮未重跑；A23/A24/A25 未在本次定向验证中单列，不宣称完成。P01/P03 未验证。

| 项 | 证据（真实名称） | 结论 |
| --- | --- | --- |
| 窗口协商/请求/响应/错误码 | `peri-acp-types/src/history_window_test.rs`：`open_defaults_to_latest_and_both_sides`、`page_requires_exactly_one_cursor_and_a_valid_limit`、`unknown_fields_and_kinds_are_not_degraded_to_initial_load`、`error_codes_and_reasons_match_the_wire_table`；`peri-acp/src/host/history_page_window_test.rs`：`open_latest_pages_backwards_and_reports_metadata`、`unnegotiated_window_request_is_rejected_without_initial_load`、`initialize_request_controls_history_window_v1` | PASS（默认关闭） |
| 二次校验/旧票据保留/tail | `second_check_rejects_stale_generation_structure_ticket_and_snapshot`、`failed_open_keeps_the_old_ticket_usable`、`fixed_tail_keeps_later_appends_out_of_an_open_snapshot`、`handed_off_and_reload_required_are_not_empty_history` | PASS |
| 正文描述符/分块 | `oversize_descriptor_serves_bounded_body_chunks`；`session_history_window_test.rs`：`oversized_entries_become_descriptors_without_decoding_the_body`、`body_chunks_are_bounded_and_reconstruct_the_exact_bytes`、`body_chunk_is_bounded_by_the_snapshot_tail`；TUI `history_detail_test.rs`：`detail_chunks_cross_utf8_boundary_without_garble`、`detail_close_releases_chunk_allocations`、`detail_budget_keeps_at_most_4mib`、`detail_allows_one_in_flight_request` | 服务端/TUI PASS |
| 详情面板打开/滚动/继承 | `history_detail_test.rs`：`open_close_detail_uses_window_descriptor_and_keeps_slot_state`、`event_layer_opens_detail_from_placeholder_and_clears_on_close`、`detail_scroll_event_path_requests_next_chunk_without_main_viewport_change`、`inherited_detail_request_keeps_frozen_source`、`detail_panel_lines_show_loading_error_and_inherited_notice` | PASS（交互限制见 §8.4） |
| 提交通知 | `history_commit_notification_is_gated_by_an_open_window_and_idempotent`、`open_after_durable_full_reads_checkpoint_from_storage`、`tickets_and_commit_notifications_are_bound_to_the_opening_connection` | PASS |
| rewind | `rewind_revokes_the_window_and_advances_structure_once`、`initial_load_refreshes_generation_and_invalidates_old_window`、`rejected_initial_load_keeps_generation_and_ticket` | PASS |
| 短锁 load/resume/fork | `history_page_window_lock_test.rs`：`live_load_does_not_hold_the_global_sessions_lock_across_io`、`cold_load_does_not_hold_the_global_sessions_lock_across_io`、`resume_does_not_hold_the_global_sessions_lock_across_io`、`fork_does_not_hold_the_global_sessions_lock_across_io`；`connection_test.rs`：`resume_wire_uses_the_shared_preparation_path`、`fork_wire_uses_the_shared_preparation_path` | PASS（残余关闭竞态见 §8.4） |
| 手动 compact 有效集 | `peri-acp/src/session/command/compact_persistence_test.rs`：`test_compact_pipeline_accepts_effective_only_history_for_second_lifecycle`、`test_compact_pipeline_rejects_incoming_history_that_differs_from_bound_thread`；`compact_command_contract_test.rs`：`test_compact_again_using_host_canonical_history` | PASS（有效集路径） |
| TUI 预算/接纳/淘汰 | `history_window/admission_test.rs`：`over_budget_admission_evicts_farthest_segment_first_and_reports_plans`、`all_pinned_segments_reject_admission_and_report_reasons`、`stale_page_generation_is_rejected_and_reservation_is_released`、`one_pending_merge_page_blocks_the_next_admission`；`model_test.rs`：`release_target_uses_watermark_and_remaining_capacity`、`disabled_window_does_not_admit_pages` | PASS（模型/账本） |
| 段封印/保留/迟到页 | `acp_events_test/history_window_commit_test.rs`：`test_history_committed_seals_window_without_touching_execution_state`、`test_compact_completed_does_not_seal_or_release_window_segments`、`test_turn_done_does_not_seal_live_window_segment`、`retention_plan_keeps_viewport_and_one_adjacent_segment`、`open_after_lost_commit_notification_realigns_window`；`history_page_consumer/window_test.rs`：`bidirectional_page_requests_keep_placement_and_cursors`、`stale_or_late_response_is_dropped_and_reservation_released`、`reopen_required_enters_degraded_and_retry_uses_open_not_full_load` | PASS（默认关闭） |
| 释放/渲染 ack/对象证据 | `acp_events_test/history_window_release_test.rs`：`release_removes_committed_and_invalidates_holders_before_ack`、`release_does_not_touch_live_phase_or_input_queues`、`release_ack_is_idempotent_and_late_ack_does_not_double_credit`、`pending_release_keeps_budget_conservative_until_ack`；`message_area/window_viewport_test.rs`：`structural_epoch_drops_old_slot_lines_and_uses_8mib_to_6mib_budget`、`structural_epoch_with_empty_snapshot_reaches_zero_retained_bytes` | PASS（对象级证据）；S6.3/S6.4 仍有边界 |
| 继承 | `history_page_window_test/inherited_test.rs`：`inherited_open_page_seek_and_chunks_use_the_frozen_snapshot`、`inherited_absence_and_ancestor_mismatch_are_explicit`、`rewind_invalidates_old_inherited_ticket_but_frozen_prefix_remains_readable`；`session_history_inherited_test.rs`：`inherited_bounds_report_the_frozen_range_and_snapshot_identity`、`inherited_page_seek_and_chunks_are_bounded_and_isolated_from_parent_changes`；TUI `inherited_detail_request_keeps_frozen_source` | 服务端/SQLite/TUI 详情 PASS；remote/emscripten 未实现 |
| 配置入口 | `peri-config/src/betas_test.rs`：`test_history_window_v1_beta_defaults_false_and_accepts_override`；`peri-tui/src/kit/entry.rs` 启动读取 + `history_window::apply_history_window_beta` | PASS（默认 false；启动只写一次；Beta 面板未列，需手动 settings） |
| P01/P03 性能曲线 | `session_history_perf_test.rs` 的 `history_perf_tests` 为 `#[ignore]`；本轮未重跑 | 未验证，不宣称 |

### 8.4 未完成/限制

1. **S6.3 折叠组/子 Agent 组未真正惰性化**：当前构造通过 `im::Vector` 结构共享、不深复制正文；释放只 `FoldedHistory::invalidate()` 与整组缓存失效，没有按段惰性物化/精准清理。完成条件：组按段惰性物化与释放，A19 对象级断言通过。
2. **S6.4 结构性失效清全部 slot**：`Transcript::prepare` 观察到 `STRUCTURAL_EPOCH` 变化时清空全部渲染缓存，尚未按段精准清理。完成条件：按段清理 slot/index 并保留未受影响缓存，Weak 行缓存断言通过。
3. **S6.5 旧分页 pending 表未迁移**：旧 `HISTORY_PAGING` 路径仍使用轻量静态 `PENDING_TOOL_ENDS`（1 MiB/256 条），未完全迁移到 `ResidentIndex`；窗口路径共享清理入口。完成条件：旧分页也经 `ResidentIndex` 配对，删除静态表并补跨段配对用例。
4. **详情交互限制（S7.3 已交付）**：鼠标单击只设焦点，需再 Enter 打开；`e` 只复制已加载正文，文件导出/超窗流式导出尚未接入 UI（模型 `export_stream` 已支持分块）；未做 JSON 字段级展示；inherited 段内部不插入独立占位行，使用全局 inherited notice；TUI 仍接受旧顶层 `bodyDescriptors` 兼容输入，服务端已不再发送。完成条件：按需接入文件/流式导出、字段级展示与段内占位，或明确保持当前交互。
5. **Beta 面板未列 `history-window-v1`（历史；该键已删除）**：settings beta 已删除，TUI 不再读取该键；历史记录：TUI 启动读取一次并写 `HISTORY_WINDOW_V1`，无热重载，但 ConfigPanel 只列 ACP `BETA_FLAGS`，该键需手动写入 settings。该完成条件随旧协议删除失效。
6. **remote/emscripten**：未覆写窗口/继承端口，默认 `Unsupported`（出口 `-32603`），不宣称已支持。能力协商是连接级客户端声明，服务端当前不按后端关闭协商；因此“协商成功”不等于该后端可读，实际读取失败必须显式处理。完成条件：adapter 覆写端口并通过 open/page/seek/body/inherited 契约测试。
7. **ticket 无密码学签名**：进程内 opaque 值 + 精确匹配，绑定 connection/session/generation/structure/snapshot/source/tail/投影版本/时间/nonce；不是访问授权。当前票据只在进程内使用；若未来跨进程/网络读取，需另行设计签名/认证，不能沿用当前 opaque ticket。
8. **rewind uncertain 依赖文本 marker**：`PERSISTENCE_UNCERTAIN_MARKER` 集中于 `host/requests/rewind.rs`，尚无 typed mutation outcome。完成条件：存储/门面返回 typed outcome，删除字符串 marker 判定并补失败路径用例。
9. **规模存量**：`peri-resources/src/sessions/sqlite_store/schema_test.rs` 1038 行超出 `STD-SIZE-001`，不属本次变更。完成条件：另立拆分任务，保持断言与公开路径不变。
10. **遗留验证缺口**：S4 open 的 bounds 读取失败/页读取失败/发布前 generation/structure/epoch 失败缺少端到端用例；fork 在 environment shutdown 未确认时不撤销目标；load 回放与 close 并发仍有竞态；`bodyDescriptors` 负向与 descriptor 字段断言不完整。详见 `.peri/plans/s4-verification-round3.md` §3。完成条件：补齐对应失败路径与竞态回归。
11. **旧分页语义未改**：初始 load 后补写 `nextCursor`/`snapshotToken` 及迟到 cursor attach 的既有行为保持不变；V1 不回灌旧 SDK。

### 8.5 旧断言改写清单

以下“compact 后必须保留全部已加载正文/恢复完整 payload”的旧描述已改写：

1. `docs/design/micro-compact.md` §6.3：手动 `/compact` 从“恢复完整 payload”改为“只读有效集 + 归档正文不入读取/摘要”。
2. `docs/standards/architecture-contracts.md` `ARC-COMPACT-001` Rule：同步为有效集语义，并补充窗口默认关闭条件。
3. `docs/code-index/peri-tui.md` “改 compact 信息展示”：从“已加载窗口与翻页进度原样保留”改为“默认关闭时保留旧行为；窗口启用时 `CompactCompleted` 只提示、`HistoryCommitted` 封印并预算释放”。
4. `docs/code-index/peri-ts-sdk.md` 实时 Yjs 投影行：把“完成只更新 compaction 元数据并保留窗口与游标”限定为旧分页路径，并注明 SDK 尚未接入 `HistoryCommitted`/段释放。
5. `docs/code-index/peri-agent.md` “/compact 命令路径”行：从“从完整快照恢复 payload（含 reminder）”改为“`load_active_snapshot` 有效集 + 有界 flags 确认额外已归档 id”。
6. `spec/issues/2026-10-13-transcript-shrink-history-pagination.md` D9：标记为已取代，补记 `load_active_snapshot` 现行实现。

代码测试 `peri-tui/src/kit/acp_events_test/session_compact_events_test.rs::test_compact_turndone_keeps_window_without_reload` 仍保留“默认关闭路径不重载、旧消息仍在”的断言；窗口启用时的“未收提交回执不淘汰、收到后按预算释放”由 `history_window_commit_test.rs` 覆盖。该旧测试的窗口分支改写属测试变更，不在本次仅文档范围。
