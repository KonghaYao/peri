# S9 独立验证：标准客户端完整回放是否「有界读取、不物化全历史」

日期：2026-10-14（审计时段）。worktree：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
（分支 `feat/transcript-shrink-history-pagination`，S9a/S9b 均未提交）。
审计者为独立 verification 角色：**默认只读**，未做任何 DDL、未加依赖、未做 git 写操作、未改生产语义。
唯一改动是**新增**一段审计证据代码（§2 第 14 项，可单独 revert）与两个日志文件。

## 0. 结论（先说判据，再说结论）

| 判据 | 结论 | 依据类型 |
| --- | --- | --- |
| 新增的向前段读取在**计划形状**上按索引定位、无 `messages` 全表扫描 | **成立** | 实测（EQP） |
| 新增的向前段读取**触达行数**有界（与线程总行数无关） | **成立**：每段索引项与表行都 ≈ `limit + 1` | 实测（EQP + 字节码） |
| 回放进程**内存与解码字节**有界（不物化全历史） | **成立**（继承区与单条超预算行例外，见风险 R5/R6） | 实测（段长/字节实测 + 探针） |
| 阶段二只取本段正文，多读的那一条候选行**正文不进内存** | **成立**（探针实测：候选行正文不可解码，本段仍读出） | 实测 |
| 「阶段一不取正文」这句 doc 的字面表述 | **过强**：`length(CAST(content AS BLOB))` 关掉了 SQLite 的 length-arg 优化，引擎仍会把候选行正文取回寄存器（不越过 SQL 边界、不进解码器） | 实测（字节码标志位）+ 静态判断（页级字节不可观测） |
| 回放期间**追加**新行不越过固定上界 `tail` | **成立**（追加路径 rowid > tail） | 实测（本机 + 远端用例） |
| 回放期间 **rewind 删掉 tail 行**再加新行，仍不越过上界 | **被反驳**（实测：SQLite 复用被删 rowid，新行落回 `(after, tail]` 并进入本次回放） | 实测（§3.4） |
| `after` 锚点消失失败关闭 | **成立** | 实测（本机 + 远端 + ACP 三处用例） |
| 「标准客户端完整回放不物化全历史」整体 | **有条件成立**：canonical 历史（含归档原文）不再整份物化；**有效集合仍整份物化**（`prepare_existing`），且该读取触达行数 = O(线程行数) | 实测（ACP `full_reads()==0`）+ 静态判断（代码路径）+ 引用既有证据 |

一句话：**「完整回放是有界读取」这句在内存与解码字节、以及新增向前读取的行触达上站得住；但它不等于「初始恢复成本有界」，也不覆盖 rewind 并存这一条边界（实测被打破）。**

## 1. 范围

1. 审计对象：S9a 存储契约（`load_history_segment` + 首段继承区 + `tail` 上界）与 S9b ACP 接线（未协商连接的流式 canonical 回放）。
2. 断言来源：`peri-acp-types/src/history_page.rs` 与 `session_resources.rs` 的 doc comment（当契约读）。
3. 「标准客户端」= 未协商 `peri.historyPagination` 的连接：走 `dispatch::replay_canonical_history_streamed`。
4. 不在本次范围：TUI / SDK 侧（S7）、`peri-acp` 的分页 wire 形状细节、真实云端部署。

## 2. 改动文件清单（本次审计读到/触碰的文件）

切片自身（`git diff --stat` = **102 files changed, 7779 insertions(+), 651 deletions(-)**，另有 **13 个未跟踪新文件**）只列与本次断言直接相关的：

| 绝对路径 | 一句话 |
| --- | --- |
| `/Users/.../peri-transcript-history/peri-acp-types/src/session_resources.rs` | `load_history_segment` / `load_history_page` / `history_page_bounds` / `load_entry_positions` 的契约与 doc（含语义承诺） |
| `/Users/.../peri-acp-types/src/history_page.rs` | 游标/页/段类型与**两个后端共用的唯一一份**页规则、段规则（`HistoryPagePlan` / `HistorySegmentPlan`） |
| `/Users/.../peri-resources/src/sessions/sqlite_store/session_data/history.rs` | 本机后端：4 条向前读 SQL 常量 + 两阶段读取实现 |
| `/Users/.../peri-resources/src/sessions/remote/session_read.rs` | 远端后端：同一份段规则，执行器换成 `read_batch` |
| `/Users/.../peri-resources/src/sessions/remote/session_sql.rs` | 远端 SQL 原文（与本地逐条对照见 §3.5） |
| `/Users/.../peri-acp/src/dispatch/session_replay.rs` | 未协商连接的流式回放循环（`REPLAY_SEGMENT_ROWS=50` / `REPLAY_SEGMENT_BYTE_BUDGET=512 KiB`）与 `Unsupported` 回退 |
| `/Users/.../peri-acp/src/host/requests/session_restore.rs` | `handle_load` 的两条分支；有效集合仍在 `prepare_existing` 里整份进内存 |
| `/Users/.../peri-resources/src/sessions/sqlite_store/session_history_perf_test.rs` | 计划证据用例（切片自带）+ **我新增的审计用例**（第 1116–1334 行，见下） |
| `/Users/.../peri-resources/src/sessions/sqlite_store/session_history_segment_test.rs` | 本机段语义端到端用例（空线程/仅继承区/归档+System/上界/锚点/超预算/字节定长） |
| `/Users/.../peri-resources/src/sessions/sqlite_store/session_history_page_test.rs`、`remote/session_history_page_test.rs` | 分页与段的两后端对照用例 |
| `/Users/.../peri-acp/src/host/requests_unnegotiated_replay_test.rs` | S9b 的 host 层用例（含「不读全量」断言） |

**审计者新增（唯一的代码改动）**：`session_history_perf_test.rs:1116-1334`
—— `explain_bytecode()`（新 helper）+ `audit_segment_lookahead_and_plan_shape()`（`#[ignore]`，默认不跑）。
产物日志：`.recon/S9-boundedness-raw.log`、`.recon/S9-boundedness.md`（本文件）。

## 3. 实测证据（命令 + 关键输出）

### 3.1 三 crate 的 check 与相关 lib 单测（串行，一次一个 cargo）

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp-types` | `Finished` 无错 |
| 2 | `./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp` | `Finished` 无错 |
| 3 | `./scripts/cargo-rmcp-patched.sh check --locked -p peri-resources` | `Finished` 无错 |
| 4 | `... test --locked -p peri-acp-types --lib` | **576 passed; 0 failed** |
| 5 | `... test --locked -p peri-resources --lib -- history_segment` | **12 passed; 0 failed**（本机 8 + 远端 3 + 计划 1） |
| 6 | `... test --locked -p peri-resources --lib -- history_page` | **15 passed; 0 failed**（本机 11 + 远端 4） |
| 7 | `... test --locked -p peri-acp --lib -- replay` | 首次 **29 passed; 1 failed** → 复跑 **30 passed; 0 failed**（flake，见 R8） |
| 8 | `... test --locked -p peri-acp --lib -- unnegotiated_replay --test-threads=1` | **14 passed; 0 failed** |
| 9 | `... test --locked -p peri-acp --lib -- history_page` | **18 passed; 0 failed** |

### 3.2 计划证据：新增的向前读取 4 条语句（EQP 原文）

命令（切片自带用例，默认运行、无夹具）：

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib \
    -- history_segment_statements_are_index_located --nocapture
```

语句与计划原文（逐字）：

```text
· read_history_segment/边界（上界） — SELECT
        (SELECT MAX(m.rowid) FROM messages m WHERE m.thread_id = t.id)
    FROM threads t WHERE t.id = ?1
      #2  SEARCH t USING COVERING INDEX sqlite_autoindex_threads_1 (id=?)
      #9  CORRELATED SCALAR SUBQUERY 1
      #14 SEARCH m USING COVERING INDEX idx_messages_thread_id (thread_id=?)

· read_history_segment/首段（阶段一） — SELECT message_id, rowid, length(CAST(content AS BLOB))
    FROM messages
    WHERE thread_id = ?1 AND rowid <= (
        SELECT MAX(m.rowid) FROM messages m WHERE m.thread_id = ?1)
    ORDER BY rowid ASC LIMIT ?2
      #7  SEARCH messages USING INDEX idx_messages_thread_id (thread_id=? AND rowid<?)
      #14 SCALAR SUBQUERY 1
      #19 SEARCH m USING COVERING INDEX idx_messages_thread_id (thread_id=?)

· read_history_segment/锚点存在性 — SELECT
        (SELECT m.rowid FROM messages m WHERE m.thread_id = t.id AND m.rowid = ?2)
    FROM threads t WHERE t.id = ?1
      #2  SEARCH t USING COVERING INDEX sqlite_autoindex_threads_1 (id=?)
      #9  CORRELATED SCALAR SUBQUERY 1
      #13 SEARCH m USING INTEGER PRIMARY KEY (rowid=?)

· read_history_segment/非首段（阶段一） — SELECT message_id, rowid, length(CAST(content AS BLOB))
    FROM messages
    WHERE thread_id = ?1 AND rowid > ?2 AND rowid <= ?3
    ORDER BY rowid ASC LIMIT ?4
      #7  SEARCH messages USING INDEX idx_messages_thread_id (thread_id=? AND rowid>? AND rowid<?)
```

判读（实测）：

1. **无 `SCAN messages`**，无 `USE TEMP B-TREE FOR ORDER BY`（顺序由 `idx_messages_thread_id` 的隐式
   `(thread_id, rowid)` 键序直接给出，`LIMIT` 因而能提前终止）。
2. 非首段把 `after` 与 `tail` 都变成**索引约束**（`rowid>? AND rowid<?`）：区间定位，不走尾部。
3. 首段的 `MAX(rowid)` 走**覆盖索引**（`COVERING INDEX`），不是全范围聚合；该子查询是 `SCALAR SUBQUERY`
   （只算一次），不是相关子查询。
4. 锚点存在性走 `INTEGER PRIMARY KEY (rowid=?)`（单行），不是扫描。
5. `read_page_bodies/阶段二` 走 `SEARCH messages USING INDEX sqlite_autoindex_messages_1 (message_id=?)`
   —— N 次主键单行读取，N = 本段推进行数 ≤ limit。

### 3.3 触达行数量级：字节码级判读（区分「索引项」与「表行」）

命令：

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib \
    -- audit_segment_lookahead_and_plan_shape --ignored --nocapture
```

**（a）向前段（`HISTORY_SEGMENT_NEXT_SQL`）——两级都定界：**

```text
12 SeekGT             2   26    2  0 2      ; 从 (thread_id, after) 之后定位
16 IdxGT              2   26    2  0 2      ; 超过 (thread_id, tail) 即退出
17 DeferredSeek       2    0    0  0        ; 表行取回延后到真正读列时
25 Next               2   16    1  0        ; 逐条前进，LIMIT 计数提前终止
```

索引项访问与表行取回都 ≈ `limit + 1` 条，**与线程总行数无关**。

**（b）分页阶段一（`HISTORY_PAGE_ROWS_SQL`，向更早）——只有表行定界：**

```text
12 DeferredSeek       3    0    0  0
14 IdxRowid           3    4    0  0      ; 当前索引项的 rowid
32 Ge                 7   40    4 83      ; rowid < 锚点判定（不成立就跳 Prev，不进表）
33 Column             0    0   12  0      ; 表行取回：只在这一行通过判定之后发生
35 Column             0    3    4  0
40 Prev               3   11    1  0      ; 反向逐条走索引
```

判读（实测 + 静态）：谓词 `?2 IS NULL OR rowid < (标量子查询)` 里的 `OR` 让 SQLite **无法**把锚点
变成索引约束（计划里外侧只有 `(thread_id=?)`），因此反向扫描从该线程的**最大 rowid** 起步、逐条比较；
被跳过的行**不取表行**（判定用的是索引项里的 rowid，`Column` 在其后），但**索引项访问量 ≈ 尾部到锚点的
距离**（第 k 页 ≈ `k × limit` 条）。这是对 `.recon/perf-evidence.md` §8「分页是 keyset 定位，不在此列」
的**收紧**：表行与字节有界成立，索引项触达随浏览深度线性增长（单条索引项很小，且大多在缓存页内）。

**（c）`length(CAST(content AS BLOB))` 关掉了 length-arg 优化（新发现）：**

```text
生产语句：  Column             0    3    4  0     ; p5 = 0
对照语句：  Column             0    3    3 64     ; p5 = 64 = OPFLAG_LENGTHARG（SELECT length(content)）
```

对照语句是我为取证据自造的（**不是生产语句**）。结论：不用 `CAST` 时 SQLite 只从记录头读长度；
生产语句因为 `CAST` 挡在 `length()` 与列之间，标志位为 0 —— **长度是在「取回的值」上算的**。
静态判断（未实测页级字节）：这意味着阶段一每条候选行（含那条**永不返回**的 lookahead 行）的正文
仍会被引擎读入寄存器。它不越过 SQL 边界、不进计划对象、不进解码器，也不改变「返回集大小有界」，
但 `session_resources.rs:714-717` 的「只读 id + 存储序键 + 正文长度…**不取正文**」字面上过强，
且给大行带来一次额外 I/O（见风险 R2）。

**（d）恢复路径（引用既有证据，本次未复跑）**：`.recon/perf-evidence.md` §8 已实测
`load_active_payloads_on` / `load_active_flags_on` / `read_active_entry_positions` 三条语句的计划都是
`SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)`（无 rowid 约束、无 LIMIT），
**触达行数 = 该线程总行数**，恢复 cold p50 随归档行数 10×→10×（11 / 98 / 1082 ms，debug 构建）。
我用 `PERI_PERF_ROWS=200 PERI_PERF_ITERS=3 PERI_PERF_PAGES=3` 复取了这三条语句的**计划原文**，
形状与之一致（其中 `read_active_entry_positions` 只在**协商了分页**的初始 load 上被调用：
`session_restore.rs:544` 的 `if history_pagination`；未协商的标准客户端不读它）。

### 3.4 有界性实测（段长、字节、上界、锚点、反例）

同一条审计用例（`audit_segment_lookahead_and_plan_shape`）的三段输出：

```text
== 探针：6 条 8 KiB 归档行 + 3 条 64 KiB 有效行 + 1 条不可解码行（rowid 最大），limit=3 ==
    第 1 段: payloads=3 next_cursor=true
    第 2 段: payloads=3 next_cursor=true
    第 3 段: payloads=3 next_cursor=true      <- 它的候选行（第 4 条 = 不可解码行）正文没进内存
    第 4 段失败: session data is corrupt: history entry is not decodable   <- 探针确实带电
== 段长/字节预算实测：limit=50 预算=98304 字节 ==
    段: payloads=6 bytes=49902 超预算=false next_cursor=true
    段: payloads=1 bytes=65657 超预算=false next_cursor=true
    段: payloads=1 bytes=65657 超预算=false next_cursor=true
    段: payloads=1 bytes=65657 超预算=false next_cursor=false
    合计推进 9 条自有行 / 246873 字节（夹具：6 归档 + 3 有效）
    反例（删 tail 行 + 追加）：续段 payloads=1 含新行=true 含被删行=false next_cursor=false
```

逐条判读：

1. **段长**：`limit=50` 时首段 6 行、其余 1 行；任何一段都不超过 `limit`（断言在用例里）。
2. **字节预算**：`98 304 B` 预算下首段 49 902 B 停在第 7 条（65 657 B）之前；后续三段各 65 657 B ≤ 预算。
   合计 246 873 B 恰好等于夹具写入的 `archived_bytes + live_bytes`（断言），即段内字节 = 写入字节。
3. **阶段二只取本段正文**：探针夹具把「第 4 条」的正文写成不可解码；第 3 段照样成功（若阶段二取了它，
   会以 `Corrupt` 失败），第 4 段推进它时才失败——**证明多读的那一条候选行正文没有出库、没有解码**。
   （同一手法在分页方向已有：`session_history_page_test.rs:189`。）
4. **`tail` 上界（追加）**：`session_history_segment_test.rs:240` 与 `remote/...:329` 都实测了「回放中途
   append 两行，续段仍到旧 `tail` 收尾」，两条用例本次均通过。
5. **锚点消失失败关闭**：`...:273`（本机）、`remote/...:416`、`peri-acp/.../requests_unnegotiated_replay_test.rs:728`
   三处均通过（错误分类为 `Cursor(Expired)` / load 失败，不回退成首段、不用空段冒充结束）。
6. **反例（实测，被反驳的一条）**：删掉 `tail` 那一行（模拟 rewind 的尾部切除）再追加一条新消息，
   SQLite 把被删的 rowid 复用给新行，新行因此落在 `(after, tail]` 内，**被本次回放交付**，
   而被删行的内容静默消失、无任何错误。契约只要求「追加的行 `rowid > tail` 时不进入」，
   并明确「不要求 `tail` 那一行仍存在」——这两条合起来就是这个缺口（见风险 R1）。
7. **空线程 / 只有继承区 / 单条超大正文 / excluded + System 行**：四处反例均有既有用例并本次通过
   （`history_segment_empty_thread_ends_without_a_cursor`、`history_segment_only_inherited_context_has_no_own_rows`、
   `history_segment_gives_an_oversized_entry_its_own_segment`、`history_segment_returns_archived_and_system_rows_in_canonical_order`）。

### 3.5 两后端语义一致性（对照 SQL 与用例，静态 + 实测）

1. **SQL 原文**：远端 `session_sql.rs` 的 4 条段语句、页语句、边界语句与本机 `history.rs` 的常量
   **逐字同形**（只差表别名 `m.` ↔ `s.`），锚点/上界/长度口径一致。
2. **唯一措辞差异（不构成语义差）**：阶段二正文语句本机是
   `SELECT content FROM messages WHERE thread_id=?1 AND message_id=?2`，远端多一个 `LIMIT 1`
   （`session_sql.rs:180`）。`message_id` 是主键，两者结果集恒等；但远端该语句的 doc 同时写着
   「多行即回复形状不符（调用方按损坏失败）」，与它自己的 `LIMIT 1` 互相矛盾——**文档问题**，非行为问题。
3. **用例**：两端用例是**镜像期望**（同一份手写期望分别落在两个文件），不是一次差分执行；
   `remote_history_segment_replay_matches_the_local_backend` 的「matches」指语义同形，不是同夹具对拍。
4. **A3×A4 的反例覆盖在远端缺一段**：远端只有「追加不越过上界」，没有「删 tail 行 + 追加」的用例；
   与 §3.4 第 6 条同源（风险 R1 在两后端都存在，因为规则只有一份）。
5. 远端测试跑在**进程内 `SqliteTransport`** 上，真实云端（`cloud_history_test.rs`）是 `#[ignore]`：远端
   路径的 wire 层与真实部署**未验证**。

### 3.6 ACP 接线（S9b）证据

1. `session_restore.rs:566-590`：未协商连接走 `replay_canonical_history_streamed`（逐段循环、只留当前段），
   协商连接走内存有效集合回放——**两条分支互不混用**。
2. `session_replay.rs:306-371`：循环体不累积历史；`Segment.next_cursor == None` 才是结束；
   `Unsupported` 只在**尚未交付任何内容**时回退到 `load_session_history`（全量），其余错误让整次 load 失败。
3. 用例实测（本次通过）：`streamed_canonical_replay_reads_bounded_segments_and_never_the_full_history`
   —— 60 条历史的 load 触发 ≥2 次段读取、`full_reads() == 0`、每次 `limit`/`byte_budget` 等于命名常量、
   首段无游标后续段带游标、逐条交付不重不漏。
4. 缺口：该用例只断言「没有任何一次返回全量」（`call.payloads < total`），**没有断言每次 ≤ limit**；
   逐段行数上界由存储层用例与我的审计用例覆盖（§3.4 第 1 条）。

## 4. 未验证与不确定项（不得当成已证）

1. **页级读盘字节 / 峰值 RSS**：sqlx 公开面不可观测，本次未采样。§3.3(c) 的「引擎把候选行正文读进
   寄存器」是**字节码级静态判断**，没有页级读数或 RSS 支撑（与 `perf-evidence.md` §7.1/§7.2 同一限制）。
2. **真实远端后端**：只对照了 SQL 文本与进程内 `SqliteTransport` 用例；云端部署、网络分片、真实
   数据库版本的计划差异均未验证。
3. **requirement §9 原文**：仓库内找不到该需求文件（`grep -rl "A04" spec/ docs/` 无命中），本报告
   只能引用 `.recon/perf-evidence.md` 与 `spec/issues/2026-10-13-*.md` 对 §9 的转述。
4. **1M 规模数据未复跑**：恢复随归档行数线性的数字引自 `perf-evidence.md` §8，本次只复取了计划形状。
5. **flake 根因**（§3.1 第 7 行）：只在「通过 / 串行通过、默认并发失败」这一层定位，未证明是 tracing
   interest cache 竞争；未做对照实验（需要改测试或加日志）。
6. **持续浏览后的累计缓存**：本次**零测量**（见 §5 第 2 类）。
7. **`USE TEMP B-TREE FOR ORDER BY` 是否在所有 SQLite 版本都缺席**：只在 sqlx 捆绑的 3.46.0 上取过计划，
   且既有断言函数并不检查这一项。
8. **跨后端差分执行**：没有一次「同一份夹具、两个 adapter 各自跑、逐字段对比」的实验。

## 5. 与需求 §9 的对齐（两类成本，分开写）

### 5.1 第一类：初始恢复成本降低

- **证据（实测）**：① 未协商连接的 load **不再调用全量 canonical 读取**（ACP 用例 `full_reads()==0`）；
  ② 新增向前读取 4 条语句全部按索引定位、无全表扫描、无临时排序，行触达 ≈ `limit+1`（§3.2/§3.3）；
  ③ 每段返回的字节 = 写入字节、段长受 `limit`/字节预算双重约束（§3.4）。
- **缺口（实测 + 静态）**：恢复链**本身**仍然整份读取有效集合（`session_restore.rs:138-151` →
  `load_active_session_payloads`），该读取的计划是 `thread_id=?` 的范围搜索且**无 LIMIT**，
  触达行数 = O(线程行数)（`perf-evidence.md` §8 的 D10，本次以计划原文复核、未复跑 1M 基准）。
  因此「初始恢复成本降低」只对**回放通道**成立；对「归档很多的大线程」，load 的总成本仍由有效集合读取主导。
- **不得写**：任何固定毫秒承诺。既有数字来自 debug 构建 + 4 KiB 块 0.5 MB/s 的慢盘（`perf-evidence.md`
  §1.8/§2），且本次只复取了计划形状、没有复跑基准。

### 5.2 第二类：持续浏览后的累计缓存

- **证据**：**无**。标准客户端的完整回放是一次性流式交付（内存只留当前段），本身不产生「累计缓存」；
  累计缓存属于协商分页的客户端（首屏 + 逐页 + 前端合并），落在 S7 前端切片。
- **缺口**：本切片没有测「翻到很深处后客户端驻留多少」，也没有测服务端侧的跨请求驻留（服务端每次
  纯读、无页缓存）。`perf-evidence.md` §7.6/§8 末尾已同样声明。
- **不得写**：任何「浏览 N 页后内存 = X」的结论（没有对应测量）。

## 6. 风险与已知偏差（按影响排序）

1. **R1 回放中途 rewind（尾部切除）+ 追加 → 交付错位（实测）**：删除 `tail` 行会让 SQLite 复用其 rowid，
   新行落进 `(after, tail]` 被本次回放交付，被删内容静默消失，全程无错误。触发路径：同会话两个连接
   并发（连接 A 在 load 流式回放、连接 B 发 rewind/精确删除）。回放游标没有结构版本（分页游标有
   `structure_version`）。建议：回放侧复用结构版本，或在首段/续段校验行集合仍同源。
2. **R2 阶段一仍读候选行正文（实测标志位 + 静态）**：`length(CAST(content AS BLOB))` 使 SQLite 失去
   length-arg 优化；每段（页）会多读最多 `limit+1` 条正文，含永不被返回的 lookahead 行。影响是 I/O 与
   瞬时内存（不改变返回集与解码字节）。已记录的契约措辞「不取正文」应按此收紧。
3. **R3 分页反向读取的索引项触达随深度增长（实测字节码）**：表行有界、索引项无界（第 k 页 ≈ k×limit 条）。
   与 `perf-evidence.md` §8「分页不受影响」的表述不一致：那是「表行不受影响」，不是「触达不受影响」。
4. **R4 恢复路径 O(线程行数) 未变（引用既有实测）**：`ACTIVE_PAYLOADS_SQL` / `ACTIVE_FLAGS_SQL` /
   `ACTIVE_ENTRY_POSITIONS_SQL`（后者只在协商分页的 load 上）。修复需要索引 = DDL，未授权、未做。
5. **R5 继承区不进预算**：首段整份交付 `threads.inherited_context`（单行 JSON），内存上限不受
   `REPLAY_SEGMENT_BYTE_BUDGET` 约束。S9a 报告已自述此边界，本次未测它的大小分布。
6. **R6 单条超预算行独占一段**：设计如此（完整回放不丢内容），代价是单段内存可达该行大小（无上限）。
7. **R7 回退路径仍全量（实测）**：后端不实现 `load_history_segment` 时回退到 `load_session_history`
   全量读取（有 warn 日志、只在未交付任何内容前触发）。契约上显式，但它是「有界」的唯一例外分支。
8. **R8 新用例的日志断言在默认并发下会 flake（实测）**：
   `... test --locked -p peri-acp --lib -- replay` 首跑 `streamed_canonical_replay_falls_back_to_the_full_read_when_the_segment_read_is_unsupported`
   在 `requests_unnegotiated_replay_test.rs:783` 失败（`LogBuffer` 为空）；同命令复跑通过、单跑通过、`--test-threads=1` 通过。
   判定：测试侧并发干扰（不是产品缺陷——该用例的产品断言 `segment_calls==1`、`full_reads==1` 两次运行都通过）。
   切片自身的 S9b 报告记录过**另一条**全量 lib flake，本条是新文件里的新 flake。
9. **R9 文档引用不存在**：`session_history_segment_test.rs:4` 指向 `remote/session_history_segment_test.rs`，
   该文件不存在（远端段用例实际在 `remote/session_history_page_test.rs`）。
10. **R10 断言强度弱于结论**：`assert_segment_plan_bounded` / `assert_located_by_index` 不检查
    `USE TEMP B-TREE FOR ORDER BY`，也不检查行触达；结论里「行数有界」目前由本次审计的字节码判读支撑，
    没有固化成断言（夹具无关、可加）。
11. **R11 指令遵守**：本次未做任何 DDL、未加依赖、未做 git 写操作；审计新增代码只在一个既有 `#[ignore]`
    用例文件内（`session_history_perf_test.rs:1116-1334`），删掉即回到切片原状。

## 7. 复现命令清单

```bash
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history

# 1) 计划证据（默认运行，无夹具）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib \
    -- history_segment_statements_are_index_located --nocapture

# 2) 审计用例：字节码 + 探针 + 段长/字节实测 + 反例（日志见 .recon/S9-boundedness-raw.log）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib \
    -- audit_segment_lookahead_and_plan_shape --ignored --nocapture

# 3) 两后端段/页语义
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- history_segment
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- history_page

# 4) ACP 接线与 flake 复现
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- unnegotiated_replay --test-threads=1
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- replay      # 注意：并发下会 flake

# 5) 既有性能/计划证据（本次只复取计划，未复跑 1M 基准）
PERI_PERF_ROWS=200 PERI_PERF_ITERS=3 PERI_PERF_PAGES=3 \
    ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib \
    -- history_perf_restore_and_paging_evidence --ignored --nocapture
```

## 8. 环境备注

1. 有一次 cargo 调用出现 `Blocking waiting for file lock on build directory`（54 s）——同 worktree 内有
   另一个 cargo 进程（其会话不在本审计控制内）。所有测试结论均在同一串行前提下取得，不影响语义判定，
   但影响任何时长数字（本报告未给时长结论）。
2. rustc/sqlx 捆绑 SQLite 3.46.0（计划文本来自该版本）；机器为 macOS aarch64。
