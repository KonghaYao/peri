# S6d：S6a–S6c 验证报告（ACP 历史分页）

日期：2026-10-13
范围：只读核对 S6a/S6b/S6c 已落地的实现与测试；**未改任何生产代码**，本报告是本次唯一写入的文件。
基线：worktree `peri-transcript-history`，分支 `feat/transcript-shrink-history-pagination`，脏工作树 `git diff HEAD` 97 个文件（含 S1–S6 各切片，git 层面不可再分）。
契约事实源：`peri-acp-types/src/history_page.rs`（**不是** `.recon/S6-design.md`——该草稿已过期）。

---

## 1. 命令与实测结果

| # | 命令 | 观测结果行 | 退出码 |
| --- | --- | --- | --- |
| 1 | `check --locked -p peri-acp-types -p peri-resources -p peri-acp -p peri-agent --all-targets` | `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 39.15s`（输出 0 条 `warning`） | 0 |
| 2 | `test --locked -p peri-acp-types --lib` | `test result: ok. 565 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s` | 0 |
| 3 | `test --locked -p peri-resources --lib`（第 1 次） | `test result: FAILED. 480 passed; 1 failed; 21 ignored; 0 measured; 0 filtered out; finished in 56.30s` | 101 |
| 3 | 同上（第 2 次） | `test result: FAILED. 478 passed; 3 failed; 21 ignored; 0 measured; 0 filtered out; finished in 44.76s` | 101 |
| 3 | 同上（第 3 次） | `test result: FAILED. 480 passed; 1 failed; 21 ignored; 0 measured; 0 filtered out; finished in 45.28s` | 101 |
| 3a | 控制组：`test --locked -p peri-resources --lib -- --test-threads=1`（**全量串行**） | `test result: ok. 481 passed; 0 failed; 21 ignored; 0 measured; 0 filtered out; finished in 272.52s` | 0 |
| 3b | 控制组：`test --locked -p peri-resources --lib -- --test-threads=1 discovery::tests::test_worktree` | `test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 484 filtered out; finished in 20.93s` | 0 |
| 3c | 控制组：`test --locked -p peri-resources --lib -- --test-threads=1 incompatible_schema_is_rejected` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 501 filtered out; finished in 1.30s` | 0 |
| 4 | `test --locked -p peri-acp --lib` | `test result: ok. 854 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 170.03s` | 0 |
| 5 | `test --locked -p peri-agent --lib` | `test result: ok. 1089 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 20.43s` | 0 |
| 6 | `git diff HEAD \| grep -in "ALTER TABLE\|CREATE TABLE\|CREATE INDEX\|DROP TABLE"` | 无输出（无匹配） | 1 |

### 1.1 命令 3 的失败不是本切片的回归（证据）

三次全量并行运行都失败于**同一个**用例，且它单跑通过：

- `sessions::sqlite_store::connection::open_tests::incompatible_schema_is_rejected_without_wal_or_database_changes`
  → `panic at peri-resources/src/sessions/sqlite_store/connection_open_test.rs:377:66: called Result::unwrap() on an Err value: session database initialization is busy`
  （该行是 `lock_schema_open(&path, Duration::ZERO)` —— **零预算**探测，对负载天生敏感。）
- 第 2 次运行另多出 2 例 `sessions::discovery::tests::test_worktree_{ancient,legacy}_git*`
  → `workspace discovery failed: Git discovery timed out`（外部 `git` 子进程超时；控制组 3b 串行 18/18 通过）。
- **决定性控制组**：同一二进制**全量串行**（`--test-threads=1`，3a）**481/481 全绿**，而同一套用例在默认并行下 3/3 运行失败。即失败由并行资源争用触发，不是用例本身或实现的确定性缺陷。
- 归属判据：`git diff HEAD --name-only` 中**没有** `peri-resources/src/sessions/sqlite_store/connection.rs`、`connection_open_test.rs`、`discovery_test.rs`（`grep -i connection` 只命中 `peri-acp/src/host/connection*.rs`）。同一现象在上一轮切片验证中已记录：`.recon/S5-verification.md:74-77`「`session database initialization is busy`…该文件未被本切片改动…判为负载下的初始化锁 flake」。
- 本机同一时段另有 worktree（`peri-mcp-apps`）在编译，全程处于高负载。
- **诚实边界**：三次 `peri-resources --lib` **并行**全量都没拿到绿；绿只出现在串行。因此「负载下的 flake」这一判断依赖 3a 这个控制组，而不是「跑一次碰巧通过」。

---

## 2. 不变量核对

| # | 不变量 | 判定 | 证据（file:line） |
| --- | --- | --- | --- |
| 1 | `session/load` 仍是唯一入口；InitialLoad/HistoryPage 在**生命周期锁之前**决定 | **PASS** | `host/server_loop.rs:379-396`：`if method == "session/load"` → `classify` → `HistoryPage` 立即 `handle_page` 并 `return`；`Err` 立即回错并 `return`。`lifecycle_lock` 直到 `:397-460` 才创建。顶层路由 `:36-44` 也把 `session/load` 送进 `dispatch_request`（不经 `spawn_prompt` / `spawn_session_io` / mcp_apps）。`git diff HEAD \| grep '"session/'` 无新增方法名（只有既有 `session/rewind-*`） |
| 2 | 每个非法分页请求都被显式拒绝，且**不存在**回落成普通 load 的路径 | **PASS** | `host/history_page.rs:103-156` 是唯一解析点，`match params.get("cursor")` 只有两类出口：`None`（**没有 cursor 字段**）→ `InitialLoad`；`Some` 分支的每一条错误路径都 `return Err`：未协商能力 `-32601`(:117-123)、`cursor` 为 `null`/非字符串 `-32602`(:124-129)、空串 `-32602`(:130-135)、setup 参数 `-32602`(:136,346-356)、`loadGeneration` `-32602`(:137-144)、缺 `sessionId` `-32602`(:145,336-344)、游标解码失败(:146-147)、`limit` 非整数/越界 `-32602`(:148,358-375)。调用点 `server_loop.rs:389-392` 对 `Err` 回错后 `return`。`serde_json` 语义下 `"cursor": null` 是 `Some(Null)`，不会误判成「无字段」 |
| 3 | 历史页不触发任务绑定 / config·commands 广播 / MCP attach / continuation 调度 | **PASS** | `handle_page`（`host/history_page.rs:165-214`）只有三步：短锁读存在性·closing·结构版本(:172-184) → `dispatch::load_history_page`(:186-193) → 投影(:194-199)；无 `sessions` 写入、无 `prompt_locks`、无 `attach_session_workflow`。`server_loop.rs:386` 的 `return` 早于 `schedule_mailbox`(:497) 与 `session_setup`(:522)；`attach_initial_cursor`(:483) 由 `load_initial_cursor` 门控，历史页是 `false` |
| 4 | 分页返回归档（excluded）行、不按实时 `excluded` 重筛；不可展示行推进位置；`nextCursor = null` 只在真到头 | **PASS** | 两条分页 SQL 都没有 `excluded` 条件（`sqlite_store/session_data/history.rs:22-25`；remote 同形见 `remote/session_read.rs:181-207`）。`displayable_to_client`（`peri-acp-types/src/store/mod.rs:66-75`）只判 System / reminder 受众，不看 flags。装配规则 `peri-acp-types/src/history_page.rs:396-420`：`consumed`（位置）与 `items`（可见）分开推进；`:424-427` `exhausted = consumed == rows.len() && rows.len() <= limit` 才是 `None`。用例：`sqlite_store/session_history_page_test.rs:114-145`（先断言有效视图已不含该行，再断言页仍返回它）、`:148-177`（空页 + 前进游标，不假终点）、`peri-acp-types/src/history_page_test.rs:107` |
| 5 | 首游标来自最旧的**自有**有效 payload（不是首屏第一条、不是继承条目），无更早内容时为 `null` | **PASS** | `dispatch/session_load.rs:106-115`：锚点 = `load_active_snapshot().payloads.first()`——`ActiveSessionSnapshot` 把 `inherited` 与 `payloads` 分开给，自有区不含继承前缀；仅当 `bounds.range_head != Some(anchor)` 才铸游标。用例：`host/history_page_wire_test.rs:231-281`（普通新会话 `nextCursor == null`；归档后锚点 = 最旧有效条目、页与首屏不重叠、读到起点即收尾） |
| 6 | 能力由**当前连接**决定；未协商的连接既不得分页语义也不得游标 | **PARTIAL** | 成立部分：`server_loop.rs:380` 取 `connection.lock().await.caps()`；`ConnectionContext` 每连接一份（`host/mod.rs:381`），`commit_initialize` 只在成功 `initialize` 时落地且幂等（`host/connection.rs:44-55`、`server_loop.rs:509-513`），重连即新上下文。用例：`peri-acp-types/src/peri_caps_test.rs:19`（缺省/`"true"`/`false` → false）、`host/history_page_wire_test.rs:195-228`（明确 initialize 但未声明 peri 键的连接带 `cursor` → `-32601`，**且不触发一次 initial load**）。**缺口**：`host/connection.rs:57-71` 的 `caps()` 在**从未 initialize** 时回退 `PeriCaps::all_enabled()`（含 `history_pagination: true`），此类连接既会拿到 `nextCursor`/`snapshotToken` 也能走分页分支；判据与既有 `SessionManager::effective_host_caps` 一致（进程内 MPSC/TUI 本就不发 initialize），S6b 已披露、本片未改 |
| 7 | 回放失败使初始 load 失败，不留半加载会话态 | **PASS（代码级）/ 无用例** | `host/requests/session_restore.rs:534-552`：`replay_persisted_session_history(...).await.map_err(...)?` —— 失败直接以 `-32603` 冒泡，`configOptions`/modes/commands 广播（`:554-569`）与 `server_loop.rs` 的成功路径后处理都不可达。已装配的会话态本身是完整的（`:515` clone 的 `history_payloads` 就是权威有效集合），重试命中 `AlreadyLive`(`:140-149`) 后从同一份整体重放，不重复安装。**全仓没有让回放投递失败的用例**（`MockTransport` 不注入发送失败；`grep -rn "history replay failed"` 只命中实现两行），本条只有代码级证据 |
| 8 | rewind 后游标失效是真的 | **PASS** | 计数器：`host/mod.rs:156` `SessionState::structure_version`（内存，无存储列）。唯一写入点：`host/requests/rewind.rs:299`，位于 `DurableRewindCommit` 成功返回（`:283-293`）之后的成功分支，失败分支在此前已 `?` 返回；`grep -rn structure_version --include=*.rs peri-acp/src` 的**生产代码**写入点仅此一处。校验点：`host/history_page.rs:180-183`（短锁内、**先于**存储委派）与类型侧 `peri-acp-types/src/history_page.rs:246-254`。端到端证明：`host/requests_rewind_cases_test.rs:402-520` —— rewind 前旧游标可翻页 → rewind 后先证明锚点在冷恢复视图里**仍存活**（排除「锚点被删」这一解释）→ 旧游标得 `-32012` → `structure_version == 1`（一次成功只推一次）→ 新游标可用 |
| 8b | 追加消息与后续 Full **不**使游标失效 | **PASS（构造性）** | 版本侧：除 `rewind.rs:299` 无其他写入点（同上 grep）。范围侧：`resolve_browsing_range`（`history_page.rs:331-350`）只要求「起点未变 + 锚点仍在 + 版本相同」；追加只动尾部（`sqlite_store/session_history_page_test.rs:315-320` 直接断言「追加只动尾部，起点与身份不变」）；Full 只改 flags + 追加摘要，不改行集合与顺序，`displayable_to_client` 也不看 flags。**注意**：没有「追加/Full 之后旧游标仍可用」的端到端用例，只有上述构造性证据 + 该边界断言 |
| 9 | 无 DDL；schema 文件未被触碰 | **PASS** | 命令 6：`git diff HEAD` 中 `ALTER TABLE`/`CREATE TABLE`/`CREATE INDEX`/`DROP TABLE` 零匹配；`git diff HEAD --stat -- '*.sql' '**/schema*'` 只命中 `peri-resources/src/sessions/remote/schema_upgrade_test.rs \| 19 +`（测试夹具 `SqliteTransport::for_test`，非表结构） |
| 10 | 一次有界页读取：不全量物化、不 OFFSET、读 N 再裁；字节预算在解码前止步 | **PARTIAL** | 成立部分：两条 SQL 都是 keyset（`rowid < (锚点子查询)`）+ `LIMIT limit+1`，无 `OFFSET`、不扫全历史（`sqlite history.rs:22-25,111-117`；`remote/session_read.rs:181-207`）。字节预算**在解码之前**停下：`history_page.rs:401-406` 的 `let size = row.content.len(); if !items.is_empty() && size > remaining { break; }` 早于 `:407` 的 `decode_row`，用例 `history_page_test.rs:140`。**缺口**：两个后端都是先按**行数**取回 `limit+1` 条**完整正文**（SQLite `fetch_all` / remote `read_batch`），字节预算只在装配阶段生效。默认 `limit = 50` 时最坏先持有 51 条正文；在 A07 场景（连续 ≥1 MiB 的 payload、页目标 512 KiB）下 `assemble_history_page` 只产出 1 条 `oversized`，但上层已取回约 51 MiB。REQ-H03「禁止先批量物化 100 条大正文，再裁成符合响应预算的一页」与 A07「只持有本页加至多一个候选正文的量级」在此**不成立**。另注：预算检查在 `items.is_empty()` 时不生效，页面若由不可展示行开头会逐条解码（每次只持有一条），峰值仍是「本页 + 1 条候选」，不改变上面的结论 |
| 11 | `position` 是可比较的逻辑展示位置（REQ §5.2） | **PARTIAL** | `entryId = "<threadId>:<uuid>"`（`host/history_page.rs:312`）跨页稳定，可去重；但 `position` 是**页内**序号（`:194-199` 的 `enumerate`），不是全局可比较的逻辑位置，跨页顺序只能由 `nextCursor` 链推断。S6b 已披露并写进 `docs/code-index/peri-acp.md:179`。前端 A20/A24 的阅读锚点因此只能依赖链顺序与 `entryId`，不能依赖 `position` |

---

## 3. 可诚实主张的验收项

| 验收 | 判定 | 依据 | 未覆盖的余量 |
| --- | --- | --- | --- |
| **A15** 普通新会话尚无 Full：初始 load 正常；没有更早范围时 `nextCursor = null`，不触发迁移或自动摘要 | **PASS（服务端）** | `host/history_page_wire_test.rs:231-281`（新会话 `nextCursor == null` 且 `snapshotToken` 仍给出；归档后才有游标）；空会话 `range_head = None` → `HistoryPageBounds::new(None)`（`history_page.rs:293-296`）；`sqlite_store/session_history_page_test.rs:299-341`（空/单条/有内容三形状 + 会话不存在 `NotFound`）。初始游标只做有界边界读取，不写任何状态 | 没有断言「不触发自动摘要」的负向用例（该行为本片未引入，属既有路径） |
| **A21** 长 turn 执行中翻页：分页成功且不获取执行生命周期锁；MCP、绑定、队列和执行状态不变；不持有全局 sessions 锁跨 I/O | **PASS（服务端）** | 结构：`server_loop.rs:379-396` 在 `lifecycle_lock` 之前 return；`handle_page` 只在短作用域内 `sessions.lock()`（`host/history_page.rs:172-184`），存储 I/O 与投影都在锁外（`:185-213`），且走 `controller.sessions()`（`dispatch/session_load.rs:64-72`），不碰宿主 `SharedSessions`。运行时证据：`host/history_page_wire_test.rs:143-193`（真实 MPSC 循环 + 会话已持有 prompt 锁时翻页仍然成功）。不绑定/不 attach/不广播/不续跑见不变量 3 | 「MCP、绑定、队列和执行状态不变」没有运行时断言（只有调用图证明）；没有「长 turn + 并发加载另一会话」的端到端压测 |
| **A22** 第 N 条初始回放失败/取消，同会话两次 load 交错 | **PARTIAL（服务端一半）** | 回放先于响应、失败即 load 失败：`session_restore.rs:534-552`；失败时**不会**得到成功 cursor（`server_loop.rs:483` 只在 `result.is_ok()` 时 `attach_initial_cursor`）；代次标记 `_meta.periLoadGeneration` 由 `caps.replay` 统一门控（`dispatch/session_replay.rs:306-322`），客户端据此把通知与响应配对 | **未测**：回放投递失败（无用例）；客户端暂存/丢弃、两次 load 交错、迟到事件不污染新代次——全在前端（S7）。「旧画面不冒充新会话就绪」是客户端契约 |
| **A25** 未协商/版本不匹配、重连能力变化、非法 cursor·limit、跨会话 cursor、分页携带 setup 参数：明确拒绝、不降级为初始 load | **PASS（服务端，带 3 处保留）** | `host/history_page_test.rs` 10 例覆盖：模式分流 + limit 边界、无 cursor 时带 limit 拒绝、未协商拒绝、坏 cursor 各形态、结构版本过期、`loadGeneration` 形状、缺 sessionId、跨会话、limit 非整数/越界、setup 参数。wire 级：`history_page_wire_test.rs:195-228`（未协商 → `-32601`，**且不触发一次初始 load**）。错误码：跨会话 `-32011`、过期 `-32012`（`dispatch/session_load.rs:122-147`）。**保留**：①`-32011`/`-32012` 是 Peri 自定码（非 ACP 标准），客户端须按 `data.reason` 分支；②「重连能力变化」只有构造性证据（每连接一份 caps），无端到端用例；③完全跳过 `initialize` 的连接会拿到 `all_enabled`（不变量 6） | — |
| **A19 的 cursor 部分** | **PASS** | `host/requests_rewind_cases_test.rs:402-520`：rewind 前旧游标可翻页、rewind 后锚点仍存活但旧游标 `-32012`、版本恰好 `+1`、新游标可用 | A19 的其余部分（两次 Full、三种 rewind 目标、重启后有效输入一致）属 S4/S5 面，由 `requests_rewind_cases_test.rs:305+`（热态 vs 冷恢复在模型边界逐条一致）等既有用例覆盖，**非本片新增**，我未重跑单独用例 |

---

## 4. 明确**没有**验证的部分

1. **前端行为全部未测**：A12 / A20 / A23 / A24（区间合并、阅读锚点不跳动、多 block 不互相覆盖、旧 TodoWrite 不影响当前 Todo）以及 REQ-C01~C03 的 TUI/SDK/Yjs 实现——本片没有读也没有跑 `peri-tui` / SDK 的任何测试，只跑了四个 crate 的 check/test。
2. **性能验收 A05–A07 没有任何实测数字**：没有查询计划、扫描行数、字节数、p50/p95、峰值内存。A07 的内存上界按不变量 10 的缺口**不成立**。
3. **回放失败（不变量 7）没有自动化用例**；「无更早历史」的初始 load 不触发迁移/摘要也没有负向用例。
4. **追加/再次 Full 之后旧游标仍可用**（REQ-H02、A09）只有构造性证据，没有端到端用例。
5. **相同 cursor / 相同 limit 重试等价**（A08）没有专门用例；「页成员不依赖实时 flags」由 `sqlite_store/session_history_page_test.rs:114` 间接覆盖。
6. **真实远端**未验证：`remote/session_history_page_test.rs` 复用 SQLite 夹具与 `SqliteTransport`（假传输），不是真远端服务；A13 的双后端比对只在同一夹具上成立。
7. **A01–A04、A10、A11、A14、A16–A18** 未核对（属 S1–S5 面，不在本片任务书范围）。
8. **命令 1 的范围**：只 check 了 4 个 crate 的 `--all-targets`；未跑 `--workspace`，未跑 TUI/agent 集成测试、E2E、doc tests。
9. **`peri-resources --lib` 在并行下没有全量绿**（命令 3，三次全量并行均失败于既有 flake），仅在**串行**下拿到 481/481 绿（命令 3a）。失败用例的实现与测试文件均未被本工作树改动。
10. **切片归属不可分**：`git diff HEAD` 是 97 个文件的混合脏树，S6a/S6b/S6c 的改动无法在 git 层面区分；本报告核对的是全量现状。
11. **代码索引与设计文档只做了「是否同步」的目视核对**（`docs/code-index/peri-acp.md:63,157,159,179`、`peri-acp-types.md`、`peri-resources.md`、`docs/design/peri-acp-protocol.md` §2.1、`peri-acp/CLAUDE.md`），没有跑文档站构建或 `DOC-UPDATE-001` 的自动化检查。

---

## 5. 结论

1. S6a–S6c 的 ACP 侧契约、分流、投影与失效语义与需求/验收一致，11 条不变量里 **8 条 PASS、3 条 PARTIAL**（能力回退 `all_enabled`、字节预算不约束存储读取、`position` 为页内序号）。
2. 最需要跟进的两条：**REQ-H03 的有界读取**（两个后端都先取回 `limit+1` 条完整正文，A07 的内存条款不成立）与**未 initialize 连接的 `all_enabled` 回退**（未协商连接可能拿到游标）。
3. 命令 1/2/4/5 全绿；命令 3 的失败是既有、可复现于负载下的 flake——同一二进制串行 481/481 全绿（命令 3a），不是本切片的实现回归。
4. 无 DDL、无 schema 改动、无新增依赖。
