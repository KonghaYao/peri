# R4 验证报告：R1-R3（契约 / 收缩 / rewind + 分页存储层）

日期：2026-10-13。范围：`peri-acp-types`、`peri-resources`、`peri-acp`、`peri-agent` 的当前 dirty tree（分支
`feat/transcript-shrink-history-pagination`，基线 `7f0de56e8`）。未改动任何代码。

## 1. 命令与观测结果

所有命令在 worktree 根执行，`cargo-rmcp-patched.sh` 为仓库包装脚本。

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp-types -p peri-resources -p peri-acp -p peri-agent --all-targets` | PASS。`Finished dev profile in 1m 18s`，exit 0，**零 warning** |
| 2 | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib` | PASS。`563 passed; 0 failed; 0 ignored`（0.03s），exit 0 |
| 3 | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib` | PASS。`477 passed; 0 failed; 21 ignored`（45.08s），exit 0 |
| 4 | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- rewind` | PASS。`63 passed; 0 failed; 773 filtered out`（0.98s），exit 0 |

三条测试命令串行执行（避免 cargo target 锁竞争），输出落在
`$TMPDIR/peri-bg-shell-01a1350a-…-stdout.log`（1073 行，含三段 `=== EXIT …=0 ===` 标记）。
未跑 `--workspace`（按任务约束）。

## 2. 不变量核对

| # | 不变量 | 结论 | 证据 |
| --- | --- | --- | --- |
| I1 | 只剩一个生产 rewind 入口且原子（删除 + 清 flags 同一次提交） | **PASS（ACP RPC 路径）／CAVEAT（slash 路径）** | 见下 |
| I2 | 边界规则：摘要存活 → 保留前缀排除；摘要被删 → 整个前缀清零 | PASS | `peri-acp-types/src/store/history.rs:201` `rewind_clear_excluded_ids`；判定只用 `excluded` 前缀结构 + System 跳过，无字符串约定 |
| I3 | 领域函数旁没有第二份内存 rewind 算法 | PASS | 规则唯一定义在 `store/history.rs:201`；调用点只有 `peri-acp/src/host/requests/rewind.rs:200`；`effective_view`（`rewind.rs:96`）只做「按计划物化视图」，不重算边界 |
| I4 | 分页读取不过滤 `excluded`，锚点缺失不回退最新页 | PASS | 见下 |
| I5 | 未新增列 / 表 / 索引 | PASS | `git diff HEAD` 中 **0 条** `ALTER TABLE`/`CREATE TABLE`/`CREATE INDEX`；`canonical.rs`、`sqlite_store/schema.rs`、`schema_cleanup.rs`、`remote/schema.rs` 全部未修改；3 个新文件都是 `.rs` |
| I6 | 三条 rewind 请求路径不再以 `state.history` 为定位权威 | PASS | 见下 |

### I1 细节

- ACP RPC 路径的持久化提交只有一处：`peri-acp/src/session/command/rewind.rs:229`
  `DurableRewindCommit::Boundary => store.rewind_history(tid, *boundary, clear_excluded)`；
  执行体只调用一次（`command/rewind.rs:222-233`），调用点在文件复原成功之后、事件之前。
- 原子性（本机）：`peri-resources/src/sessions/sqlite_store/session_data/history.rs:164`
  `rewind_stored_history` 在**同一事务**内删除边界之后的行（`:200` 前后）并清零 flags
  （`:204` `clear_rewind_excluded(&mut tx, …)`），再 `:207` `tx.commit()`。清零行数不符即整体失败（`:286`
  `rows_affected() != unique.len()` → `InvalidInput`），未知边界保持「无变更且不清零」（`:230-232`）。
- 原子性（远端）：`peri-resources/src/sessions/remote/session_history.rs:304` `write_rewind` 把删除语句、
  逐 id 的 guard 与 `CLEAR_EXCLUDED_SQL` 放进**同一个 effects 批次**，`:356` 单次
  `commit_effects`；前置 `:320` 的 `missing_session_messages` 拒绝非本会话 id。
- **CAVEAT**：`DurableRewindCommit::RemovedIds`（`command/rewind.rs:128-140`）仍是第二条 durable rewind
  模式，走 `remove_history_entries`，不做 flags 重建，由 slash `/rewind`
  （`command/rewind.rs:117`）使用。它与 RPC 语义不同（按 id 精确删除，无边界重建依据），
  且是本轮新引入的枚举分支（此前已存在同等行为）。当前 checkout 中
  `dispatch::execute_command`（slash 命令的唯一执行入口）**没有生产调用者**，因此这条路径在生产不可达；
  若后续恢复 slash 路由，需要为它补等价的 flags 语义或显式限制。

### I4 细节

- SQLite：`session_data/history.rs:21` `HISTORY_PAGE_ROWS_SQL` 只按 `thread_id` + `rowid <` keyset，
  读取条件里**没有** `excluded` 过滤；锚点用 `message_id` 子查询定位，`?2 IS NULL` 只在首屏可达。
- 锚点缺失的失败关闭：`session_data/history.rs:10` `HISTORY_PAGE_BOUNDS_SQL` 只回答「锚点行在不在」，
  刻意不写 `COALESCE(…, MAX(rowid)+1)`；判定集中在 `peri-acp-types/src/history_page.rs:194`
  `resolve_browsing_range`：带游标时**起点一致且锚点存在**才返回范围身份，否则
  `HistoryCursorError::Expired`。远端同形（`remote/session_sql.rs` 的
  `SELECT_HISTORY_PAGE_ROWS_SQL` / `SELECT_HISTORY_PAGE_BOUNDS_SQL`、
  `remote/session_read.rs` `read_history_page`）。
- 测试：`peri-resources/src/sessions/sqlite_store/session_history_page_test.rs:114`
  `excluded_archive_rows_are_returned_because_the_archive_is_what_pages_read`、
  `:224` `a_deleted_anchor_or_a_changed_range_head_expires_the_cursor`、
  `:283` 跨会话拒绝；`peri-acp-types/src/history_page_test.rs:202`
  `a_cursor_is_only_usable_while_its_anchor_and_range_head_survive`。

### I6 细节

- candidates：`peri-acp/src/host/requests/rewind.rs:38` 调 `dispatch::load_canonical_session_payloads`
  （`dispatch/session_load.rs` 的存储全量读取），不再 clone `s.history`。
- preview：`rewind.rs:55` 只从 `SessionState` 取 `cwd`，历史同样来自存储读取（与执行同源，fingerprint 才有意义）。
- execute：`rewind.rs:224` → `CanonicalRewind::load`（`rewind.rs:135`，走 `load_session_snapshot`
  取一次一致读取的完整逻辑上下文 + own flags），`plan`（`:165`）在存储历史上按 `MessageId` 定位目标并拒绝落在继承区的目标。
  提交成功后 `publish_rewind_view`（`:85`）才重建 `state.history` / `state.history_payloads`；
  `:296` 仅把重建结果写回 wire。
- 全仓 grep `state.history` / `s.history` 的剩余生产者：`host/prediction.rs:39`（标题预测的展示投影）、
  `host/prompt.rs:614`（prompt 后按有效视图更新）、`host/requests/session_restore.rs:142`（冷恢复发布有效集合）——
  均非 rewind 目标定位。

## 3. 需求映射

### REQ-T04（显式历史操作仍然正确）

| 需求点 | 状态 | 证据 |
| --- | --- | --- |
| 候选/预览/执行不依赖 Host 完整历史 | 已覆盖 | I6 |
| 原子建立有效 payload 与 flags | 已覆盖（RPC 路径） | I1；`session_resources.rs:868` 的 `rewind_history` 增加 `clear_excluded` 参数，门面 `resources.rs:667` 透传 |
| 不能只删后置摘要 | 已覆盖 | `history_test.rs::rewind_inside_the_excluded_block_restores_every_retained_body`；`sqlite_store/session_history_test.rs:778` / `:810`；`remote/full_remote_test.rs:964` |
| 不能把保留前缀 `excluded` 全清 | 已覆盖 | `history_test.rs::rewind_after_the_block_keeps_the_prefix_excluded`；`session_history_test.rs:833`；`full_remote_test.rs:991` |
| 热态与冷恢复同一有效视图 | 部分覆盖 | `requests_rewind_cases_test.rs:220` 断言 Host 发布视图 == 冷恢复视图；**缺**「rewind 后继续 prompt，模型请求历史 == 冷恢复有效视图」的 Agent 侧回归（设计 §2.3 要求） |
| 破坏历史范围的操作推进结构版本使旧 cursor 失效 | **未实现** | 无结构版本计数器；失效改由 `RangeToken`（自有历史最旧一条 id）+ 锚点存在性派生（`history_page.rs:36-40`、`:194`）。rewind 保留了范围起点与锚点时旧游标**仍有效**，与设计 §2.4「rewind 成功即 +1」不一致 |
| 最小元数据 / 不引入 Checkpoint | 已覆盖 | 结构判定，无新列（I5） |

### REQ-H01~H03（分页契约、游标、有界页）

| 需求点 | 状态 | 证据 / 缺口 |
| --- | --- | --- |
| H01-2 keyset 直接定位，无大 OFFSET / 无全表物化 | 已覆盖 | 两端 `… AND rowid < (SELECT rowid … message_id = ?2) ORDER BY rowid DESC LIMIT ?3` |
| H01-3 物理排序键封装在适配器内 | 已覆盖 | 游标身份是 `MessageId`；`rowid` 只出现在存储 SQL 与存在性判据里 |
| H01-7 过滤非展示条目时位置仍推进，不假造终点 | 已覆盖 | `history_page.rs:244` `assemble_history_page` 用**未过滤**行数判终点；`history_page_test.rs:107`、`session_history_page_test.rs:148` |
| H02 游标绑定会话 / 下一读取位置 / 可区分错误 | 已覆盖 | `HistoryCursor` + `HistoryCursorError::{Malformed, CrossSession, Expired}`（`history_page.rs:48`），映射到 `SessionResourceErrorKind::Cursor`（`session_resources.rs:345` 附近） |
| H02 追加消息、再次 Full 不使旧页失效 | 已覆盖（存储语义） | 分页读取不消费 `flags`、不改行集合；`full_remote_test.rs` / `session_history_test.rs` 的 rewind 用例证明清 flags 不影响行序 |
| H02 结构版本 / `snapshotToken`（含投影与字节预算版本） | **未实现** | 现以 `RangeToken` 代替（见 REQ-T04 表）；契约里没有 `snapshotToken` 字段 |
| H03 每页条数、512 KiB 预算、`oversize` 独占一页且游标前进 | 契约与存储层已覆盖 | `limit` / `byte_budget` 为参数；`history_page.rs:244` 的规则 3/4；`history_page_test.rs:140/164`、`session_history_page_test.rs:180` |
| H03 预算同时约束读取与反序列化（不先物化再裁） | 已覆盖（本页 + 1 条候选） | 最多读 `limit + 1` 行；超预算的行**解码前**停下（`history_page.rs:260-264`） |
| H03 默认参数（50 / 100 / 512 KiB）落地 | **未实现** | 契约只声明参数，没有默认值来源（ACP 出口未接） |
| H01-1 首屏 cursor 从有效视图与压缩覆盖边界派生 | **未实现** | 属 ACP 出口职责 |
| H01-4/6、H02「已返回集合及补集」、H03 工具跨页关联 | **未实现** | 前端（S7）与 ACP 出口（S6）职责 |

### 验收条件 A01-A25 覆盖情况

已由本轮实现 + 测试覆盖（**在存储/核心层语义范围内**，非端到端验收）：

- **A01**（收缩后规模受控、模型请求等价）：`transcript.rs:802` `shrink_after_full_commit` +
  `transcript_test.rs` 新增用例；跨 turn 快照改走 `active_payloads`（`transcript.rs:396`）。
- **A04**（恢复不读已排除正文、无全历史聚合）：`load_active_snapshot` 两端都在读取条件里带
  `excluded = 0`（`sqlite_store/context.rs:137`、`remote/session_sql.rs:141`），`content_size` 的
  `SUM(LENGTH(content))` 相关子查询移出两后端投影（`row_mapping.rs:12-18`、`session_sql.rs:30-37`）。
  **注意**：`content_size` 现在恒为 0，全仓无生产消费者（只有定义与一个 TUI 测试夹具），属行为降级而非破坏。
- **A09 / A13**（同 cursor 等价、两后端一致）：`assemble_history_page` 是两端唯一页装配实现。
- **A14**（父会话 Full 不改子快照）：`ForkedHistory::active_payloads`（`store/history.rs:43`）+
  fork/session_restore 相关改动。
- **A16**（重复 Full 保护用稳定身份）：`compact_progress.rs` 的 `full_watermark: Option<MessageId>` +
  `has_new_work`（水印缺席 → 视为有新工作）。
- **A17**（手动 Compact 有效快照校验）：`compact_pipeline.rs` 的 `history_matches` 改为按快照 flags
  对称剔除 `excluded` 后比较 id 序列。
- **A18**（delivery 三态幂等）：`async_tasks/handoff.rs:144-160` 的 `Ok(true)` / `Ok(false)` + flags 查询
  区分「新提交 / 已提交未安装 / 已提交且已排除」；用例
  `receive_test.rs::committed_delivery_missing_from_hot_state_is_restored_without_reemitting`、
  `::excluded_committed_delivery_is_neither_restored_nor_reemitted`。
- **A19**（两次 Full 后三种 rewind）：`session_history_test.rs:778/810/833`、`full_remote_test.rs:964/991/1011`；
  旧 cursor 失效一项未覆盖。

仍**未覆盖或仅部分覆盖**：

- A02（提交前失败 / 提交后确认丢失 / 后续 turn 取消）：`transcript_test.rs:196`
  `test_commit_compaction_lifecycle_is_atomic_or_uncertain` 覆盖不确定态；「提交后确认丢失」的端到端场景未见。
- A03（Full 期间到达新输入/后台结果）：`receive.rs` 有改动，但未见到针对「旧边界不移除新消息」的专项目用例。
- A05：性能测量（冷/热 p50/p95、查询次数、扫描量）无任何记录。
- A06、A07、A08、A10、A11、A12、A15、A20、A22、A23、A24：需要 ACP 分流 + TUI/SDK（S6/S7）。
- A21、A25：需要 `dispatch_request` 前置分流 + 连接级 caps（S6）。

## 4. 剩余缺口与建议归属

1. **ACP `session/load` 分页出口未实现**（S6）：无 `cursor`/`limit` 参数解析、无
   `PeriCaps::history_pagination`、无「短锁 prepare + 锁外 handle」分流。契约与两后端实现已就绪
   （`load_history_page`），仅缺调用方。→ A21/A25 与 REQ-H01-1/4/6。
2. **结构版本缺失**（S6 决策点）：设计 §2.4 要求 rewind 成功即 +1 并让旧 cursor `CursorExpired`；
   当前用 `RangeToken` 派生失效，rewind 保留起点/锚点时旧游标不失效。需裁决是补最小元数据
   （不新增列的前提下用 `threads.updated_at` 或 `SessionState` 计数器）还是修订设计。
3. **slash `/rewind` 仍走 `RemovedIds`**（I1 caveat）：语义与 RPC 不同且不清 flags。当前无生产调用者，
   建议 S6 一并处理（限制入口或补等价语义）。
4. **手动态 Compact 仍全量读快照**：`compact_pipeline.rs` 在有效集合校验通过后仍用
   `load_session_snapshot` 的完整 payload 重建 transcript（含已排除正文）——A17「不读取已归档正文」未完全达到，
   属 REQ-T03 的残余（建议 S3b 补充：重建只取有效集合 + 显式历史操作按需取归档）。
5. **热/冷一致性回归缺失**：需补「rewind 后同会话继续 prompt，模型请求历史 == 冷恢复有效视图」的 Agent 侧集成测试（S4）。
6. **`content_size` 恒 0**：无生产消费者，但 `ThreadMeta.content_size` 的语义已名存实亡；若前端未来消费需另设按需读取（已在两处注释中记录）。
7. **未验证项**：本轮只跑 `--lib` 与 `rewind` 过滤测试；未跑 E2E、TUI/SDK 侧、`--all-targets` 测试、性能基准。
