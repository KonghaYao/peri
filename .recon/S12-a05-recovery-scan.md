# A2：恢复路径 O(线程行数) 能否在无 DDL 前提下收敛（侦察 + 度量）

- worktree：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`，分支 `feat/transcript-shrink-history-pagination`（HEAD `3ccf75864`）。
- 本任务**未改任何被跟踪文件**：只读代码、复跑既有 `#[ignore]` 用例、在 `.recon/a2-raw/` 下的一次性数据库副本上做对照实验。仓库代码与任何 Peri 会话库都没有被写入。
- 原始产物（未编辑）：`.recon/a2-raw/perf-ignored.log`、`scan-probe.out`、`ab-probe.out`、`timing-inproc.out`、`write-probe.out`、`rewind-demo.out`、`realdb-probe.out`。

## 0. 结论

1. **偏差 D10 复现**：三条恢复热路径 SQL 的计划是 `SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)`，**执行期实际步进 200 003 个索引项、逐项回表判定 `excluded`**，只为返回 3 行（scanstats `rows=` 实测，不是推断）。VDBE 里 `DeferredSeek` + `Column 0 5`（`messages.excluded`）就在 `Next` 循环内。
2. **无 DDL 的三条收窄手段全部不成立**：`(b)` 在本 schema 下**不可能**（`messages` 上唯一的用户索引只有 `(thread_id)`，无任何索引覆盖 `excluded`/`content`）；`(a)/(c)` 即使把「有效后缀起点」持久化，也**既无效又不安全**——真实会话里有效行与排除行在 rowid 序上交织（实测 45 次切换、最优下界只省 0.9%/10%/0%），且 `clear_rewind_excluded` 会把任意低 rowid 行重新置为有效，下界会**静默丢行**（实测 5 行 → 3 行，无报错）。
3. **裁决：必须 DDL。** 建议新增**部分索引** `CREATE INDEX IF NOT EXISTS idx_messages_thread_active ON messages(thread_id) WHERE excluded = 0`：同副本同语句实测触达 200 003 → **3** 行、单查 user CPU 30–35 ms → **0.01–0.06 ms**；代价是写入 +10%、`excluded` 翻转 +约 2 倍 CPU、索引体积 ∝ 有效行数（本夹具 1 页 / 4 096 B）。

## 1. 复跑性能基线（命令、真实输出、负载）

命令（既有用法，见 `session_history_perf_test.rs` 文件头；未加任何新参数）：

```bash
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history
PERI_PERF_ROWS=10000,100000,1000000 PERI_PERF_ITERS=20 PERI_PERF_PAGES=20 \
  ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib \
  -- history_perf_tests --ignored --nocapture
```

结果（`.recon/a2-raw/perf-ignored.log`，5 条 `#[ignore]` 全跑）：

```text
test ...history_perf_restore_and_paging_evidence ... ok
test ...history_perf_byte_budget_bounds_page_length ... ok
test ...history_perf_oversize_page_carries_one_body ... ok
test ...history_perf_excluded_bodies_are_never_decoded ... ok
test ...audit_segment_lookahead_and_plan_shape ... ok
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 518 filtered out; finished in 238.33s
```

三规模（同一份日志；**毫秒数只作参考**）：

| 归档行 | 恢复 cold p50 | 恢复 hot p50 | 行数探针（`COUNT(*) WHERE thread_id=?`） | 解码正文字节 | 库文件 |
| --- | --- | --- | --- | --- | --- |
| 10 000 | 11.067 ms | 10.639 ms | 10 003（excluded 10 000） | 196 971 B | 14.7 MB |
| 100 000 | 90.959 ms | 94.927 ms | 100 003（excluded 100 000） | 196 971 B | 144.7 MB |
| 1 000 000 | **3 506.125 ms** | 2 370.096 ms | 1 000 003（excluded 1 000 000） | 196 971 B | 1 445.0 MB |

同一进程内的对照（1M 规模，分页路径 20 页）：`p_ms=2.310…3.823`（`第 1 页 items=50 bytes=251209`），即**分页是「计划有界 + 触达有界」的对照组**，恢复不是。

**负载与环境（必读，否则上表会被误读）**：

1. 本机同时有其它 agent 在编译：本任务开始时 load average 1 分钟值 **10.91**，1M 计时阶段实测 **30.04**（旧基线记录的是 6~8）。
2. `cargo test` 默认多线程：上面 5 条用例是**并发**跑完的，1M 段与其它三条的夹具生成/写入同时进行。
3. 因此：1M 的 3 506 ms **不能**与 D10 记录的 1 081.927 ms 直接比较（相差 3.2×，主要归因于负载与并行）；本轮的 10k→100k 仍是 ~8.2×（11.067 → 90.959），100k→1M 是 38.5×（超线性，指向 144 MB → 1.45 GB 的页缓存换出 + load 30 + 同进程 4 条用例并发写盘）。**结论只按「touch 行数随线程行数线性」这一确定性事实下判**，毫秒数只作旁证。
4. 环境：macos aarch64，`test` profile（unoptimized + debuginfo）；Rust 侧 SQLite 3.46.0（sqlx 捆绑，由用例打印的 `sqlite_version` 确认）。

## 2. 决定性证据：计划 + 执行期触达行数（scanstats）

`EXPLAIN QUERY PLAN` 只证明计划形状；**触达行数**用 `sqlite3` 的 `.scanstats on`（`sqlite3_stmt_scanstatus`）实测——它给出每个循环**实际步进**的次数，不是估算。

夹具（`.recon/a2-raw/scan-probe.sql`，与生产 canonical DDL 同形；20 万归档行 `excluded=1` + 3 有效行 `excluded=0`，归档 1 KiB/行、有效 64 KiB/行；复刻说明：`message_id` 用可读串而非 UUID，CLI 侧不解码 payload，行数与 `excluded` 分布口径与生产夹具一致）：

```bash
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history/.recon/a2-raw
sqlite3 probe.db < scan-probe.sql     # 脚本内含 .eqp full / .scanstats on / .timer on，见文件头
```

三条生产语句原文（从实现常量抄录，逐字未改；CLI 无法绑定参数，`?1` 在计划探针里以字面量 `'probe-thread'` 代入——SQLite 不做参数嗅探，且 Rust 侧用**真实绑定**跑出的计划形状已在上表结论 3 交叉确认）：

```text
=== Q1 ACTIVE_PAYLOADS_SQL ===
SELECT message_id, content FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid;
      `--SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)     (loops=1 rows=200003)

=== Q2 ACTIVE_FLAGS_SQL ===
SELECT message_id, truncated, excluded, projection FROM messages WHERE thread_id = ?1 AND excluded = 0
  AND (truncated = 1 OR projection IS NOT NULL);
      `--SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)     (loops=1 rows=200003)

=== Q3 ACTIVE_ENTRY_POSITIONS_SQL ===
SELECT message_id, rowid FROM messages WHERE thread_id = ?1 AND excluded = 0 ORDER BY rowid ASC;
      `--SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)     (loops=1 rows=200003)
```

对照：**只加一个 rowid 下界**（同一份夹具、同一条 SQL，仅追加 `AND rowid >= 199998`）：

```text
=== V1 只用 rowid 下界（字面量） ===
      `--SEARCH messages USING INDEX idx_messages_thread_id (thread_id=? AND rowid>?)     (loops=1 rows=6)
```

> 结论 1（可复算）：三条语句当前各步进 **200 003** 个索引项才产出 3 行 ⇒ 触达行数 = 线程行数。
> 结论 2：`idx_messages_thread_id(thread_id)` 的索引项实为 `(thread_id, rowid)`，**rowid 可以被下界直接吃掉**（`rows=6`），这是后面评估 (a)/(c) 的基础。
> 结论 3：Rust 侧同一计划由本次复跑再次确认（`perf-ignored.log:167/169/177`、`:222/224/232`、`:278/280/288`，三段规模全部是 `SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)`）。

### 2.1 逐行回表：VDBE 级证据

```bash
sqlite3 probe.db "EXPLAIN SELECT message_id, rowid FROM messages \
  WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid ASC;" \
  | grep -E "OpenRead|SeekGE|IdxGT|DeferredSeek|Column|Next"
```

```text
3     OpenRead       2     6     0     k(2,,)         2   root=6 iDb=0; idx_messages_thread_id
5     SeekGE         2     14    1     1              0   key=r[1]
7       DeferredSeek   2     0     0                    0   Move 0 to 2.rowid if needed
8       Column         0     5     2     0              0   r[2]=messages.excluded
10      Column         0     0     4                    0   r[4]= cursor 0 column 0
12      ResultRow      4     2     0                    0   output=r[4..5]
13    Next           2     6     1                    0
（上面是同一段输出里与「逐行回表」相关的行，其余 opcode 略）
```

`Column 0 5`（表列 `excluded`）在 `Next` 循环体内 ⇒ **每个索引项都要读一次表行**。索引不含 `excluded`/`content`，因此「过滤在 SQL 里完成」只是不把归档正文送进 Rust，并没有减少存储层触达。

## 3. 无 DDL 手段逐条评估

### 3.1 手段 (b)：让过滤在索引内完成（覆盖索引）——**不可能，本 schema 下无从谈起**

```bash
grep -rn "CREATE INDEX" --include=*.rs peri-resources/src/sessions/ | grep -v "_test\|fixtures" \
  | grep -io "on messages([^)]*)" | sort -u
# ON messages(thread_id)     ← 唯一一条
```

- `messages` 上的索引只有：`idx_messages_thread_id(thread_id)`（`canonical.rs:146/248`）与 `TEXT PRIMARY KEY` 带来的 `sqlite_autoindex_messages_1(message_id)`（`PRAGMA index_list` 实测：`sqlite_autoindex_messages_1`、`idx_messages_thread_id`）。
- 三条语句需要 `excluded`（Q1 还要 `content`），两者都不在任何索引里 ⇒ **不存在**可用的覆盖索引；要造覆盖索引就是新增索引 = DDL。
- 附带事实：`schema_shape.rs` 文件头明确「索引是派生对象…**不参与形状比对**」，即新增索引不会改变 schema 形状契约，但**已有库**不会自动获得它（见 §5.3）。

### 3.2 手段 (a)：用存储序（rowid）把读取收窄到「最后一次 Full 之后」——**不存在可用的持久下界，且有静默丢行风险**

1. **没有任何地方持久化「最后一次 Full 的边界」。** `full_watermark` 是 `peri-agent/src/agent/stages/compact_progress.rs:28` 的 `CompactBudgetRecovery` 字段，纯内存、进程内有效，重启即丢；存储侧 `threads` 的列（`config`/`snapshot_at_message_id`/`frozen_context`/`message_count`）都没有这个语义（`UPDATE threads` 的全部写入点：`sqlite_store.rs:256/341/377/485/537`、`session_data.rs:211/311/760/810/884`、`compaction.rs:32/131`、`session_data_helpers.rs:232`）。需求也明确「不在恢复时…从摘要字符串猜测压缩边界」。
2. **就算持久化了，它在真实数据上几乎没有收窄效果。** 只读抽查真实会话库 `~/.peri/threads/threads.db`（`sqlite3 -readonly` + `PRAGMA query_only=1`；产物 `realdb-probe.out`）：

   ```text
   ## 最大会话 01a0cbc5-…（4636 行，范围 rowid 757952..798569）
   assistant|0|27     assistant|1|2056
   system_reminder|0|22
   tool|0|32          tool|1|2368
   user|0|8           user|1|123
   first_active|758122
   transitions|45              ← 按 rowid 序 excluded 的切换次数
   rows_after_bound|4596       ← 若下界取「第一条有效行」，仍要步进的行数（总 4636）
   ```

   另两个较大会话：`01a04668` 4509 行 / 有效 63 / 首有效 rowid 587130（min 573572，省 10%）；`01a0b75c` 4408 行 / 有效 231 / 首有效 rowid 683209 = min rowid（**省 0%**）。
   ⇒ 有效集合在存储序上**不是后缀**：下界最好情况只省 10%，最坏 0%。
3. **rewind 会把低 rowid 的行重新变成有效**，任何固定下界都会漏读且不报错。`clear_rewind_excluded`（`session_data/history.rs:531`）就在 rewind 的同一事务里执行 `UPDATE messages SET excluded = 0 WHERE thread_id = ?1 AND message_id IN (…)`。实测（`rewind-demo.out`，一次性副本）：

   ```text
   ### 1) 当前下界 watermark = 200001
   ### 2) clear_rewind_excluded 把两个归档行改回 excluded=0 → changes() = 2
   ### 3) 生产原句（无下界）返回行数 = 5
   ### 4) 加 rowid 下界 [rowid >= 200001] 后 = 3
   ### 5) 被下界丢掉的行：arch-000042|42、arch-000043|43
   ```

   即：**下界方案的失败模式是「少恢复有效历史」**（数据丢失类），不是变慢。
4. **rowid 复用单独不会击穿下界**（精确一点）：新行的 rowid = 表内最大 rowid + 1，只要水位行还在，新行必然 > 水位；水位行被精确删除时按 message_id 解析会得到 NULL，而 `rowid >= NULL` 的行为是**返回 0 行**（VDBE：`IsNull … goto Halt`），需要额外的 `COALESCE` 兜底。真正击穿的是第 3 条的 rewind 路径（结构性），不是复用。
5. **语义等价性风险汇总**：`rewind`（破坏下界，静默丢行）> 精确删除（水位行消失 → NULL → 需要 fail-open 回退到全范围）> rowid 复用（需与删除组合才危险，且方向是「少读」）。

### 3.3 手段 (c)：把「有效后缀起点」写进已有列——**载体存在，但收益≈0、并引入漂移风险**

1. 唯一说得通的载体是 `threads.config`（TEXT/JSON；`load_meta_on` 恢复时本来就会读到它，不增加查询）。`snapshot_at_message_id` 语义是 fork 快照锚点、`message_count` 是计数、`messages.projection` 是投影指令，借用它们是语义劫持。
2. 但它与 (a) 共用同一个数学前提——「有效行都在下界之后」——§3.2 第 2 条已用真实数据否掉（省 0%~10%）。
3. 额外的维护代价：会改 `excluded` / 行的写路径至少 4 条（`commit_compaction_lifecycle_on`、`rewind_stored_history`、`remove_stored_history_entries`、`update_message_flags`）都要在同一事务里同步这个派生值；任何一处漏写或分叉，失败模式同样是**静默丢有效历史**，而 SQLite 索引由引擎维护、不可能漂移。这与仓库原则「业务规则单一权威」相悖。
4. 结论：不成立（无收益 + 新风险）。

### 3.4 一并排除的其它无 DDL 变体（附否决理由）

1. **把归档行的 `thread_id` 改成墓碑值**（让 `WHERE thread_id=?` 天然只剩有效行）：`messages.thread_id` 有外键指向 `threads(id)`，且分页/回看、精确删除、`message_count` 全部按 `thread_id` 归属 ⇒ 直接破坏归档语义，否决。
2. **把「有效消息 id 集合」存进 `threads.config`**：违反 REQ-T02「不得保留无界 ID 集合」，且 `IN (…)` 受 `SQLITE_MAX_VARIABLE_NUMBER` 限制，否决。
3. **反向从最新行扫描、遇到第一条 excluded 就停**：等价于假设「有效集是后缀」，同样是 rewind 下静默丢行，否决。
4. **恢复只读「有效段」而归档行删表**：与「标记代替删除」的既有设计冲突（分页/回看需要归档正文），否决。

## 4. 假设 DDL 的收益与代价（一次性副本实测）

对照脚本：`.recon/a2-raw/ab-probe.sh`（同一份 20 万行副本、同一进程外条件、结果 stdout 丢弃以免 64 KiB 正文干扰），产物 `ab-probe.out`。

| 阶段 | 计划 | scanstats 触达行数 | 单查 real |
| --- | --- | --- | --- |
| 0 基线（无附加索引） | `SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)` | **200 003** | 0.05–0.07 s |
| V1 仅加 `rowid >= 199998` | `… (thread_id=? AND rowid>?)` | **6** | 0.00–0.01 s |
| 1 索引 A `(thread_id, excluded)` | `SEARCH messages USING INDEX idx_active_a (thread_id=? AND excluded=?)` | **3** | 0.00–0.01 s |
| 2 索引 B `(thread_id) WHERE excluded = 0` | `SEARCH messages USING INDEX idx_active_b (thread_id=?)` | **3** | 0.00–0.01 s |

进程内计时（`timing-inproc.out`，同进程、页缓存热，避开进程启动与打印）：

```text
### 阶段 0 基线（无附加索引）      Run Time: real 0.063~0.182  user 0.030~0.035   （每查 9 次采样）
### 阶段 1 索引 A                  Run Time: real 0.000        user 0.000013~0.000063
### 阶段 2 索引 B                  Run Time: real 0.000        user 0.000012~0.000063
```

（区间已排除各阶段首条含冷启动的样本：阶段 0 首条 `real 0.799 user 0.042`、阶段 1 首条 `user 0.000208`、阶段 2 首条 `user 0.000194`；这些在 `timing-inproc.out` 里可逐条核对。）

⇒ 索引把单查 CPU 从 ~30 ms 降到 ~0.02 ms（本夹具 20 万行；比值随归档增长而不变，因为一边 O(线程行数)、一边 O(有效行数)）。

写入/标记代价（`write-probe.out`，每档 10 万行，**user CPU**；三个样本落在同一库的不同行组，单次采样，故只作量级参考）：

| 操作 | 无附加索引 | 索引 A `(thread_id, excluded)` | 索引 B partial |
| --- | --- | --- | --- |
| 追加 10 万行（user） | 0.0714 s | 0.0804 s（+13%） | 0.0784 s（+10%） |
| 翻转 10 万行 `excluded=1`（user） | 0.0226 s | 0.0644 s（2.8×） | 0.0496 s（2.2×） |

占用（`dbstat`，**同一份 20 万行夹具**）：`messages` 117 424 128 B；`idx_messages_thread_id` 4 792 320 B；`sqlite_autoindex_messages_1` 4 571 136 B；索引 A `(thread_id, excluded)` **4 407 296 B**（1 076 页，≈ 每条行一个索引项）；索引 B partial **4 096 B（1 页，因为它只索引 3 条有效行）**。真实会话的有效行占比 1.4%~5.2%（§3.2 抽查），故 partial 索引的体积随**有效行数**增长，而不是随线程总行数。

## 5. 建议与最小实现草案（供批准）

1. **索引定义（首选，B）**：

   ```sql
   CREATE INDEX IF NOT EXISTS idx_messages_thread_active ON messages(thread_id) WHERE excluded = 0
   ```

   理由：三条热路径语句都含字面量 `excluded = 0`，部分索引谓词可被蕴含；体积 ∝ 有效行数；对分页语句（无 `excluded` 谓词）与 `load_flags_on`（`truncated = 1 OR excluded = 1 OR …`，不含 `excluded = 0`）**没有副作用**（它们继续用 `idx_messages_thread_id`）。
2. **备选（A，若团队不接受「查询文本必须保留 `excluded = 0` 字面量」这种耦合）**：`CREATE INDEX IF NOT EXISTS idx_messages_thread_active ON messages(thread_id, excluded)`。收益一致（rows=3），代价是体积与写入更大、且对 `excluded <> 1` 这类写法也仍不会被当成范围约束。
3. **已有库如何拿到索引（实现时必须选定）**：`canonical::CREATE_INDEXES` 只在**空库建库**、v11 迁移终点、远端升级收尾三处执行（`sqlite_store/schema.rs:228`、`storage_v11_migration.rs:205`、`remote/schema_upgrade.rs:323/549`）。已处于 v11 的库需要二选一：
   1. 版本推进 11→12，并在迁移里按 `IF NOT EXISTS` 建索引（走既有迁移契约）；
   2. 打开时幂等补索引（`session_close_intents` 已有「写打开幂等补齐」的先例），因为索引被显式排除在形状契约之外（`schema_shape.rs` 文件头）。
4. **两端一致**：远端下发的是同一份 `CREATE_INDEXES` 清单（`remote/session_shape_test.rs:199` 断言 `spec.sql == index_sql`）。清单里**已经有一条带 WHERE 的部分索引**（`idx_threads_updated … WHERE hidden = 0 AND message_count > 0`），所以「部分索引」本身不是新形态；但远端执行器是否忠实支持仍需在远端后端实测（见 §6）。
5. **验收闭环方式**：加索引后重跑 §1 的同一条命令，A05 的判据应从「10k/100k/1M 线性」变为「三档接近常数」，显式断言可复用用例里已有的 `assert_located_by_index`，并把计划断言收紧到「三条语句的计划行出现 `excluded=` 或明确写出 `idx_messages_thread_active`」——**收紧需权衡**：现有注释刻意不写死索引名以免假绿，这条留给实现切片决定。

## 6. 未验证项与局限

1. **未端到端验证「加索引后 A05 通过」**：那需要改 schema 代码或用例夹具，超出本任务「只侦察」的范围。本报告给的是同一 SQL 路径上的行数与 CPU 对照（20 万行），1M 档的收益是**按 O(线程行数)→O(有效行数) 的外推**，不是实测。
2. **SQLite 版本差异**：CLI 实验用系统的 3.51.0，生产/Rust 侧是 sqlx 捆绑的 3.46.0。计划形状用 Rust 侧复跑交叉确认（§2 结论 3，三段一致），但 scanstats 的「行数」只在 CLI 上测到。
3. **端口/执行器差异**：远端（云端）后端执行 `CREATE INDEX` 的行为、部分索引的支持度、以及远端查询计划是否同样受益，均未实测。
4. **毫秒数受负载影响**：1M 档 3 506 ms（load 30.04）与旧基线 1 081.927 ms（load 6~8）不可直接比较；跨阶段比较只用同进程同副本的对照（§4），并优先使用 **user CPU** 与**触达行数**这类与负载弱相关的量。
5. **真实库抽查是抽样**：只查了 `~/.peri/threads/threads.db` 里最大的 3 个会话（只读、`query_only=1`），不是全库普查；结论「有效集不是后缀」在这 3 个样本上成立，机制上由 `clear_rewind_excluded` 保证。
6. **峰值内存未采样**（D10 同样未采样）；**页级读盘字节**在 sqlx 公开面上不可观测（未测量）。
7. **索引迁移成本未测量**：在 11.8 GB 的真实会话库上建索引需要一次全表扫描与额外磁盘，本报告没有实测该时长（避免在用户的活跃库上做写操作）。
8. **`write-probe` 的写入代价是单次采样**，且生产路径是「按 id 逐条 UPDATE flags」（`commit_compaction_lifecycle_on`），与脚本里的整表 UPDATE 形态不同，故只能作量级参考。

VERDICT: DDL-REQUIRED
