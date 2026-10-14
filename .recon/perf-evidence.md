# R3：有界读取的性能证据（需求 §9 的 A04 / A05 / A06 / A07）

日期：2026-10-13。worktree：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
（分支 `feat/transcript-shrink-history-pagination`）。**未改动任何生产语义**：为了让计划证据拿到
生产语句原文，只把 6 条 SQL 文本提成 crate 内常量、给 `meta_by_id_statement()` 一个函数入口
（见 §9）；测量暴露的问题只报告、未修（见 §8）。

## 0. 一条命令跑完

```bash
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- history_perf_tests --ignored --nocapture
```

- 默认跑 3 个规模：**10k / 100k / 1M** 归档行；`iters=20`（冷、热各 20 次）；分页在**最大规模**上跑 20 页。
- 环境变量：`PERI_PERF_ROWS`（默认 `10000,100000,1000000`，逗号分隔）、`PERI_PERF_ITERS`（默认 20）、
  `PERI_PERF_PAGES`（默认 20）、`PERI_PERF_TRIPWIRE_ROWS`（默认 10000）。
- 单跑一条用例：把 `history_perf_tests` 换成用例名，例如
  `-- history_perf_oversize_page_carries_one_body --ignored --nocapture`。
- 四条用例全部 `#[ignore]`，且模块是 `#[cfg(test)]`：**默认测试运行不受影响**（见 §10 的库全量结果）。
- 注意需求里写的 `--test <harness name>` 形式在这里不适用：夹具是 `peri-resources` 库内的
  `#[cfg(test)]` 模块（要能直接引用 crate 内常量与 `super::*`），所以用 `--lib -- <模块名>` 过滤。

原始日志（本次实际输出，未编辑）：`.recon/perf-raw-3sizes.log`（默认三规模）、
`.recon/perf-raw-10k-paging.log`（只跑 10k 规模，分页证据落在 10k 上）。

## 1. 测量口径（先声明，避免误读）

1. **夹具**：临时目录 + `Resources::open_with(Some(db))` 经门面 `create_session` 建会话，
   历史行直接写进**同一张 `messages` 表**（生产 `append_history` 就是这张表），列与索引复用生产
   schema，没有第二套 schema。归档段 `excluded = 1`、有效段 `excluded = 0`（生产里由 Compact
   把旧前缀置成同一个行状态），行 payload 用生产的 `serialize_persisted_payload` 编码。
2. **固定有效集合**：3 条 × 64 KiB 正文（需求只固定有效条数与正文大小）；归档正文 1 KiB。
   有效行在 `rowid` 序上位于归档段之后（与 Compact 后「旧前缀被排除、新后缀有效」同形）。
3. **计划**：对**生产语句原文**加 `EXPLAIN QUERY PLAN` 前缀（语句文本取自实现里的常量，不重抄 SQL）。
4. **扫描行数**：`SELECT COUNT(*) FROM messages WHERE thread_id = ?1` **行数探针**——报告的是
   「这条线程在索引范围里有多少行」，不是执行期实际读了多少页。索引 `idx_messages_thread_id(thread_id)`
   对 `excluded` / `content` 不覆盖，所以范围行数就是这一步**允许触达**的行数上界（见 §8）。
5. **查询次数**：按代码路径逐条列出（附 `file:line`），**不是**执行期计数。
6. **字节**：只报实测值——解码回来的 payload 正文字节、`page_count × page_size` 与 `dbstat` 的落库占用。
   页级实际读盘字节在 sqlx 公开面上不可观测（本任务不得新增依赖），故 §7 标为未测量。
7. **冷/热**：冷 = 每次测量前新开一个部署句柄（新连接池 ⇒ SQLite 页缓存为空），句柄打开不计入计时；
   热 = 同一句柄连续读取。**两者都不刷新 OS 页缓存**（无权限），所以「冷」只到 SQLite 页缓存这一层。
8. **profile**：`test`（`unoptimized + debuginfo`）——下面的毫秒数是 **debug 构建**的数，不是 release 数。
9. 机器同时是全盘 99% 占用、4 KB 块 fsync 写只有 ~0.5 MB/s 的存储（见 §2），夹具构建与读时延都受它影响。

## 2. 环境（实测）

| 项 | 值 |
| --- | --- |
| 平台 | macos aarch64 |
| CPU | Apple M5 Pro |
| rustc | 1.99.0 (b940084d7 2026-09-28)，LLVM 23.1.1 |
| 构建 profile | `test`（unoptimized + debuginfo） |
| SQLite | 3.46.0（sqlx 捆绑），`page_size = 4096` |
| 存储实测 | 顺序写 1 MiB 块 + fsync：**77 MB/s**；4 KiB 块 + fsync：**0.52 MB/s** |

最后一行是本机噪声来源，也解释 §6 的夹具构建耗时：SQLite 按 4 KiB 页落盘，正好落在慢路径上
（同一份数据用 1 MiB 块写快 150 倍）。跑测试期间**没有**其他 cargo 命令并发；机器 load average 6~8。

## 3. 原始 `EXPLAIN QUERY PLAN`（生产语句原文，逐条 verbatim）

恢复链（`SqliteSessionData::load_active_snapshot`，`sqlite_store/session_data.rs:375`）7 条语句：

```text
· load_meta_on — 语句: SELECT t.id, t.title, t.cwd, t.created_at, t.updated_at, t.message_count,
    0 as content_size,
    t.parent_thread_id, t.snapshot_at_message_id, t.hidden, t.cancel_policy, t.config, t.agent_status FROM threads t WHERE t.id = ?1
      #3 SEARCH t USING INDEX sqlite_autoindex_threads_1 (id=?)
· binding_row_state_on — 语句: SELECT schema_version, project_id, workspace_id, relative_cwd
         FROM session_bindings WHERE thread_id = ?1
      #3 SEARCH session_bindings USING INDEX sqlite_autoindex_session_bindings_1 (thread_id=?)
· frozen_bytes_on — 语句: SELECT frozen_context FROM threads WHERE id = ?1
      #3 SEARCH threads USING INDEX sqlite_autoindex_threads_1 (id=?)
· load_active_payloads_on — 语句: SELECT message_id, content FROM messages WHERE thread_id = ?1 AND excluded = 0 ORDER BY rowid
      #4 SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)
· load_active_flags_on — 语句: SELECT message_id, truncated, excluded, projection FROM messages WHERE thread_id = ?1 AND excluded = 0 AND (truncated = 1 OR projection IS NOT NULL)
      #3 SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)
· load_inherited_context_on — 语句: SELECT inherited_context FROM threads WHERE id = ?1
      #3 SEARCH threads USING INDEX sqlite_autoindex_threads_1 (id=?)
· resolve_ancestor_chain_on — 语句: SELECT parent_thread_id FROM threads WHERE id = ?1
      #3 SEARCH threads USING INDEX sqlite_autoindex_threads_1 (id=?)
```

分页路径（`sqlite_store/session_data/history.rs`）4 条语句：

```text
· require_session（分页路径） — 语句: SELECT 1 FROM threads WHERE id = ?1
      #2 SEARCH threads USING COVERING INDEX sqlite_autoindex_threads_1 (id=?)
· read_active_entry_positions — 语句: SELECT message_id, rowid FROM messages WHERE thread_id = ?1 AND excluded = 0 ORDER BY rowid ASC
      #4 SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)
· read_history_page_bounds — 语句: SELECT
        (SELECT m.message_id FROM messages m WHERE m.thread_id = t.id ORDER BY m.rowid ASC LIMIT 1),
        (SELECT m.rowid FROM messages m WHERE m.thread_id = t.id AND m.message_id = ?2)
    FROM threads t WHERE t.id = ?1
      #2 SEARCH t USING COVERING INDEX sqlite_autoindex_threads_1 (id=?)
      #9 CORRELATED SCALAR SUBQUERY 1
      #19 SEARCH m USING INDEX idx_messages_thread_id (thread_id=?)
      #31 CORRELATED SCALAR SUBQUERY 2
      #36 SEARCH m USING INDEX sqlite_autoindex_messages_1 (message_id=?)
· read_history_page/阶段一 — 语句: SELECT message_id, rowid, length(CAST(content AS BLOB))
    FROM messages
    WHERE thread_id = ?1 AND (?2 IS NULL OR rowid < (
        SELECT m.rowid FROM messages m WHERE m.thread_id = ?1 AND m.message_id = ?2))
    ORDER BY rowid DESC LIMIT ?3
      #7 SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)
      #18 SCALAR SUBQUERY 1
      #23 SEARCH m USING INDEX sqlite_autoindex_messages_1 (message_id=?)
· read_page_bodies/阶段二 — 语句: SELECT content FROM messages WHERE thread_id = ?1 AND message_id = ?2
      #3 SEARCH messages USING INDEX sqlite_autoindex_messages_1 (message_id=?)
```

**断言（每次运行都执行，10k / 100k / 1M 三段都通过）**：

1. 没有任何一条语句文本或计划行出现 `SUM(`（历史里的全历史聚合 `SUM(LENGTH(content))` 不得回到热路径）。
2. 没有任何计划行出现 `SCAN messages` / `SCAN m`（`messages` 全表扫描）。
3. 计划里必须出现 `USING INDEX` / `USING COVERING INDEX` / `USING INTEGER PRIMARY KEY`
   （按索引定位；不断言具体索引名，索引换来换去不会让断言假绿）。
4. 计划采集耗时三段都 ≤ 0.2s（0.179s / 0.153s / 0.166s）。

计划断言只保证**计划形状**有界；它**不能**证明触达行数有界——后者见 §8，这是本次测量最重要的发现。

## 4. 每个夹具规模的数字

三段都来自同一次默认运行（`PERI_PERF_ROWS=10000,100000,1000000`、`iters=20`）。

| 规模（归档行） | messages 行数 | 归档正文合计 | 有效正文合计 | 库文件 | 库页数 | `messages` dbstat 占用 | 夹具生成 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 10k | 10 003 | 11 518 890 B | 196 971 B | 14 721 024 B | 3 594 | 13 889 536 B | 44.3 s（生成 0.27 s + INSERT 39.60 s） |
| 100k | 100 003 | 115 288 890 B | 196 971 B | 144 687 104 B | 35 324 | 137 072 640 B | 321.1 s（生成 2.75 s + INSERT 313.50 s） |
| 1M | 1 000 003 | 1 153 888 890 B | 196 971 B | 1 445 011 456 B | 352 786 | 1 368 969 216 B | 2192.6 s（生成 28.09 s + INSERT 2162.17 s） |

| 规模 | 恢复 cold p50 / p95 | 恢复 hot p50 / p95 | 恢复语句数（代码路径） | 返回 payload | 解码正文字节 | 排除正文解码条数 |
| --- | --- | --- | --- | --- | --- | --- |
| 10k | 11.109 ms / 11.675 ms | 11.032 ms / 11.494 ms | 7 | 3（flags 1） | 196 971 | **0** |
| 100k | 98.182 ms / 103.924 ms | 103.699 ms / 130.253 ms | 7 | 3（flags 1） | 196 971 | **0** |
| 1M | 1 081.927 ms / 1 230.147 ms | 1 084.641 ms / 1 282.421 ms | 7 | 3（flags 1） | 196 971 | **0** |

- 「排除正文解码条数」= 实测：恢复只返回 3 条有效 payload，解码字节恰好等于有效段字节
  （196 971 B，三段完全一致）；同一夹具换成人肉不可解码的归档正文时，`load_active_snapshot`
  仍然成功、解码字节不变（§6 的 A04 探针），而完整读取 `load_session_snapshot` 会失败。
- 「恢复语句数」按代码路径逐条数：`load_active_snapshot` 走 meta → binding → frozen →
  active payloads → active flags → inherited → ancestor（`session_data.rs:375`；
  `context.rs:108/142/185/252`、`session_data_helpers.rs:86/111`、`compaction.rs:188`），
  **不含**分页/位置读取。它不是执行期计数器。
- 冷 / 热在同一规模上几乎重合（10k：11.109 vs 11.032；100k：98.182 vs 103.699），因为「冷」只清
  SQLite 页缓存，OS 页缓存仍热（§1.7）。

### 4.1 1M 段

同一次默认运行（`PERI_PERF_ROWS=10000,100000,1000000`、`iters=20`、`pages=20`），命令与 §1 相同；
1M 段耗时 2616.36 s（含夹具生成），整段运行期间机器没有其它构建负载。

```text
  夹具分相：生成（uuid + 正文 + 序列化）28.09s，批量 INSERT 2162.17s（500 行/语句）
  夹具：messages=1000003 行（归档 1000000，有效 3），归档正文合计 1153888890 字节，有效正文合计 196971 字节，库文件 1445011456 字节，夹具生成 2192.6s
  sqlite_version: 3.46.0
  page_size/page_count: 4096/352786
  messages 表落库占用（dbstat）: 1368969216 字节
  计划采集耗时 0.166s
  恢复实测：payloads=3 flags=1 解码正文字节=196971 | 范围内行数（探针）=1000003 其中 excluded=1000000
  恢复的语句数（代码路径逐条列出，非执行期计数）：恢复链 7 条（session_data.rs 的 load_active_snapshot）+ 该链不含分页/位置读取
  恢复 cold（每次新句柄，SQLite 页缓存为空）: n=20 p50=1081.927ms p95=1230.147ms min=955.423ms max=1419.517ms mean=1084.425ms
      返回 payload=3 flags=1 解码正文字节=196971
  恢复 hot（同一句柄连续读取）: n=20 p50=1084.641ms p95=1282.421ms min=981.778ms max=1284.046ms mean=1101.310ms
      返回 payload=3 flags=1 解码正文字节=196971
  -- 分页：20 页 × limit=50 / 目标 524288 字节（同一句柄） --
    第  1 页: items=50 bytes=251209 oversize=0 最旧 position=999954 语句数（代码路径）= 53 p_ms=3.316
    第  2 页: items=50 bytes=57700 oversize=0 最旧 position=999904 语句数（代码路径）= 53 p_ms=3.601
    …（第 3–19 页同形：items=50、bytes=57700、oversize=0，最旧 position 每页递减 50）…
    第 20 页: items=50 bytes=57700 oversize=0 最旧 position=999004 语句数（代码路径）= 53 p_ms=2.964
  分页 hot（无游标首屏，重复读）: n=20 p50=3.073ms p95=3.503ms min=2.394ms max=3.832ms mean=3.068ms
  分页 20 页连续遍历（含游标页）: n=20 p50=2.772ms p95=3.340ms min=2.030ms max=3.601ms mean=2.743ms
```

1M 段要点：

1. **恢复的不变量在最大规模上成立**：返回 3 条 payload、解码正文恰好 196 971 B、被排除正文解码 0 条
   （归档正文合计 1.15 GB）。即"内存与解码字节有界"与归档规模无关。
2. **恢复耗时随归档行数线性增长**：cold p50 11.109 ms（10k）→ 98.182 ms（100k）→ **1081.927 ms（1M）**，
   每 10× 行数约 10× 耗时。原因与后果见 §8——这是本次测量最重要的负面发现，直接影响 A05。
3. **分页（更早方向）不受归档规模拖累**：1M 行上每页 50 条 / 57 700 B，单页 p_ms 2.0–3.6，
   20 页遍历 p50 2.772 ms，`position` 每页严格递减（999 954 → 999 004）说明游标在推进。
   每页语句数 53 = 3 + items.len()（§6 口径）。

### 4.2 复跑：`idx_messages_active` 在位（2026-10-14，A05 收窄实施后）

用户授权新增索引后（形状 `(thread_id, excluded)`，见 §8.1 与验收报告 §5.18.3），同一命令、同一默认
口径（`PERI_PERF_ROWS=10000,100000,1000000`、`iters=20`、`pages=20`）复跑一次；原始输出
`.recon/a05-raw/perf-rerun.out`（`test result: ok. 5 passed; 0 failed; …; finished in 104.74s`）。

| 规模 | 恢复 cold p50 / p95 | 恢复 hot p50 / p95 | 恢复语句数 | 返回 payload | 解码正文字节 | 排除正文解码条数 |
| --- | --- | --- | --- | --- | --- | --- |
| 10k | **3.299 ms** / 6.027 ms | **2.322 ms** / 4.351 ms | 7 | 3（flags 1） | 196 971 | **0** |
| 100k | **3.379 ms** / 4.631 ms | **2.092 ms** / 2.490 ms | 7 | 3（flags 1） | 196 971 | **0** |
| 1M | **2.435 ms** / 2.900 ms | **2.105 ms** / 2.401 ms | 7 | 3（flags 1） | 196 971 | **0** |

1. 对照 §4 的授权前基线（11.109 / 98.182 / **1 081.927** ms）：三段分别改善 ≈3.4× / 29× / **444×**，
   并且**跨三个规模基本持平**（3.30 / 3.38 / 2.44 ms，差异与页缓存/噪声同量级）——
   「恢复工作量不随归档条数线性增长」（A05）本次**成立**。
2. 计划证据：三条有效范围语句（`load_active_payloads_on`、`load_active_flags_on`、
   `read_active_entry_positions`）都从 `SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)`
   变成 `SEARCH messages USING INDEX idx_messages_active (thread_id=? AND excluded=?)`（三段一致；
   分页两条仍走 `idx_messages_thread_id` 的 keyset 定界，本就不受影响）。
3. 索引的落库代价（三次运行的库文件/页数之差，同一夹具口径）：
   | 规模 | 库文件（授权前 → 复跑） | 差 | 库页数差 | 每行≈ |
   | --- | --- | --- | --- | --- |
   | 10k | 14 721 024 → 14 970 880 B | +249 856 B | +61 页 | 25.0 B/行 |
   | 100k | 144 687 104 → 147 185 664 B | +2 498 560 B | +610 页 | 25.0 B/行 |
   | 1M | 1 445 011 456 → 1 470 291 968 B | +25 280 512 B | +6 172 页 | 25.3 B/行 |
   即 `(thread_id, excluded)` 索引约占 **25 B/行**（含 rowid 与两列编码），相对正文为主的库体
   （1M 档 1.4 GB）是 ≈1.7%。
4. **仍未测量（不得当成已证）**：① 升级路径「首次补建索引」的一次性耗时（1M 行的单趟建索引；
   本报告没有该数字，只有上面的稳态占用）；② 「实际触达行数」计数器——sqlx 公开面上没有，
   结论由计划形状 + 跨规模耗时给出（口径见模块 doc 第 4 条）；③ 夹具生成耗时三次运行差异很大
   （1M：2192.6 s → 87.7 s），本表的**耗时对照只取恢复/分页，不取夹具生成**。

## 5. 三条补充用例（A04 探针 / A06 字节预算 / A07 oversize）

各自一条命令（`-- <用例名> --ignored --nocapture`），都是实测。

### 5.1 A04：被排除的正文一条都不解码（探针夹具，不可解码归档正文）

夹具：10 000 归档行，正文是 45 B 的**不可解码占位**（合计 450 000 B），有效段照旧 3 × 64 KiB。

```text
  夹具分相：生成（uuid + 正文 + 序列化）0.02s，批量 INSERT 8.17s（500 行/语句）
== 探针夹具：归档 10000 行，正文为不可解码占位（45 字节/行，合计 450000 字节） ==
  对照组 load_session_snapshot 失败（预期）：session data is corrupt: stored session data is not readable
  处理组 load_active_snapshot 成功：payloads=3 flags=1 解码正文字节=196971（归档 10000 行正文一条未解码）
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 509 filtered out; finished in 14.27s
```

对照组失败 + 处理组成功，两者读的是同一份数据：归档正文**一条都没有进入解码器**。

### 5.2 A07：单条正文超过页字节目标

夹具：6 条 × 1 250 000 B 正文（合计 7 500 750 B），页字节目标 524 288 B、limit 50。

```text
== oversize 夹具：6 条 × >1 MiB 正文（合计 7500750 字节），页目标 524288 字节 ==
    第 1 页（有效段）: items=3 bytes=196971 oversize=0
    第 2 页: items=1 bytes=1250125 oversize=true position=6 语句数（代码路径）= 4 p_ms=5.527
    第 3 页: items=1 bytes=1250125 oversize=true position=5 语句数（代码路径）= 4 p_ms=5.217
    第 4 页: items=1 bytes=1250125 oversize=true position=4 语句数（代码路径）= 4 p_ms=5.268
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 509 filtered out; finished in 43.76s
```

超预算的单条独占一页（`items=1`）、显式 `oversize`、正文完整可解码、`position` 严格更旧（6 → 5 → 4）、
`next_cursor` 每页都在前进。首屏先给有效段（3 × 64 KiB = 196 971 B 都放得进预算），
所以「超预算独占一页」的证据从第 2 页起。

### 5.3 A06：页长由字节预算而不是条数上限决定

夹具：200 条 × 约 20 480 B（合计 4 121 290 B），页字节目标 524 288 B、limit 50。

```text
== 字节预算夹具：200 条 × 约 20480 字节正文（合计 4121290 字节），页目标 524288 字节 ==
    首屏页: items=18 bytes=506076（96.5% 预算）p_ms=3.840 语句数（代码路径）= 21
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 509 filtered out; finished in 21.99s
```

`items=18 < limit=50`、`506 076 B ≤ 524 288 B`、且「再加一条就放不下」（506 076 + 20 480 > 524 288）：
这一页是字节预算截断的，不是条数截断的。语句数 21 = `3 + items.len()`（见 §6 口径说明）。

## 6. 夹具构建耗时为什么这么长

INSERT 分相实测：10k → 39.60 s、100k → 313.50 s、1M → 2162.17 s（500 行/语句批量写入，
每批一次事务提交）。生成分相（uuid + 正文 + JSON 序列化）只有 0.27 s / 2.75 s / 28.09 s，
即耗时几乎全在写盘：库文件 14.7 MB / 144.7 MB / 1.445 GB，`page_size = 4096`。
§2 实测本机 4 KiB 块 + fsync 只有 **0.52 MB/s**（1 MiB 块 77 MB/s，差 150 倍），
所以行数翻 10 倍、写入时间也近似翻 10 倍——这是**机器噪声**，不是被测代码的复杂度。

## 7. 未测量项（不得当成已证）

1. **页级读盘字节**：sqlx 公开面拿不到实际读盘字节，只报了库文件/页数/dbstat 与解码字节。
2. **峰值内存 RSS**：只按代码路径论证"被排除正文不进解码器、段/页读取有界"，没有采样 RSS。
3. **OS 页缓存的冷**：冷测量只清 SQLite 页缓存（新句柄），OS 页缓存仍热（§1.7）。
4. **release 构建**：全部数字来自 debug/test profile（§1.8），不得作为发布性能承诺。
5. **远端后端**：本组测量只覆盖 SQLite adapter；Remote（`sessions/remote/**`）只做了契约层对齐。
6. **持续浏览后的累计缓存**：见 §8 末尾，1M 规模上只测了 20 页，没有测"浏览到很深处"的驻留量。

## 8. 计划形状有界 ≠ 触达行数有界（授权前基线；**2026-10-14 已由索引闭合，见 §8.1**）

**结论**：恢复路径的**内存与解码字节**与归档规模无关（196 971 B 恒定），
但它**触达的存储行数与线程总行数同阶**，因此恢复耗时随归档行数线性增长：

| 归档行 | 恢复 cold p50 | 归档正文合计 | 库文件 |
| --- | --- | --- | --- |
| 10k | 11.109 ms | 11.5 MB | 14.7 MB |
| 100k | 98.182 ms | 115.3 MB | 144.7 MB |
| 1M | **1081.927 ms** | 1 153.9 MB | 1445.0 MB |

每 10× 归档 ⇒ 约 10× 冷恢复耗时（11 → 98 → 1082 ms），而三段返回的有效集合完全相同。

**机制（静态判断，与上表一致）**：`load_active_payloads_on`
（`SELECT message_id, content FROM messages WHERE thread_id = ?1 AND excluded = 0 ORDER BY rowid`）
的计划是 `SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)`。
`idx_messages_thread_id` 只覆盖 `thread_id`，`excluded`/`content` 都在表里，因此这一档是
**该线程索引范围内的范围搜索 + 逐行回表判 `excluded`**：被排除的行虽然不进内存、不解码，
但每个行都要读一次表页（正文就在行内），实际触达的上界就是 §1.4 的行数探针
（10 003 / 100 003 / 1 000 003 行）。同形的还有 `load_active_flags_on`（`ACTIVE_FLAGS_SQL`）
与 `read_active_entry_positions`（`ACTIVE_ENTRY_POSITIONS_SQL`）：三条都是"在 SQL 里过滤"
（满足 REQ-T03「不得先全量再过滤」的字面要求），但都**不是**"触达行数与有效集合同阶"。

**影响**：
- 验收 A04 的「禁止全历史聚合 / flags 全扫描 / 先全量再过滤」：聚合已消除（`SUM(LENGTH(content))` 已删）、
  没有全表扫描、过滤在 SQL 里完成 —— 但"扫描量"这一项只算部分满足（线程内范围扫描仍在，见上）。
- 验收 A05 的「恢复工作量不随归档条数线性增长」：**未达成**，实测呈线性（上表）。
  （授权前基线；2026-10-14 索引实施后已达成，见 §4.2 与 §8.1。）
  可区分的部分是：解码/反序列化的排除正文仍为 0 条，内存有界这一半是成立的。
- 分页（更早方向）**不**受这个影响：keyset 从锚点向后按索引定位（1M 上每页 2–3.6 ms，§4.1），
  这是"计划有界且行数也有界"的对照组。
- **修复方向（未做）**：需要一条覆盖 `(thread_id, excluded)`（或等价的 `(thread_id, excluded, rowid)`）
  的索引，让排除判定在索引内完成、不再逐行回表。这属于**新增索引 = DDL**：
  需求 §8 明确"不授权直接执行数据库 DDL"，仓库根 CLAUDE.md 原则 12 也要求先设计评审 + 用户批准。
  因此本次只报告，未改 schema；批准后另开切片补索引并复跑本节三个规模即可闭环。
  → **2026-10-14：用户显式授权该索引，已实施并复跑，闭环见 §8.1。**

### 8.1 索引实施与闭环（2026-10-14）

1. **授权与实施**：用户显式授权 A05 新增索引。落点是 canonical 单一来源
   `peri-resources/src/sessions/canonical.rs::CREATE_INDEXES`：
   `CREATE INDEX IF NOT EXISTS idx_messages_active ON messages(thread_id, excluded)`
   （形状 = 本节给出、A2 实验实测过的那一条）。本机 `sqlite_store/schema.rs` 的 **Current 分支**
   同时补一次幂等索引重放——形状判定只覆盖表与列，不补则上一版构建建出的库拿不到新索引；
   新建、v11 迁移终点、远端初始化与远端升级批次本就走同一份清单。
2. **实测结果**（§4.2）：三段恢复 cold p50 3.299 / 3.379 / 2.435 ms，跨规模持平；
   计划三条有效范围语句全部改为 `SEARCH messages USING INDEX idx_messages_active
   (thread_id=? AND excluded=?)`。授权前的 11.109 / 98.182 / 1 081.927 ms 线性曲线消失。
3. **代价**：索引落库占用 ≈25 B/行（§4.2 第 3 条，10k/100k/1M 三点一致，1M 档约 +25.3 MB / +1.7%）；
   升级路径首次补建的一次性耗时**未测量**。
4. **边界**：`idx_messages_thread_id` 未删除（只按线程取全部行的路径仍在用，例如
   `read_history_page/阶段一` 与 `read_history_page_bounds`）；两个索引并存是当前形状。

**持续浏览后的累计缓存（需求 §9 要求区分的第二类成本）**：本组数据只覆盖
"初始恢复"与"连续 20 页"两种；20 页驻留量 ≈ 20 × 57 700 B ≈ 1.15 MB 量级（页内容），
没有测"翻到几千页后前端驻留多少"——那属于前端累计缓存，须由前端切片（S7）与其验证补充，
本节不宣称任何结论。

## 9. 为拿到计划证据做的代码改动（无生产语义变化）

1. 6 条 SQL 文本提成 crate 内常量，供测试引用**生产语句原文**（不重抄）：
   `ACTIVE_PAYLOADS_SQL`（`context.rs:137`）、`INHERITED_CONTEXT_SQL`（`context.rs:178`）、
   `PARENT_THREAD_ID_SQL`（`context.rs:182`）、`ACTIVE_FLAGS_SQL`（`compaction.rs:182`）、
   `HISTORY_PAGE_BOUNDS_SQL` / `HISTORY_PAGE_ROWS_SQL` / `HISTORY_PAGE_BODY_SQL` /
   `ACTIVE_ENTRY_POSITIONS_SQL`（`session_data/history.rs:13/29/36/40`），
   以及 `BINDING_ROW_STATE_SQL` / `FROZEN_CONTEXT_SQL` / `THREAD_EXISTS_SQL`
   （`session_data_helpers.rs:74/79/83` 与 `context.rs` 的 `meta_by_id_statement()`）。
2. `meta_by_id_statement()` 把 `threads` 元数据投影（含 `0 as content_size` 槽位）变成函数入口，
   使计划证据与生产热路径同文。
3. 夹具与计时全部落在 `#[cfg(test)] mod history_perf_tests`（四条用例 `#[ignore]`），
   默认测试运行不生成大夹具、不计入时长。

## 10. 默认测试面（不受影响）

四条性能用例都在 `#[ignore]` 下，模块是 `#[cfg(test)]`：跑单条用例时输出
`509 filtered out`（该 crate 的 lib 测试面 510 条），默认 `--lib` 全量运行不会生成任何大夹具。
本 run 之外没有改任何生产语义（只做 §9 的常量提取）。
