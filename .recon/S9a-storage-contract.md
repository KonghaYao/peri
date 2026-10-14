# S9a — 存储与契约切片：canonical 完整回放的**向前段**读取

本文件只记录本切片（S9a）的落地与证据。设计与编排来源：
`.recon/workflows/s9-standard-replay.mjs`、`spec/issues/2026-10-13-transcript-shrink-history-pagination.md`。

## 1. 范围

1. **做**：为 REQ §5.3「未协商 `peri.historyPagination` 的标准客户端仍须拿到 canonical 完整历史」准备**有界流式读取**的契约与两个生产后端实现：
   - 契约类型：`HistoryForwardCursor` / `HistorySegment` / `HistorySegmentPlan`（+ 存储序键文本别名）。
   - trait 方法：`SessionResources::load_history_segment`（默认 `Unsupported`）。
   - 后端：SQLite（`sqlite_store`）与远端（`remote`）各一条真实读取 + 4 处转发。
   - 测试：SQLite 语义 8 项、契约规则 8 项、Remote 最小契约 3 项、计划证据 1 项。
2. **不做**（留给后续切片或明确排除）：ACP 出口接线（S9b：`Unsupported` 时显式回退 `load_session_history` 并记日志）、wire 形状变更、DDL、新依赖、`load_history_page` / `load_session_history` 既有语义。
3. **硬约束遵守情况**：未新增依赖、未做任何 DDL（无表/列/索引/schema 变更）、未改既有 ACP 字段名与错误码、未修改既有两条读取方法的语义（只新增）。
4. **未提交**：本次工作全程未执行任何 git 写操作（无 commit/stash/checkout/reset/clean/分支/config）。工作树内大量 `M` 文件属前序切片 S1–S8，非本切片引入。

## 2. 改动文件清单

前缀省略：`=/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history/`

### 契约层（peri-acp-types）

1. `/peri-acp-types/src/history_page.rs` — 新增 `StorageOrderKey`、`HistoryForwardCursor`（`new`/`encode`/`decode`/`rowid_bounds` + 私有 `rowid_of` + 版本化 wire 结构）、`HistorySegment`、`HistorySegmentPlan`（`plan`/`body_ids`/`materialize`）、私有 `invalid_input`；模块 doc 增加「canonical 完整回放（向前方向）」段落。
2. `/peri-acp-types/src/session_resources.rs` — `SessionResources` 新增 `load_history_segment`（默认 `Unsupported`，doc 写全首段/续段/上界/有界/两阶段/失败关闭规则）+ 导入 `HistoryForwardCursor, HistorySegment`。
3. `/peri-acp-types/src/history_page_test.rs` — 新增 7 条向前段契约测试 + 夹具 `segment_ids` / `read_segment`；顺带修正 `fn reminder` 处一处被压行的空行（纯格式，无行为变化）。
4. `/peri-acp-types/src/session_resources_test.rs` — 新增测试替身 `SegmentlessResources`（33 个必需方法 `unimplemented!()`）与测试 `load_history_segment_defaults_to_unsupported`（首段/续段都必须 `Unsupported`）。

### 存储层（peri-resources）

5. `/peri-resources/src/sessions/sqlite_store/session_data/history.rs` — 4 条 SQL 常量（`HISTORY_SEGMENT_TAIL_SQL` / `_HEAD_SQL` / `_ANCHOR_SQL` / `_NEXT_SQL`，均 `pub(in crate::sessions)`）+ `read_history_segment`（单事务、两阶段、复用 `read_page_bodies` 与 `context::load_inherited_context_on`）。
6. `/peri-resources/src/sessions/sqlite_store/session_data.rs` — `impl SessionDataPort for SqliteSessionData` 转发 `load_history_segment` + 导入。
7. `/peri-resources/src/sessions/sqlite_store/session_data_test.rs` — 声明新测试模块 `#[path = "session_history_segment_test.rs"] mod history_segment_tests;`。
8. `/peri-resources/src/sessions/sqlite_store/session_history_segment_test.rs`（新文件，340 行）— SQLite 段语义 8 项。
9. `/peri-resources/src/sessions/sqlite_store/session_history_perf_test.rs` — 新增 `segment_read_plans` / `assert_segment_plan_bounded`，新增**默认运行**的计划证据用例 `history_segment_statements_are_index_located`（4 条语句按索引定位、无 `SCAN messages`）。
10. `/peri-resources/src/sessions/data.rs` — `trait SessionDataPort` 新增 `load_history_segment` 默认实现（`Unsupported`）+ 导入。
11. `/peri-resources/src/sessions/resources.rs` — `impl SessionResources for SessionResourcesImpl` 转发到 `gate.data()` + 导入。
12. `/peri-resources/src/sessions/remote/session_sql.rs` — 5 条 SQL 常量（`SELECT_HISTORY_SEGMENT_{TAIL,HEAD,ANCHOR,NEXT}_SQL`、`SELECT_INHERITED_CONTEXT_SQL`）+ 5 个 `pub(super) fn ..._statement(...)` 构造器。
13. `/peri-resources/src/sessions/remote/session_read.rs` — `read_history_segment`：首段一批 3 条语句（上界 / 候选行 / 继承区）、续段一批 2 条（锚点存在性 / 候选行），阶段二复用 `decode_page_bodies` 与 `history_page_body_statement`。
14. `/peri-resources/src/sessions/remote/session_data.rs` — `impl SessionDataPort for RemoteSessionData` 转发 + 导入。
15. `/peri-resources/src/sessions/remote/session_history_page_test.rs` — 追加 3 条 Remote 段契约测试（与分页测试同一夹具）+ 模块 doc 一句补充 + `segment_ids` 夹具。

### 索引（最小补充）

16. `/docs/code-index/peri-acp-types.md` — 「改历史分页读取契约」行补 `HistoryForwardCursor` / `HistorySegment` / `HistorySegmentPlan` / `load_history_segment` 与一段向前方向不变量。
17. `/docs/code-index/peri-resources.md` — 「改历史分页读取（本机/远端同一语义）」行补 `load_history_segment`、4（本机）+ 5（远端）条新 SQL 常量、`read_history_segment`、向前段两阶段与失效语义、回归文件与计划证据用例名。

### 报告

18. `/.recon/S9a-storage-contract.md` — 本文件。

## 3. 契约与不变量

### 3.1 签名（S9b 依赖，不得改）

```rust
async fn load_history_segment(
    &self,
    id: &ThreadId,
    cursor: Option<&HistoryForwardCursor>,
    limit: usize,
    byte_budget: usize,
) -> SessionResourceResult<HistorySegment>;
```

1. **默认实现 `Unsupported`**：没有这条读取的后端必须显式失败，不得用空段冒充「回放结束」（空段在契约里是合法的「已到上界」）。
2. **段内容**：`HistorySegment { payloads, next_cursor }`。`payloads` 按 canonical 顺序 —— 继承区在前（**只在首段**出现，整份给出、不分页），其后是自有行按 `rowid` 升序；**不过滤** excluded 归档原文、System 条目、仅模型受众 reminder。
3. **固定上界（tail）**：`cursor = None` 时，首段在**同一批/同一事务**读出该线程那一刻的最大 `rowid` 作为上界，并随游标透传；续段谓词为 `rowid > after AND rowid <= tail`。上界是**数值**，不要求 `tail` 那一行还在（它可能已被 rewind 删除）。
4. **锚点**：`after` 那一行必须仍在，否则 `HistoryCursorError::Expired`（不回退成首段、不用空段结束）。
5. **有界**：阶段一最多取 `limit + 1` 行（多出的一条只用于判断「更晚方向还有内容」）且只读 `message_id` + `rowid` + 正文长度；阶段二**只**按本段的 id 取正文。字节预算按本段推进的每一行计；单条整行超预算时独占一段并仍给出游标（否则它之后的历史永远读不到）。
6. **游标形式**：线上 JSON `{"v":1,"after":"<十进制>","tail":"<十进制>"}`，`after < tail` 且均为非负十进制；形式/版本/缺失字段错误一律 `HistoryCursorError::Malformed`（复用既有错误类型，未新造错误）。两个方向的游标**不得互相解析**（各有结构版本，字段名不同，互相喂入必然失败）。
7. **失败关闭清单**：`limit = 0` 且还有剩余行 → `InvalidInput`；正文行数或身份与本段不符 → `Corrupt`；`has_more` 但取不到上界 → `Corrupt`；会话不存在 → `NotFound`；坏游标在**任何** I/O 之前失败。
8. **`next_cursor = None` 的唯一含义**：本次回放没有更多自有行（已到上界或本来没有行）。
9. **两个后端同形**：段规则只有一份实现（`HistorySegmentPlan`），SQLite 与远端只差执行器（远端批次内语句必须在发请求前定好，故阶段一/阶段二各一次批次）。

### 3.2 与既有能力的关系（未改）

1. `load_session_history`（全量、无界）与 `load_history_page`（协商分页、锚点 = 条目身份）**语义与 SQL 一字未改**；段读取是新增的第三条读取。
2. 未新增索引：受限读取全部落在既有 `idx_messages_thread_id` 上（`thread_id = ? AND rowid > ?` 形状），锚点存在性落在 `INTEGER PRIMARY KEY (rowid=?)` 上。
3. 未改动任何 ACP 字段名/错误码；`HistorySegment` 目前是**内部契约类型**（未定义 wire 形状），出口形状属 S9b。

## 4. 实测证据

所有命令均以 `cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history &&` 开头，前台串行执行（未并发跑多个 cargo），仅用 `./scripts/cargo-rmcp-patched.sh`。

### 4.1 规定命令（4 条，全部实跑）

1. `./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp-types`

```
   Checking peri-acp-types v0.2.0 (...)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.71s
```
（首次新鲜编译为 `Finished dev profile in 14.95s`；退出码 0，无 error/warning 来自本 crate）

2. `./scripts/cargo-rmcp-patched.sh check --locked -p peri-resources`

```
   Compiling peri-acp-types / peri-workflow / peri-resources
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.62s
```
（首次新鲜编译 `Finished dev profile in 30.41s`，退出码 0；唯一 warning 是无关的 hyper-util patch `unused variable: timer`）

3. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib -- history_segment`

```
running 8 tests
test session_resources::tests::load_history_segment_defaults_to_unsupported ... ok
test history_page::tests::a_history_segment_forward_cursor_only_parses_its_own_wire_form ... ok
test history_page::tests::a_history_segment_replay_advances_without_gap_or_duplicate_until_the_tail ... ok
test history_page::tests::a_history_segment_length_is_decided_by_bytes_not_by_limit ... ok
test history_page::tests::an_oversized_entry_takes_a_history_segment_alone_and_still_advances ... ok
test history_page::tests::a_history_segment_fails_closed_on_a_partial_reply_or_a_missing_tail ... ok
test history_page::tests::a_history_segment_with_a_zero_limit_fails_closed_instead_of_ending_the_replay ... ok
test history_page::tests::a_history_segment_delivers_every_entry_and_never_asks_for_the_lookahead_row ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 568 filtered out; finished in 0.00s
```

4. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- history_segment`

```
running 12 tests
test ...::history_segment_tests::history_segment_empty_thread_ends_without_a_cursor ... ok
test ...::history_segment_tests::history_segment_first_read_returns_the_inherited_context_and_fixes_the_tail ... ok
test ...::history_segment_tests::history_segment_only_inherited_context_has_no_own_rows ... ok
test ...::history_segment_tests::history_segment_returns_archived_and_system_rows_in_canonical_order ... ok
test ...::history_segment_tests::history_segment_tail_keeps_later_rows_out_of_this_replay ... ok
test ...::history_segment_tests::history_segment_fails_closed_when_the_after_row_is_gone ... ok
test ...::history_segment_tests::history_segment_gives_an_oversized_entry_its_own_segment ... ok
test ...::history_segment_tests::history_segment_length_is_decided_by_bytes_not_limit ... ok
test sessions::sqlite_store::session_data::history_perf_tests::history_segment_statements_are_index_located ... ok
test sessions::remote::session_history_page_tests::remote_history_segment_replay_matches_the_local_backend ... ok
test sessions::remote::session_history_page_tests::remote_history_segment_returns_archived_and_system_rows ... ok
test sessions::remote::session_history_page_tests::remote_history_segment_fails_closed_when_the_after_row_is_gone ... ok

test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 510 filtered out; finished in 1.87s
```

（过滤串按用例名调整：本切片的用例名统一含 `history_segment`，因此这一条同时覆盖 SQLite 语义 8 项、计划证据 1 项、Remote 契约 3 项。）

### 4.2 补充回归（实跑，整包）

1. `test --locked -p peri-acp-types --lib` → `test result: ok. 576 passed; 0 failed; 0 ignored`。
2. `test --locked -p peri-resources --lib` → `test result: ok. 497 passed; 0 failed; 25 ignored; 0 measured; finished in 60.30s`（25 ignored 是既有的 `--ignored` 性能夹具，按指示未跑）。

### 4.3 计划证据（实跑 `--nocapture`）

`test --locked -p peri-resources --lib -- history_segment_statements_are_index_located --nocapture`：

```
== 向前段读取的计划（EXPLAIN QUERY PLAN，生产语句原文；无夹具） ==
  · read_history_segment/边界（上界） — SELECT (SELECT MAX(m.rowid) FROM messages m WHERE m.thread_id = t.id) FROM threads t WHERE t.id = ?1
      #2 SEARCH t USING COVERING INDEX sqlite_autoindex_threads_1 (id=?)
      #14 SEARCH m USING COVERING INDEX idx_messages_thread_id (thread_id=?)
  · read_history_segment/首段（阶段一） — SELECT message_id, rowid, length(CAST(content AS BLOB)) FROM messages
      WHERE thread_id = ?1 AND rowid <= (SELECT MAX(m.rowid) FROM messages m WHERE m.thread_id = ?1)
      ORDER BY rowid ASC LIMIT ?2
      #7 SEARCH messages USING INDEX idx_messages_thread_id (thread_id=? AND rowid<?)
  · read_history_segment/非首段（阶段一） — ... WHERE thread_id = ?1 AND rowid > ?2 AND rowid <= ?3 ORDER BY rowid ASC LIMIT ?4
      #7 SEARCH messages USING INDEX idx_messages_thread_id (thread_id=? AND rowid>? AND rowid<?)
  · read_history_segment/锚点存在性 — SELECT (SELECT m.rowid FROM messages m WHERE m.thread_id = t.id AND m.rowid = ?2) FROM threads t WHERE t.id = ?1
      #2 SEARCH t USING COVERING INDEX sqlite_autoindex_threads_1 (id=?)
      #13 SEARCH m USING INTEGER PRIMARY KEY (rowid=?)
test ... ::history_segment_statements_are_index_located ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 521 filtered out; finished in 0.62s
```

断言口径（`assert_segment_plan_bounded` + `assert_located_by_index`）：每条计划都必须按索引定位，且禁止出现 `SCAN messages` / `SCAN s` / `SUM(`。该用例**默认运行**（不加 `--ignored`），因为它只看 schema 与索引，不需要大夹具。

### 4.4 覆盖矩阵（用例 ↔ 契约条目）

| 契约条目 | 覆盖用例 |
| --- | --- |
| 默认实现 `Unsupported`（首段/续段） | `load_history_segment_defaults_to_unsupported` |
| 游标形式与失败模式、两游标互不解析 | `a_history_segment_forward_cursor_only_parses_its_own_wire_form` |
| 多段推进无缺口/无重复、上界固定 | `a_history_segment_replay_advances_without_gap_or_duplicate_until_the_tail`、SQLite `..._first_read_returns_the_inherited_context_and_fixes_the_tail`、Remote `remote_history_segment_replay_matches_the_local_backend` |
| 上界排除回放期间追加的行 | SQLite `..._tail_keeps_later_rows_out_of_this_replay`、Remote（同上用例内追加后断言） |
| 不过滤 excluded / System / model-only | `a_history_segment_delivers_every_entry_and_never_asks_for_the_lookahead_row`、SQLite `..._returns_archived_and_system_rows_in_canonical_order`、Remote `remote_history_segment_returns_archived_and_system_rows` |
| 字节决定段长 / 单条超预算独占段 | `a_history_segment_length_is_decided_by_bytes_not_by_limit`、`an_oversized_entry_takes_a_history_segment_alone_and_still_advances`、SQLite 同名两条 |
| 阶段二只取本段正文 | `a_history_segment_delivers_every_entry_and_never_asks_for_the_lookahead_row`（`body_ids`）、`a_history_segment_fails_closed_on_a_partial_reply_or_a_missing_tail` |
| 少读/多读/错行/丢上界 → `Corrupt` | `a_history_segment_fails_closed_on_a_partial_reply_or_a_missing_tail` |
| `limit = 0` 且有剩余 → `InvalidInput` | `a_history_segment_with_a_zero_limit_fails_closed_instead_of_ending_the_replay` |
| 空线程 / 只有继承区 / 会话不存在 | SQLite `..._empty_thread_ends_without_a_cursor`、`..._only_inherited_context_has_no_own_rows`、Remote（`NotFound` 断言） |
| 锚点消失 → `Expired` | SQLite `..._fails_closed_when_the_after_row_is_gone`、Remote `remote_history_segment_fails_closed_when_the_after_row_is_gone` |
| 两个后端同形 | 同一夹具的 SQLite / Remote 对照（`remote/session_history_page_test.rs` 模块 doc + 两处用例里的「与本机 adapter 同一段」断言） |
| 定界读取（无全表扫描） | `history_segment_statements_are_index_located` |

## 5. 未验证与不确定项

1. **未跑**：任何 `--ignored` 大夹具性能用例（按指示不跑）；`cargo fmt --check` / `lefthook run pre-commit` 未执行 —— 新增代码按仓库既有格式手工排版，未经 rustfmt 机器校验。
2. **未跑**：E2E（`e2e/`）与 TUI 集成用例；本切片没有出口行为变化。
3. **未实现**：ACP 侧消费（S9b）——当下没有调用方调用 `load_history_segment`，因此它的**端到端**效果（标准客户端拿到完整历史）尚未被任何测试覆盖；本切片只保证契约与两个后端读取正确。
4. **未验证**：`HistorySegment` 的 wire 形状未定（未实现 `Serialize`/`Deserialize`）。若 S9b 需要跨进程传输，需另行决定（例如按 `PersistedPayload` 数组 + 游标字段）。
5. **静态判断（非实测）**：`after` 为 `String` 形式时不得直接与 `rowid` 比较（SQLite 存储类排序 `INTEGER < TEXT` 恒成立）——两个后端都在绑定参数之前走 `rowid_bounds()`，该结论由代码结构保证，但**没有**针对「绕过 `rowid_bounds` 直接绑文本」的回归用例（该路径不存在）。
6. **偏差（有意）**：需求文字里 `HistoryForwardCursor` 的字段写作 `MessageId`，实现取「存储序键的字符串形式」（`pub type StorageOrderKey = String`，即 `rowid` 十进制文本）。理由：`tail` 是数值上界、不要求那一行存在，`rowid` 会复用，用条目身份无法表达上界。
7. **偏差（有意）**：D 项把「默认实现 `Unsupported`」列在 SQLite 语义清单里，实际放在 `peri-acp-types/src/session_resources_test.rs`（`SessionResources` 所在 crate）——那才是第三方实现要满足的契约。
8. **不确定**：`rowid_of` 用 `str::parse::<i64>()`，因此 `"+7"` 这类带正号的文本会被接受为合法存储序键（拒绝的是负号、空格、空串、小数）。本实现永远只铸造十进制无符号文本，实际不可达；未把它写成用例，以免把这一宽松性固化进契约。
9. **后端能力核对（静态判断，实测支持）**：两个生产后端都能表达 `tail` 上界（共享 canonical schema，`messages.rowid` 就是存储序键，`MAX(rowid)` + `rowid <= ?` 均可表达），因此**没有**后端被降级为 `Unsupported`；只有「没实现这条读取」的第三方/替身后端走 trait 默认实现。
10. **未改**：已知偏差 D8（rewind 到排除段内部时原文与摘要可能同时有效）不在本切片修（需 DDL），记录仍在 `peri-acp-types/src/store/history.rs` 模块文档与 `.recon/rewind-review-findings.md`。

## 6. 风险

1. **调用方遗漏预算**：`load_history_segment` 的有界性完全依赖调用方给出的 `limit` / `byte_budget`。若 S9b 传 `usize::MAX`，一次回放会退化成「全量读进内存」——本切片的选择是「契约不设上限、由出口决定」，需要 S9b 明确预算值并在那里约束。
2. **首段内存峰值**：继承区（`threads.inherited_context` 单行 JSON）整份跟在首段，不参与预算；Full Compact 后它可能较大。当前实现不改这一既有事实（`load_session_history` 也是整份加载），但它是首段唯一的「非有界」部分，建议 S9b 记录实测峰值。
3. **远端批次形状**：首段一次批次发 3 条语句、续段 2 条，阶段二一批 N 条（N = 本段行数）。执行器要求批次内语句在请求前定好，因此**不可能**把两阶段合成一个批次；若未来执行器对批次数有限制，这里会先受影响。
4. **`limit = 0` 的语义**：契约选择「有剩余行即 `InvalidInput`」而不是返回空段，调用方若把错误当「没有历史」会再次造成截断——错误分类为 `InvalidInput`（不是 `NotFound`），出口侧必须显式区分。
5. **`HistoryForwardCursor` 无结构版本绑定到存储提交**：与 `HistoryCursorEnvelope` 不同，段游标不携带「历史范围结构版本」。rewind 删除 `after` 那一行会让续段 `Expired`（失败关闭），但若 rewind **只删掉 `tail` 之后**的行或插入新行，本次遍历仍按旧上界继续（这正是设计意图：上界是数值）。破坏范围却不删除锚点又改变 `rowid` 语义的场景未在测试中构造。
6. **测试替身成本**：`SegmentlessResources` 需要 33 个 `unimplemented!()` 方法，`SessionResources` 每加一个必需方法都要同步这个替身（编译期会发现，不会静默）。
