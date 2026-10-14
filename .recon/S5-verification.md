# S5 验证报告：Transcript 收缩 + 有效范围读取（切片 S1–S4 交付后）

- 工作树：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
- 分支：`feat/transcript-shrink-history-pagination`，HEAD = `7f0de56e8`（需求核查基线）；全部改动**未提交**
- 变更规模：54 文件，+1895 / −191（`git diff --shortstat HEAD`；不含 `.recon/` 与蓝图）
- 本轮切片：S1 契约层、S2 存储层、S3 核心收缩、S4 消费者（`STATUS.md` 编号；对应蓝图 S1/S2/S3a/S3b）
- 不在本轮：蓝图 S4 rewind、S5 ACP 分页、S6 前端（见 §6）
- 注意：`origin/main` 已前进到 `5be10760`（3 个 upstream commit），其中
  `peri-agent/src/session/subagent/factory/{spawn,resume}.rs` 与本切片改到同一函数，合并时需人工解冲突（§7-8）

---

## 1. 命令与观测结果

1. `./scripts/cargo-rmcp-patched.sh check --locked --workspace`
   → `Finished dev profile ... in 1m 46s`，exit 0，**0 warning / 0 error**。
2. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib`
   → `running 1074 tests`，`1073 passed; 1 failed`，8.53s。失败：§2.1。
3. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib`
   → `running 547 tests`，`547 passed; 0 failed`，0.04s。
4. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib`
   → 首跑 `running 484 tests`，`461 passed; 2 failed; 21 ignored`，35.42s（失败：§2.3）；
   → 复跑 `463 passed; 0 failed; 21 ignored`，34.40s（同机并发负载消失后全绿）。
5. **附加**（覆盖 `cargo check --workspace` 不编译的测试目标）：
   `./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp -p peri-acp-types -p peri-resources -p peri-agent --all-targets`
   → `Finished ... in 1m 49s`，exit 0，0 warning。`#[cfg(test)]`、`tests/`、集成测试全部编译通过。
6. **附加**（本轮新增测试大多落在 peri-acp，不在必跑清单内）：
   `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib`
   → `running 830 tests`，`829 passed; 1 failed`，121.62s。失败：§2.2。
7. 定向复跑：`... test --locked -p peri-resources --lib -- connection::open_tests` → 12 passed，0 failed。
8. 定向复跑：`... test --locked -p peri-agent --lib -- test_sqlite_subagent_spawn_full_micro_cold_resume_preserves_provenance` → 稳定 FAILED（非 flake）。

汇总：**2 个确定性测试失败（均为测试侧未随契约更新，非实现回归）**，2 个并发负载 flake（复跑全绿）。

---

## 2. 失败项（S5 需要跟进）

### 2.1 `peri-agent`：`provenance_tests::test_sqlite_subagent_spawn_full_micro_cold_resume_preserves_provenance`

- 位置：`peri-agent/src/session/subagent/provenance_test.rs:288`（循环 `287–289`）
- 现象：`assert_eq!(transcript.flags(id), flags, "当前 child own flags 必须恢复")`
  → `left: MessageFlags{excluded:false}`（默认值），`right: MessageFlags{excluded:true}`
- 原因：S4 把 `resume.rs:155` 的 `load_session_snapshot` 换成 `load_active_snapshot`
  （`peri-agent/src/session/subagent/factory/resume.rs:151-179`）。child 自己 Full Compact 后
  被排除的条目**不再进入重载热态**，`transcript.flags(已排除 id)` 自然返回默认值。
  测试仍按「完整快照的 flags 必须逐条还原」断言，属旧契约。
- 判断：**实现符合设计**（REQ-T02 明确禁止为已移除条目保留常驻 ID/标记集合），测试断言需更新：
  只对 `active_snapshot.payloads` 覆盖的 id 比较 flags，并补两条断言——
  已排除 id 不在热态 transcript、但仍能从 `load_session_history` 读到。
- 处置：未改（按 S5 约定只报告）。

### 2.2 `peri-acp`：`legacy_tests::legacy_post_adoption_load_failure_drains_environment_without_rollback`

- 位置：`peri-acp/src/host/requests_legacy_test.rs:891`（`expect_err("history 读取失败不得发布会话")`）
- 现象：注入的历史读取失败没有生效，会话被发布（返回 Ok(ReadOnly/…)，`sessions` 非空）
- 原因：故障夹具 `FailingHistoryLoad`（`:916-937` + 宏 `:939-968`）只覆写
  `load_session_history`；发布路径已改为 `load_active_snapshot`
  （`host/requests/session_restore.rs:151`、`host/requests/legacy_session.rs:44,96`）。
  夹具未覆写 `load_active_snapshot` → 落到 trait 默认实现 → 走 `load_session_snapshot`
  → 转发真实门面 → 成功。
- 判断：**注入点过期**，不是实现回归（真实读取失败仍经同一 `map_err` 变成
  `"session history load failed: …"`，测试断言的错误文案不变）。建议最小修法：在宏里额外覆写
  ```rust
  async fn load_active_snapshot(&self, _id: &ThreadId)
      -> SessionResourceResult<ActiveSessionSnapshot> {
      Err(SessionResourceError::conflict("injected: session history load failed"))
  }
  ```
- 处置：未改（按 S5 约定只报告）。

### 2.3 `peri-resources` 首跑 2 例（flage，非缺陷）

`sessions::sqlite_store::connection::open_tests::{incompatible_schema_is_rejected_without_wal_or_database_changes, failed_migration_closes_connections_and_releases_initialization_lock}`
→ `session database initialization is busy`（`lock_schema_open(&path, Duration::ZERO)`）。
该文件未被本切片改动；与另两个 worktree 的并发 cargo 负载同时发生，单模块复跑 12/12 通过、整包复跑 463/463 通过。判为负载下的初始化锁 flake。

---

## 3. 蓝图不变量核对

1. **Full 提交成功前不释放任何原文** — 成立。
   `transcript.rs:776-790` 顺序为 `mark_pending → flush_persistence → apply_compaction →
   apply_compaction_lifecycle_memory → shrink_after_full_commit → mark_committed`；
   `apply_compaction` 返回 `Err` 即在 `:778-782` 提前返回，收缩不可达。
   取消：`shrink` 前最后一次 await 是 `apply_compaction`，被 drop 时状态仍为 `Pending`
   （`CompactionCommitState`：1=pending/uncertain，2=committed）→ 走既有「不确定即失效重载」，
   原文未释放。既有测试：`transcript_test.rs:196 test_commit_compaction_lifecycle_is_atomic_or_uncertain`、
   `executor_helpers/compact_cancel_test.rs`（手工 Compact 取消后按 canonical 重载）。
2. **收缩不删除未排除条目 / 祖先区** — 成立。
   `transcript.rs:802-860`：`skip(self.ancestor_len)` + 仅对 `flags.excluded &&
   !System` 的条目收集 `released`；祖先区整段不参与重建；三个容器按保留数重建，被释放 id
   同时从 `entries`/`id_index`/`flags` 移除。`commit_compaction_lifecycle` 本身在
   `:749-760` 拒绝任何指向祖先的 flag 更新。
   新测试：`transcript_test.rs:262 test_full_commit_releases_excluded_own_entries`（含容量断言）、
   `:360 test_full_commit_keeps_excluded_system_entry`。
3. **工具配对 / 投影封装整体保留或移除** — 成立。收缩以 `TranscriptEntry`（= 一个持久 payload
   的完整投影封装）为原子单位；Full 的 `flag_updates` 同样按条目采集
   （`compact_v2/full.rs:86-101`），不拆包。例：`compact_v2/full_test.rs`
   `full_compact_uses_historical_tool_results_without_local_file_re_read` 断言同一 read/result 对同时释放。
4. **祖先区与冻结继承不被父会话收缩改写** — 成立。子会话 `inherited_context` 是存储里的
   write-once 快照（`sqlite_store/context.rs:169-190` 注释与实现），父侧 compact/rewind 不改它；
   子会话自身恢复走 `load_active_snapshot`，不读父会话当前行。测试：
   `peri-acp/src/dispatch/session_fork_test.rs:202 fork_publishes_effective_payloads_and_keeps_released_bodies_in_storage`、
   `provenance_test.rs:279-286`（父 Full 不污染 child 冻结 snapshot）。
5. **无调用点再假设热 transcript 持有完整历史** — 基本成立，2 处残留见 §6-2。
   - `MessageTranscript::persisted_payloads()` 已不存在；唯一出口是 `active_payloads()`
     （`transcript.rs:396`，自带 `!excluded` 过滤）。`persisted_payloads` 仅作为
     `PromptResult`/`ExecOutcome` 的**字段名**保留，文档已改为「有效集合」语义
     （`session/execution.rs:196-198`、`executor_helpers.rs:82`、`executor/context.rs:300-304`）。
   - `load_session_snapshot` 生产调用点只剩 2 个 + 1 个 trait 默认实现：
     `dispatch/session_fork.rs:44`（fork 需完整 canonical 复制到目标库，属显式历史操作）、
     `session/exec/compact_pipeline.rs:160`（残留，见 §6-2）、
     `session_resources.rs:637`（非生产后端回退）。
   - 进度/重复 Full 保护改用稳定身份：`compact_progress.rs:28/38/58/126`（`full_watermark:
     Option<MessageId>` + `entries_after`），`new_work_before_full` 在 Full 改动 transcript **之前**
     采集（`agent/stages/compact.rs:195`，修复 S3 审查 F2）。测试：
     `compact_progress_test.rs:255 test_budget_recovery_watermark_survives_shrink_and_missing_watermark_is_new_work`。
   - delivery 三态（新提交 / 已提交且仍有效未安装 / 已提交且已退出有效上下文）以持久化事实为准：
     `agent/stages/receive.rs:147-193`、`async_tasks/handoff.rs:140-180`。测试：
     `receive_test.rs:292`、`receive_test.rs:385`。
   - 手工 Compact 的 Host↔存储身份校验改为有效集合比较（修复 S3 审查 F5）：
     `session/exec/compact_pipeline.rs:190-224`。测试：
     `peri-acp/src/session/command/compact_persistence_test.rs:180`（同一 store 二次 Full 成功）。

---

## 4. 需求映射（REQ-T01…T04）

| 需求 | 状态 | 证据 | 缺口 |
| --- | --- | --- | --- |
| **REQ-T01** 仅在 Full 提交成功后收缩 | 满足 | `transcript.rs:776-790` 顺序；失败/取消路径提前返回；`full.rs:84-101` 在 await 前固定本次 own ID 快照 | 无（收缩只由唯一生产调用者 `full.rs:122` 触发，已核实） |
| **REQ-T02** 正文与配套结构一起释放 | 大体满足 | 条目/索引/flags 三容器重建（`transcript.rs:802-860`）；消费者切换：进度水位、delivery 三态、跨 turn 快照 `active_payloads()`、手工 Compact 有效集合校验 | 手工 Compact 入口仍**整份读快照**（`compact_pipeline.rs:160`，先全量再过滤）；无界墓碑集合未引入（正确） |
| **REQ-T03** 恢复直接读有效范围 | 大体满足 | SQL 层过滤：SQLite `context.rs:132`（`excluded = 0`）、`compaction.rs:183/201`；Remote 静态 SQL `session_sql.rs:143/237/250`、`session_read.rs:61/88`；`content_size` 全历史聚合从两条投影移除（`row_mapping.rs:10-17`、`remote/session_sql.rs:28`）；`v2_execute.rs:150`、`session_restore.rs:51/141/151`、`legacy_session.rs:44/96`、`session_lifecycle.rs:495`、`workflow/agent/execution.rs:31/52`、`subagent/factory/{spawn,resume}.rs`、`intercept.rs:348` 全部改走 `load_active_snapshot` | 无查询计划/扫描量证据（A04/A05）；`load_active_snapshot` trait 默认实现仍是「全量再过滤」（仅非生产后端）；`load_message_flags` 默认实现返回空 map（缺席即「未排除」的 fail-open） |
| **REQ-T04** 显式历史操作仍正确 | 部分 | fork：复制完整 canonical 到目标存储、只发布有效集合（`dispatch/session_fork.rs:75-108`）；测试 `session_fork_test.rs:202` | **rewind 未做**：三条路径仍吃 Host 内存快照（`host/requests/rewind.rs:20/41/104`，S3 审查 F6），Full 之后回退目标会丢；`structure_version` 未引入 |

---

## 5. 验收项覆盖（A01…A25）

已覆盖（有自动化证据）：

1. **A16**（连续 Full、不按旧长度越界、摘要/再注入不算新工作）：`compact_progress.rs` 水印 + `compact_progress_test.rs:255`、`compact_retry_test.rs`。
2. **A17 前半**（二次 Compact 有效快照校验通过）：`compact_persistence_test.rs:180`。
3. **A18**（稳定 delivery ID 三态幂等）：`receive_test.rs:292/385`。
4. **A13 前半**（SQLite 与 Remote 同语义）：`sqlite_store/session_history_test.rs:494/563`、`remote/full_remote_test.rs:593`。
5. **A02**（提交前失败 / 提交后不确定 / 取消保留已提交事实）：`transcript_test.rs:196` + `compact_cancel_test.rs`（既有）。

部分覆盖：

1. **A01**：条目/索引/flags 容器容量释放有断言（`transcript_test.rs:262`），但「连续 100 次 Full」与跨 turn 引用的长跑未验证。
2. **A03**：Full 期间到达的新消息不属旧边界（`full.rs:84-91` 先固定 ID + `full_report_test.rs test_full_report_arriving_during_summary_stays_available`），后台结果注入路径无新测试。
3. **A04/A05**：实现层有界（SQL 条件过滤 + 去聚合），但**无查询计划、无扫描量、无冷热 p50/p95 数据**——按需求 §9 尾段，这部分必须补证据。
4. **A14**：fork 保留归档 + 父 Full 不改子冻结快照有测试；「父收缩后子分页内容不变」属分页切片。
5. **A17 后半**：「不读取已归档正文」未满足（手工 Compact 仍整份读，见 §6-2）。

未覆盖（后续切片）：

- **A06–A12、A15、A20–A25**：ACP 分页与前端（蓝图 S5/S6），本轮无实现。
- **A19**：rewind 有效视图重建（蓝图 S4/S8）。

---

## 6. 剩余缺口

1. **rewind 暂时不可用（本轮设计内）**：`state.history_payloads` 已是有效集合，Full 之前的目标在
   `rewind-candidates`/`preview`/`rewind` 中找不到（`host/requests/rewind.rs:20/41/104`,
   `apply_canonical_rewind:53`）。需在 rewind 切片改为按目标读存储并重建边界 + 推进
   `structure_version`。**这是切片顺序的中间态，但必须在分页/前端切片前收敛**（A19）。
2. **手工 Compact 仍整份读快照**：`session/exec/compact_pipeline.rs:160` 用
   `load_session_snapshot` 取回含 excluded 正文的完整历史，随后在 Rust 侧过滤比较。
   比较语义已正确（有效集合），但读取仍是「先全量再取用」，与 REQ-T02「手动入口不能继续读取归档正文」
   的字面要求与 A17「不读取已归档正文」不符。改为 `load_active_snapshot` 需同步核对
   `with_own_payloads`/`assemble_compact_messages` 与 `changed_messages` 统计，故本切片未改。
3. **ACP 初始回放仍非分页**：`dispatch/session_load.rs` 现在返回有效集合，同进程/新进程/fork
   三处语义已一致（S3 审查 F7 已修），但**没有 cursor 分页**；前端在分页与「取消 Compact 强制 reload」
   落地前会出现「Compact 后旧历史无法回看」的可见回退（S3 审查 F8）。
4. **无查询计划证据**：`messages` 表只有 `idx_messages_thread_id(thread_id)`，没有
   `(thread_id, excluded)`；`excluded = 0` 是否减少页读取需 `EXPLAIN QUERY PLAN` 与扫描量实测。
5. **`load_message_flags` 的 fail-open 默认实现**：未覆写的后端返回空 map，Receive 会把
   「读不到排除事实」当成「未排除」并补安装提醒（`receive.rs:175-184` 注释已声明该取舍）。
   生产两个后端都已覆写；S1 契约的默认实现是 fail-open 而非 fail-closed，属可接受但需登记。

---

## 7. 风险与未经证实的假设

1. **G6 cancel arm 的 flag 计数启发式与新收缩语义相抵**：`agent/stages/compact.rs:182/215/234`
   用 `post_compact_flagged > pre_compact_flagged` 判定「取消前已提交」。收缩会**同时移除
   条目与其标记**，已提交时该计数不再上升（典型 0 → 0）。当前判定为不可达：`run_compact` 在
   `commit_compaction_lifecycle` 之后到返回之间没有 await 点，取消分支只能在它挂起时胜出（此时
   内存尚未应用、`is_uncertain` 会先接管，`:230-232`）。**但这是隐含前提**：一旦今后在提交与返回
   之间插入任何 await，已提交的 Full 会被误判为「无变更」。建议后续把该分支改为读取
   `compaction_commit_state()`/`result.outcome()`（`:296-358` 已有正确写法）。
2. **A04/A05 未证实**：仅凭「返回行数少」不能证明有界，本切片没有查询计划或扫描量证据。
3. **跨进程/跨实例语义未验证**：所有验证都在单进程 + 本机 SQLite/远端 SQLite 夹具内，
   云端 Remote 只跑了「SQLite 扮演服务端」的语句级等价测试。
4. **upstream 漂移**：`origin/main` 已含 3 个 subagent 相关 commit，且改到
   `session/subagent/factory/{spawn,resume}.rs` 的同一函数体 → 合并/变基必然冲突，需人工核对
   「恢复只重建有效视图」与上游「sync handoff / late result」语义叠加后的行为。
5. **子会话继承来源收窄的假设**：`spawn.rs:179` 改用有效视图后，若父会话某条目不可作为继承来源，
   代码回退到内存副本（`:192-196`）而不是报错。前提是 `parent_messages` 只含可见消息
   （`peri-middlewares/src/subagent/mod.rs:543-547` 由 `state.messages()` 快照）——该前提由代码
   阅读得出，**没有**直接的自动化断言把它锁住。

---

## 8. 复现命令

```bash
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history
./scripts/cargo-rmcp-patched.sh check --locked --workspace
./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp -p peri-acp-types -p peri-resources -p peri-agent --all-targets
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib        # 附加：新增测试主要落在此 crate
# 定向复现 §2.1 / §2.2
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- test_sqlite_subagent_spawn_full_micro_cold_resume_preserves_provenance
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- legacy_post_adoption_load_failure_drains_environment_without_rollback
```
