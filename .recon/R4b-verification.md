# R4b 验证报告：R1–R3 的回归验证（rewind 如实回报 + 分页两阶段 + 协商边界）

日期：2026-10-13。worktree：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
（分支 `feat/transcript-shrink-history-pagination`）。**未改动任何生产代码**：本次只写了本文件。

基线：`git status` 显示 101 个已跟踪文件被修改 + 11 个未跟踪条目（9 个 `.rs`、`.recon/` 与
`spec/issues/2026-10-13-transcript-shrink-history-pagination.md`）。本报告的所有
行号都取自**当前**工作树（脏树即交付物）。

## 1. 命令与观测结果

全部在 worktree 根执行，`cargo-rmcp-patched.sh` 为仓库包装脚本。

| # | 命令 | 观测（精确总结行） | exit |
| --- | --- | --- | --- |
| 1 | `check --locked -p peri-acp-types -p peri-resources -p peri-acp -p peri-agent --all-targets` | ``Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 18s``；**零 warning** | 0 |
| 2 | `test --locked -p peri-acp-types --lib` | `test result: ok. 568 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s` | 0 |
| 3 | `test --locked -p peri-resources --lib` | 第 1 次：`test result: FAILED. 483 passed; 2 failed; 25 ignored; 0 measured; 0 filtered out; finished in 55.72s`；第 2 次：`test result: FAILED. 484 passed; 1 failed; 25 ignored; 0 measured; 0 filtered out; finished in 54.07s`（两次失败集合**不同**，见 §1.1） | 101 |
| 3a | 复跑 `sessions::open_tests::explicit_read_only_shares_selection_and_writes_nothing --exact` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 509 filtered out; finished in 1.00s` | 0 |
| 3b | 复跑 `sessions::sqlite_store::connection::open_tests::failed_migration_closes_connections_and_releases_initialization_lock --exact` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 509 filtered out; finished in 1.48s` | 0 |
| 3c | 复跑整包 `test --locked -p peri-resources --lib`（第 2 次） | 见 #3 第 2 次；失败换成 workspace 探针（见 §1.1） | 101 |
| 3d | 复跑 `sessions::sqlite_store::workspace::tests::probe::test_worktree_registration_probes_filesystem_outside_write_lock --exact` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 509 filtered out; finished in 1.89s` | 0 |
| 4 | `test --locked -p peri-acp --lib` | `test result: ok. 863 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 255.17s` | 0 |
| 5 | `test --locked -p peri-agent --lib` | `test result: ok. 1089 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 13.95s` | 0 |

### 1.1 关于 #3 的失败（判定为环境性 flake，非回归）

第 1 次失败名单（原样）：

```
sessions::open_tests::explicit_read_only_shares_selection_and_writes_nothing
sessions::sqlite_store::connection::open_tests::failed_migration_closes_connections_and_releases_initialization_lock
```

两条都在 `open_tests` / `connection::open_tests` 这一族——任务点名要先单跑复验的锁探针家族；**串行单跑都通过**
（3a/3b）。

第 2 次（3c）这两条**都通过**了，失败换成另一条、且是另一种失败原因：

```
sessions::sqlite_store::workspace::tests::probe::test_worktree_registration_probes_filesystem_outside_write_lock
    called `Result::unwrap()` on an `Err` value: workspace discovery failed: Git discovery timed out
```

该条单跑也通过（3d：1.89s，而整包里它超时）。**两次整包失败的集合互不相交、模块互不相关**，且失败原因分别是
「锁/只读选择」与「git discovery 超时」——都是负载相关的探针，与被验证的 R1–R3 改动（rewind 语义、分页两阶段、
能力协商）没有调用关系；#4、#5 在同一环境下全绿。

环境说明（如实记录，不掩饰）：整包运行期间，**上一阶段（R3）遗留的一个后台 cargo 进程仍在运行**
（`perf-all.log` 里 `history_perf_restore_and_paging_evidence --ignored`，1M 行夹具，PID 66907，自 23:20 起、
报告时已 28 分钟）。两个 cargo 进程并存违反「cargo 命令串行」的操作纪律，也是这些探针超时最可能的环境因素。
**结论：peri-resources 库测试在干净环境下是否 100% 全绿，本报告不能证明**（未取得无干扰的整包绿）。

## 2. 逐项核对（PASS / FAIL / PARTIAL）

| # | 核对项 | 结论 | 证据 |
| --- | --- | --- | --- |
| 1 | 存储 rewind 入口如实回报是否生效；ACP handler 在未生效时失败关闭（不发布、不推进结构版本） | **PASS** | §2.1 |
| 2 | 远端 rewind 批内守卫（不删「没见过」的行）+ 只读存储先拒绝写再早返回 | **PASS** | §2.2 |
| 3 | `RewindCompleted` 载荷 = 已发布的有效视图，且有测试观测 | **PASS** | §2.3 |
| 4 | 分页读取两阶段：先长度、后只取本页正文；多读的候选正文永不取；超预算单条独占一页且游标前进 | **PASS** | §2.4 |
| 5 | `position` 同一快照内可比（文档化为不透明）；协商后的初始回放带同一 entry 身份；未协商连接不出现 Peri 私有字段 | **PASS（带一处口径说明）** | §2.5 |
| 6 | 从未协商分页的连接不能用分页 | **PASS** | §2.6 |
| 7 | 新测试存在、断言行为（非实现细节）、回退实现即失败——逐条对应 | **PASS（F5 分支无测试：PARTIAL）** | §2.7 |
| 8 | 无 DDL：diff 里 0 条 `ALTER/CREATE TABLE/CREATE INDEX`，无 schema 文件被改 | **PASS** | §2.8 |
| 9 | 既有不变量无回归：rewind 后游标失效仍生效；追加与后续 Full 仍不失效 | **PASS** | §2.9 |

### 2.1 项 1：rewind「是否生效」的如实回报与失败关闭

- 契约：`peri-acp-types/src/session_resources.rs:284` `pub enum RewindOutcome { Applied, BoundaryMissing }`；
  文档与签名 `:921-930`（「未知边界保持无变更语义，但必须如实回报」）。
- 端口：`peri-resources/src/sessions/data.rs:354-360`。
- 本机实现：`peri-resources/src/sessions/sqlite_store/session_data/history.rs:286-339`——
  `:292` 先 `writable()`，`:301` 同事务内查边界 rowid，`:312-334` 有边界则删除+清零+刷新（`Applied`），
  无边界则 `BoundaryMissing`；`:335` 单次 `tx.commit()`。删除与清零同一事务，不存在「删了边界没恢复原文」。
- 远端实现：`peri-resources/src/sessions/remote/session_history.rs:321-390`（详见 §2.2）。
- 门面透传：`peri-resources/src/sessions/resources.rs:686-691`。
- 效力映射：`peri-acp/src/session/command/rewind.rs:346-351` `durability_of`（一一对应，不折叠）；
  `:279-285` 把 `BoundaryMissing` 按失败返回（`rewind_failed`，不发事件、不改内存）。
- **失败关闭（RPC 出口）**：`peri-acp/src/dispatch/rewind.rs:342-348`——`durability == BoundaryMissing` 时返回
  `-32013` + `data.reason = "rewind_boundary_missing"`，**在返回之前没有任何发布动作**。
- **失败关闭（热态出口）**：`peri-acp/src/host/requests/rewind.rs:322-333`——发布热态与
  `state.structure_version += 1` 都在 `dispatch::rewind_execute(...).await?`（`:321`）成功之后；
  `:327-328` 注释明确「本函数是 RPC rewind 的唯一成功出口，只能递增一次」。
- 唯一另一处生产调用者 `peri-agent/src/session/transcript/persistence.rs:327-333` 走
  `KeepThrough` 且**有意**丢弃 outcome（`.map(|_| ())`），`:318-326` 写明了与本 RPC 路径的区别理由
  （transcript 自身截断没有需要重建的有效视图）。⇒ 例外已文档化，不是遗漏。

### 2.2 项 2：远端批内守卫与只读先拒绝

- 顺序：`peri-resources/src/sessions/remote/session_history.rs:327` `self.ensure_writable().await?;`
  **先于** `:329` 的边界读取 ⇒ 只读打开时，「边界恰好不存在」不会以成功收场（与本机 `:292` 同序）。
- 批内守卫在删除之前：`:348-356` 推入 `GUARD_BOUNDARY_ROWID_SQL`（`:70-72`），`:357-360` 才是删除语句；
  每个清零 id 另有 `:361-371` 的 guard + `CLEAR_EXCLUDED_SQL`。`:372` 刷新派生、`:383-386` 单次
  `commit_effects`（成功 ⇒ `Applied`，失败 ⇒ `boundary_race_error` 重读判定）。
- 守卫为何能「中止整批」：目标表是单行表 `peri_store_meta`（`peri-resources/src/sessions/remote/schema.rs:75-81`
  `singleton INTEGER PRIMARY KEY CHECK (singleton = 0)`）；边界行缺失时 `INSERT ... SELECT 0 WHERE NOT EXISTS(...)`
  触发主键冲突 ⇒ 整批回滚。失败后 `:394-403` `boundary_race_error` 重读判定，归 `Conflict`。
- 测试：`peri-resources/src/sessions/remote/session_history_rewind_test.rs`
  - `:208` 未知边界 → `BoundaryMissing`，行数与 flags 一字未动；
  - `:253` 并发删边界行并**复用同一 rowid** 插入新行 → `Conflict`、新行仍在（`:296-300`）、清零未落地（`:306-310`）、
    rowid 复用事实被断言（`:311-315`）；
  - `:323` 只读 + 未知边界 → `ReadOnlyStore`（`:335-338`），并断言只读路径什么都不写（`:339`）。

### 2.3 项 3：事件载荷 = 发布的有效视图

- 计划只有一份重建结果：`peri-acp/src/host/requests/rewind.rs:228-238` `RewindPlan::effective_payloads`。
- 三处共用同一份 `effective`（`:288`）：事件载荷经 `DurableRewindCommit::Boundary.effective_history`（`:315-319`）
  → `peri-acp/src/session/command/rewind.rs:314-329` `emit_rewind_completed`；热态发布
  `publish_rewind_view`（`host/requests/rewind.rs:104-107`）；wire `history` 取 `state.history`（`:330-332`）。
- 观测测试：`peri-acp/src/host/requests_rewind_cases_test.rs:827-906`
  `test_rewind_completed_event_carries_the_effective_view`——断言恰好一条 `rewind_completed`（`:882`）、
  事件 `messages_json` 解出的 id 序列 == `[h2]`（`:890-894`，仍被存活摘要覆盖的 `h1/a1` 不得出现）、
  且 `payload_ids == [h2]`（`:896`）与响应 `history == [h2]`（`:897-905`）三处同源。

### 2.4 项 4：两阶段分页

- 契约把两阶段做成类型：`peri-acp-types/src/history_page.rs:403-414` `HistoryPageCandidate`（身份 + 存储序键 +
  **编码长度**，不含正文）；`:432-500` `HistoryPagePlan::plan`（只看长度定页）；`:507-512` `body_ids()`
  ——「阶段二**唯一**允许取正文的条目集合；多读的那一条候选行不在其中」；`:514-552` `materialize`
  校验行集合与顺序（`:520-531`），`:544-550` 反转成正序并给出 `next_cursor`。
- 超预算规则：`:481-488`（已有条目在手的行超预算即收尾，留给下一页）；`:539-541` `oversized` 只可能是本页第一条；
  `:546-550` 只要 `has_older` 就给出游标（前进）。
- 本机 adapter：`peri-resources/src/sessions/sqlite_store/session_data/history.rs:159-165` 阶段一
  `HISTORY_PAGE_ROWS_SQL`（`:29` 只选 `message_id, rowid, length(CAST(content AS BLOB))`）→ `:172` plan →
  `:174` 阶段二 `read_page_bodies(&plan.body_ids())`（`:185-202`，逐 id 一条 `HISTORY_PAGE_BODY_SQL`），
  两阶段在**同一个只读事务**内（`:142-176`）。
- 远端 adapter：`peri-resources/src/sessions/remote/session_read.rs:217-234`——`:218` `body_ids = plan.body_ids()`、
  `:219-223` 空集合直接跳过、`:224-233` 只为本页 id 拼语句、`:234` 解码并与 id 集合核对。
- 测试：
  - `peri-acp-types/src/history_page_test.rs:184` `the_second_phase_is_asked_for_the_page_rows_only`
    （`body_ids()` 不含多读的候选行）+ `:226` `the_second_phase_must_return_exactly_the_planned_rows`
    （少一行/顺序不符 → `Corrupt`）+ `:311` `an_oversized_first_row_occupies_a_page_alone_and_still_advances`；
  - 本机 `peri-resources/src/sessions/sqlite_store/session_history_page_test.rs:189`
    `the_candidate_row_beyond_the_page_never_leaves_storage`——把多读的那一行写成**不可解码正文**：
    任何「先取 limit+1 条正文再裁页」的实现都会在阶段二以损坏失败；`:227-237` 再断言「真去读它就必须失败」，
    锁住该证据的前提；
  - 同文件 `:241` `a_payload_over_the_budget_occupies_its_own_page_and_the_cursor_advances`——
    1 MiB 单条在 64 KiB 预算下独占一页（`:269-270`）、游标仍给出（`:271-274`）、下一页继续读到更早内容（`:276-281`）；
  - 远端 `peri-resources/src/sessions/remote/session_history_page_test.rs:167` 同一夹具两端一致。

### 2.5 项 5：`position` 可比性与 entry 身份；未协商不出现私有字段

- 文档化为不透明：`peri-acp-types/src/history_page.rs:27-36`（属性物排序键 `rowid` 原样给出、客户端**不得**解释、
  子项位置是另一把尺子、继承区条目没有 `position`）；ACL 出口的字段文档 `peri-acp/src/host/history_page.rs:316-324`。
- 初始回放与页用**同一份** entry 身份：`peri-acp/src/dispatch/session_replay.rs:52` `entry_id` 是唯一格式，
  `:149-166` `write_entry_marks` 是唯一拼装点；页侧 `peri-acp/src/host/history_page.rs:335`
  `"entryId": dispatch::entry_id(thread_id, item.id)`。
- 未协商不注入：`peri-acp/src/dispatch/session_replay.rs:154-160`（`write_entry_marks` 在
  `!marks.merge()` 时直接返回，一个字段都不写；`merge` 由 `:108` `positions.is_some()` 决定），
  门在 `peri-acp/src/host/requests/session_restore.rs:536-549`（未协商分页就不读位置表，
  用 `without_entry_marks`）。测试
  `peri-acp/src/dispatch/session_replay_test.rs:92-110`（`without_entry_marks` ⇒ `periEntryId`/`periPosition`
  均不存在）与 `:125-129`（`PeriCaps::default()` ⇒ `_meta` 为 `null`）。
- 端到端：`peri-acp/src/host/history_page_wire_test.rs:643-776`
  `a_negotiated_initial_load_marks_the_first_screen_with_page_comparable_identities`——首屏逐条带
  `periEntryId`/`periPosition`/`periLoadGeneration`（`:693-710`），页给出的身份集合 == 归档段身份集合（`:757`），
  页位置严格小于首屏最低位置（`:764-769`），首屏 + 页 == 全部自有条目（`:770-774`）。
- **口径说明**：本项「不出现 Peri 私有字段」在回放层按 `caps.replay` 门控——未协商分页但有 replay 能力的连接
  仍会看到 `periReplay` / `periLoadGeneration`（`:86-87`、`:105-106`；分页私有字段 `periEntryId`/`periPosition`
  不出现）。这与 REQ §5.3 一致（replay 是既有独立能力），但对未协商连接**成功响应体不含
  `nextCursor`/`snapshotToken`** 这一点，只由构造保证（见 §3 的缺口 A）。

### 2.6 项 6：未协商的连接不能用分页

- 分页准入只看**当前连接**：`peri-acp/src/host/connection.rs:60-78` `ConnectionContext::history_pagination`
  （`initialized && negotiated_caps.history_pagination`，失败关闭），文档明说不得改用 `caps()` 或会话/进程级注册表。
- 分流在生命周期路由**之前**：`peri-acp/src/host/server_loop.rs:383-401`——`HistoryPage` 分支直接
  `handle_page` 后 `return`（不取执行生命周期锁、不装配、不广播）；`Err` 也直接回，不降级成初始 load。
- 请求解析：`peri-acp/src/host/history_page.rs:124-177`——未协商却带 `cursor` ⇒ `-32601`
  + `data.reason = "pagination_not_negotiated"`（`:137-144`）；空/非字符串/损坏 cursor、`limit` 单用、
  历史页带 setup 参数或 `loadGeneration` 一律拒绝。
- 初始游标只在协商时附加：`peri-acp/src/host/server_loop.rs:394-399` 把
  `connection_facts.history_pagination` 带到 `:499-512`，未协商则 `attach_initial_cursor` 根本不被调用。
- 测试：`peri-acp/src/host/connection_test.rs:61` `history_pagination_stays_closed_until_this_connection_negotiates_it`
  （`:63-68`：`caps()` 兜底为 true，但 `history_pagination()` 为 false——正是失败关闭的判据）；
  `peri-acp/src/host/history_page_test.rs:71`（`classify(params, false)` ⇒ 拒绝）；
  `peri-acp/src/host/history_page_wire_test.rs:226-260`（协商了 `PeriCaps::default()` 的连接带 cursor ⇒
  `-32601`，且断言热态条数不变——「拒绝不得触发一次初始 load」）。

### 2.7 项 7：测试与发现的对应关系（含「回退即失败」的判据）

| 发现 | 覆盖测试 | 回退实现后为何会失败 |
| --- | --- | --- |
| F1 未知边界不得发布热态 | `peri-acp/src/host/requests_rewind_cases_test.rs:722-818` | 断言 `-32013` + `reason`（`:774-783`）、热态 `history`/`history_payloads` 不变（`:785-786`）、`structure_version == 0`（`:787-791`）、无 `rewind_completed`（`:792-796`）、存储 flags 未清零（`:805-818`）。旧实现「存储返回成功 ⇒ 发布已截断热态」，`history_ids` 与事件断言立刻失败 |
| F1' 存储层如实回报 | `peri-resources/src/sessions/remote/session_history_rewind_test.rs:208-247` | 断言 `outcome == BoundaryMissing`（`:236-240`）——签名从 `Result<()>` 改为 `Result<RewindOutcome>`，回退即编译失败；语义上旧实现无返回值可断言 |
| F2 批内守卫 | `remote/session_history_rewind_test.rs:253-316` | 无守卫时删除下界会命中复用 rowid 的新行（`:296-300` 失败）、返回 `Ok` 而非 `Conflict`（`:292-295` 失败） |
| F3 只读先拒绝 | `remote/session_history_rewind_test.rs:323-340` | 把 `ensure_writable` 放到边界读取之后，`:335-338` 会收到 `Ok(BoundaryMissing)` 而非 `ReadOnlyStore` |
| F4 事件载荷 = 有效视图 | `requests_rewind_cases_test.rs:827-906` | 载荷退回 canonical 前缀时 `h1/a1` 会出现在事件里，`:890-894` 失败 |
| 结构版本推进 + 旧游标失效（A19） | `requests_rewind_cases_test.rs:486-613` | 断言旧游标在**进存储之前**以 `-32012` 失效（`:587-590`）、`structure_version == 1`（`:593-594`）、新游标照常可读（`:595-612`）；不递增则 `:590` 失败 |
| 分页两阶段 / 候选正文不外取 | `peri-acp-types/src/history_page_test.rs:184`、`sqlite_store/session_history_page_test.rs:189` | 先取 `limit+1` 条正文的实现会读到那条不可解码行，在阶段二以 `Corrupt` 失败（`:221` 与 `:228-237`） |
| 超预算单条独占一页且游标前进 | `sqlite_store/session_history_page_test.rs:241-282`、`peri-acp-types/src/history_page_test.rs:311` | 不处理超预算时会返回空页或吞掉该条，`:268-274` 失败 |
| 逐页等价 + 追加/Full 不使游标失效（A08/A09） | `peri-acp/src/host/history_page_wire_test.rs:427`、`:518` | 按实时 flags 重筛成员或按新历史重排时 `:561-581` 失败 |
| 回放失败不得给成功游标（A22 服务端半边） | `peri-acp/src/host/history_page_wire_test.rs:592-632` | 失败仍返回 cursor 时 `:615-619` 拿不到 `Err` |
| 未协商拒绝分页 | `host/history_page_wire_test.rs:226`、`host/history_page_test.rs:71`、`host/connection_test.rs:61` | 放行兜底能力即 `:245-257` 失败 |
| 继承区不参与清零、不当回退目标 | `requests_rewind_cases_test.rs:1546` `test_rewind_with_an_inherited_region_only_clears_own_archived_prefix` | 见下 |
| 远端未知边界无变更 | `remote/session_history_rewind_test.rs:208` | 同上 |

关于 `requests_rewind_cases_test.rs:1546`：该用例给 child 会话造非空继承区，断言存活摘要仍在时排除段不得清零、
继承条目不得成为回退目标、且事件 = 热态 = wire（R1 报告的口径）。若清零依据误用**完整上下文**，继承区首条会
终结领导段，`own[0]/own[1]` 会被复活，断言即失败。

**唯一 PARTIAL**：F5 的两条分支里，「只缺一半（有 store 无 thread_id 或反之）必须硬失败」只有生产实现
（`peri-acp/src/session/command/rewind.rs:303-311`），**没有任何测试命中该分支**；
`(None, None)`（文档化的无持久化后端）由 `rewind_test.rs` 的 `make_ctx`（`:117-139` 构造的
`CommandContext` 两字段皆缺）间接覆盖（如 `:807-842` 断言成功路径仍发 `RewindCompleted`）。

### 2.8 项 8：无 DDL

- `git diff HEAD | grep -cE "^\+.*(ALTER TABLE|CREATE TABLE|CREATE INDEX|DROP TABLE|CREATE UNIQUE INDEX)"` → **0**。
- 9 个未跟踪 `.rs` 文件逐个 grep 同样 **0**（`history_page*.rs`、两端 `session_history_page_test.rs`、
  `session_history_rewind_test.rs`、`session_history_perf_test.rs`）。
- schema 文件均未修改：`sqlite_store/schema.rs`、`sqlite_store/schema_cleanup.rs`、`remote/schema.rs`、
  `sqlite_store/canonical.rs` 都不在 `git status` 里。唯一点名的 `row_mapping.rs` 只改 SELECT 投影
  （`THREAD_META_COLUMNS` 把 `SUM(LENGTH(...))` 子查询换成常量 `0 as content_size`），**不是 DDL、不新增列**；
  代价是该字段现在恒为 0（见 §3 缺口 C）。

### 2.9 项 9：既有不变量无回归

- **rewind 后游标失效**：`peri-acp/src/host/requests_rewind_cases_test.rs:486-613`（含「恰好 +1」与
  「新游标可用」两面）；纯契约层 `peri-acp/src/host/history_page_test.rs:112-119`
  （版本不符 ⇒ `Expired`，且 `verify_structure_version` 在存储委派之前调用：`host/history_page.rs:201-204`）。
- **追加与后续 Full 不失效**：`peri-acp/src/host/history_page_wire_test.rs:518-583`——追加 2 条 + 再 Full 1 次后，
  同一 cursor 的 `items` 与 `nextCursor` 逐字节等价，且**在途的** `nextCursor` 仍可用；
  存储层 `sqlite_store/session_history_page_test.rs:285-297`（追加后 `range_head`/锚点未变故仍有效）。
- 本轮 R1 的差异不触及这两条规则的定义处：唯一规则 `peri-acp-types/src/store/history.rs:201`
  `rewind_clear_excluded_ids` 与 `:154` `rewind_keep_len` 仍是唯一定义，调用点只有
  `peri-acp/src/host/requests/rewind.rs:197,214`（R4 报告 I2/I3 的结论在本树上仍成立）。

## 3. 明确未验证 / 已知缺口（不暗示覆盖）

- **A：未协商连接的成功响应体没有 wire 级断言**。`nextCursor`/`snapshotToken` 缺席只由
  `server_loop.rs:394-399, 499-512` 的构造与 `classify` 单测保证；没有一条测试直接断言
  「`PeriCaps::default()` 连接的初始 load 响应不含这两个字段」。
- **B：前端（TUI / `npm-packages/@peri-sdk`）行为完全不在本工作流范围**：滚动加载、区间合并、锚点、
  迟到页拒绝、Compact 不再强制 reload 等（REQ-C01~C03、A11/A12/A20/A22 客户端半边、A23/A24 的展示侧）
  本轮**一行都没测**。本工作流按约束未触碰 `peri-tui/` 与 `npm-packages/`。
- **C：`content_size` 行为降级**。`row_mapping.rs` 把它变成常量 0（无生产消费者，只有定义与一个 TUI 测试夹具）；
  这是 A04 的代价，不是本轮的回归，但仍是可观察的行为变化。
- **D：F5 半配置分支无测试**（§2.7 的 PARTIAL）。
- **E：候选/预览仍把继承条目当可回退目标**（R1 已记录，本轮复查仍如此：
  `host/requests/rewind.rs:67-68` 读完整逻辑上下文 → `dispatch/rewind_candidates.rs:26-35` 只做 Human 过滤；
  执行侧 `host/requests/rewind.rs:191-193` 按 `own_start` 拒绝）。预览侧仍会返回一个执行必然失败的指纹。
- **F：性能数字（A04~A07）不在本次验证范围**。R3 的证据文件 `.recon/perf-evidence.md` 存在且是真实的；
  但它的 1M 行档在本次报告时**仍在运行**（PID 66907，自 23:20 起），我没有复跑、也没有核对原始日志。
- **G：标准未协商完整回放假路径**（无 `cursor` 的普通 load）本轮未做端到端验证；只核对了「分页字段不注入」
  的代码路径与单测（§2.5）。
- **H：`--workspace` 级别的验证未做**（按任务约束）；`peri-tui`/`npm-packages` 的编译与测试未测。
- **I：环境噪声与未取得的干净绿**：§1.1 的并发 cargo 进程使 `peri-resources` 整包两次都以失败收场（失败集合互不相交，
  逐条单跑均通过）。**没有取得一次无干扰的整包全绿**，因此不能声称「peri-resources 全绿」；能声称的是
  「两次整包的失败都不是 R1–R3 改动触达的代码路径，且逐条隔离复跑全部通过」。

## 4. 本次会话的改动

只有一个文件被写入：本报告 `.recon/R4b-verification.md`。生产代码、测试、文档索引、schema **零改动**
（§2.8 的 DDL 检查与 `git status` 的未变更确认）。
