# S9b — ACP 接线：未协商连接在 `session/load` 上流式回放 canonical 完整历史

本文件只记录本切片（S9b）的落地与证据。设计来源：`.recon/workflows/s9-standard-replay.mjs` 的 `PROMPT_S9B`、
需求 `spec/issues/2026-10-13-transcript-shrink-history-pagination.md`（REQ §5.3、REQ-H01~H04）。
存储契约由 S9a 提供（`.recon/S9a-storage-contract.md`）。**本次全程未执行任何 git 写操作**（无 commit/stash/checkout/reset/clean）。

## 1. 范围

1. **做**：把 `session/load` 的**未协商 `peri.historyPagination`** 连接从「回放热态 `history_payloads`」改为
   「经 `Controller::sessions()` 向前段读取 canonical 完整历史 → 逐段投影成标准 `session/update`」的流式投递；
   含 `Unsupported` 的显式回退与失败关闭。
2. **不做**：不改协商路径语义、不改既有 wire 字段名与错误码、不新增依赖、不做 DDL、不改 S9a 契约、
   不动 `peri-acp-types` / `peri-resources` 的任何文件。
3. **工作树背景**：分支 `feat/transcript-shrink-history-pagination` 内前序切片（S1–S9a）与 S9b 均**未提交**；
   下文「改动文件清单」只列本切片改动（同文件内的前序改动不计入）。

## 2. 改动文件清单

前缀省略：`=/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history/`

1. `/peri-acp/src/dispatch/session_replay.rs` — 模块 doc 增加「两条交付来源，判据是连接级事实」表；新增
   `REPLAY_SEGMENT_ROWS = 50` / `REPLAY_SEGMENT_BYTE_BUDGET = 512 * 1024`（注释写明与 `host::history_page`
   默认值同量级、各自持有、不出现在 wire 上）；新增 `pub async fn replay_canonical_history_streamed(controller, session_id, sender, caps, generation)`
   与私有 `replay_canonical_history_fallback(...)`；`ReplayError` 新增 `HistoryReadFailed(String)`。
2. `/peri-acp/src/dispatch/mod.rs` — `pub use session_replay::{..., replay_canonical_history_streamed, ...}`。
3. `/peri-acp/src/host/requests/session_restore.rs` — `handle_load`：未协商分支改调流式入口（协商分支保持
   「热态有效集合 + `ReplayMarks`」）；删除未协商分支对 `state.history_payloads` 的依赖；`handle_load` 文档注释
   改写为分流契约；`-32603` 失败映射沿用既有代码（前序切片落地，语义未变）。
4. `/peri-acp/src/host/requests_test.rs` — 注册 `#[path = "requests_unnegotiated_replay_test.rs"] mod unnegotiated_replay_tests;`。
5. `/peri-acp/src/host/requests_unnegotiated_replay_test.rs`（**新文件**，939 行）— 12 条用例，见 §4.2。替身为
   `ReplaySpy` 门面（宏逐项转发真实 `SqliteThreadStore` 门面，只替换 `load_history_segment` / `load_session_history`
   并按 `SpyMode{Forward,Unsupported,Corrupt,AnchorVanishes,ScriptedFirstSegment}` 造场景；记录每次段读取的
   `cursor/limit/byte_budget/payloads/error` 与全量读取次数）。
6. `/docs/code-index/peri-acp.md:159`（「历史读取与投影」行，**最小补充**）— 补两个回放入口、两个新常量、
   `Unsupported` 回退与「未协商连接的回放源是存储的 canonical 完整历史，不是热态 `history_payloads`」。
7. `/.recon/S9b-acp-streaming-replay.md` — 本文件。

## 3. 契约与不变量

1. **分流判据是连接级事实**：`handle_load` 收 `history_pagination: bool`（来自 `host/requests.rs:127` 的
   `connection.history_pagination`），不读进程/会话级 caps 注册表（静态判断：本切片未新增任何 caps 回读）。
2. **协商（`true`）行为不变**：热态**有效集合**回放 + `periEntryId`/`periPosition` + 响应 `_meta.periLoadGeneration`
   + 首游标。本切片对该分支只做「把原来的单一路径挪进 `if`」，未改其实现或 wire 形状。
3. **未协商（`false`）**：
   1. 回放源 = 存储的 **canonical 完整历史**（含 Full Compact 归档原文）；循环 `load_history_segment(id, cursor, 50, 512KiB)`，
      内存只留当前一段（不先取全量再切段）；`next_cursor = None` 才是「没有更多」。
   2. wire 上**没有** `periEntryId` / `periPosition` / `nextCursor` / `snapshotToken`：未协商连接构造
      `ReplayMarks::without_entry_marks(...)`，分页私有字段在类型上就构造不出来。
   3. 响应体形状与改动前一致（`identity_response` + modes + configOptions；`_meta` 门控见 §5.1）。
4. **有界**：`REPLAY_SEGMENT_ROWS` / `REPLAY_SEGMENT_BYTE_BUDGET` 是回放自己的命名常量，与 `host::history_page`
   的 `DEFAULT_PAGE_LIMIT` / `PAGE_BYTE_BUDGET` **同量级且当前同取值**，两者不互相引用（详见常量 doc）。
5. **Unsupported 与失败**：
   1. `Unsupported` 且**尚未送出任何通知** → `tracing::warn!("... falling back to the full canonical read")` +
      显式回退 `load_session_history` 全量读取，复用同一份投影（wire 形状一致）。
   2. `Unsupported` 出现在**已交付之后**、以及其他错误（`Corrupt`/`Unavailable`/锚点消失/读失败）→ 本次 load
      失败（`-32603` + `tracing::error!`），**不**产生「半份首屏 + 成功响应」。空段不会被当成回放结束。
6. **`AlreadyLive`**：未协商连接同样从存储流式读取（交付来源由连接事实决定，不由热态是否恰好有快照决定）。
   注意 `prepare_existing` 的 `AlreadyLive` 分支本身只装有效集合进内存，完整历史按需从存储取。
7. **`tail` 上界语义**：本次交付 = load 开始时刻的持久前缀。已完成的 turn 不会丢：写屏障先于热态发布
   （`host::prompt::finish_prompt_turn` 只在 `PromptResult` 建好之后采用 `persisted_payloads`，该结果在屏障完成之后产生，
   失败/取消同理）⇒ 热态永不领先于 disk prefix，进入过热态的内容必然在 `tail` 内。进行中的 turn 已流式发出的正文
   既不在 `tail` 内也不在热态有效集合内——两条来源一致，本切片不改变（结论已写入函数 doc comment）。
8. **继承区不分页**：首段一次性返回全部继承区 payload（S9a 段契约），本切片不切分它（已知边界，已写进 doc comment）。
9. **ACP 不直操存储**：所有存储访问经 `controller.sessions()`（`load_history_segment` / `load_session_history`）。
10. **硬约束遵守**：未新增依赖、未做 DDL、未改 wire 字段名与错误码、未改协商路径语义、未改 S9a 契约、
    未改动 `peri-acp-types` / `peri-resources`。

## 4. 实测证据

### 4.1 必跑命令（冻结树上最后一次执行，均为实测）

1. `cd <worktree> && ./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp`
   → `Checking peri-acp v0.2.0 ...` / `Finished dev profile [unoptimized + debuginfo] target(s) in 15.08s`（无 error/warning 行）。
2. `... test --locked -p peri-acp --lib -- unnegotiated_replay_tests`
   → `test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 863 filtered out; finished in 3.37s`
3. `... test --locked -p peri-acp --lib -- replay`
   → `test result: ok. 28 passed; 0 failed; 0 ignored; 0 measured; 847 filtered out; finished in 3.31s`
4. 全量 `... test --locked -p peri-acp --lib`（两次，见 §4.3）
   → 第 1 次 `874 passed; 1 failed`（1 条既存 flake）；第 2 次 `875 passed; 0 failed` / `finished in 267.01s`。
   （第 2 次全量之后只对 `session_replay.rs` 的 doc comment 做了两处措辞收紧；随后重跑第 1~3 条命令全绿。）

### 4.2 本切片用例（12 条，文件 `peri-acp/src/host/requests_unnegotiated_replay_test.rs`）

1. `streamed_canonical_replay_delivers_the_archived_bodies_in_canonical_order` — 归档原文**出现**，逐条正文与顺序
   等于 `load_session_history` 的 canonical 序列。
2. `streamed_canonical_replay_wire_carries_no_peri_private_fields_for_a_standard_client` — 回放通知上没有任何
   `peri*` 前缀字段，全量通知 + 响应不含 `nextCursor`/`snapshotToken`/`periEntryId`/`periPosition`。
3. `streamed_canonical_replay_keeps_the_generation_gated_by_the_replay_capability` — 见 §5.1（本切片**记录并接受**
   既有门控：`periLoadGeneration` 只随 `caps.replay` 出现，且与分页字段无关）。
4. `streamed_canonical_replay_reads_bounded_segments_and_never_the_full_history` — 60 条历史：段读取 ≥ 2 次、
   **没有任何一次返回全量**、每次 `limit`/`byte_budget` 等于命名常量、首段无游标后续段带游标、
   `full_reads == 0`（未协商路径**不调用** `load_session_history`）、逐条交付无重复无遗漏。
5. `streamed_canonical_replay_failure_closes_the_load` — 首段 `Corrupt` → `-32603`，**零通知**送出，存储事实不变。
6. `streamed_canonical_replay_fails_closed_when_the_anchor_vanishes_mid_stream` — 锚点消失：2 次读取（首段成功、
   续读带真实存储错误回来）→ `-32603`，且「半份首屏」确实存在（正是失败关闭要挡的组合）。
7. `streamed_canonical_replay_falls_back_to_the_full_read_when_the_segment_read_is_unsupported` — 替身返回
   `Unsupported`：只 1 次段读取即回退，`full_reads == 1`，正文仍等于完整 canonical 序列，
   日志含 `falling back to the full canonical read`（`tracing` 线程局部订阅捕获）。
8. `streamed_canonical_replay_of_an_empty_session_succeeds_without_notifications` — 空会话：1 次段读取、0 通知、
   0 全量读取。
9. `streamed_canonical_replay_delivers_an_inherited_only_segment` — 只有继承区：继承区 payload 照常投影、顺序不变、
   0 全量读取。
10. `streamed_canonical_replay_delivers_an_oversized_body_whole` — 530 KB 单条正文独占一段且**整条交付**
    （内容逐字节相等）。
11. `streamed_canonical_replay_of_a_live_session_still_reads_storage` — `AlreadyLive`（手动把热态有效集合缩到只剩
    尾条目）：回放仍给出存储里的 3 条 canonical 内容（含归档原文），`full_reads == 0`。
12. `negotiated_initial_load_keeps_the_effective_set_and_the_entry_marks` — 协商连接回归：冷恢复 + 3 条（2 条归档）
    → 只回放 1 条**有效**条目，带 `periReplay` + `periLoadGeneration` + `periEntryId`（`{sid}:{payload id}`）+
    `periPosition`，且 `full_reads == 0`、段读取 0 次。

### 4.3 全量 lib 的 1 次失败与归因（实测）

1. 第 1 次全量：`session::command::compact::tests::report_tests::test_compact_report_manual_full_and_cold_reload`
   在 `peri-acp/src/session/command/compact_report_test.rs:128` panic（`open_existing_read_only(...).unwrap()` →
   `DatabaseUnreadable`）。
2. 单跑该用例：`1 passed; 0 failed; ...; 874 filtered out; finished in 1.42s`。
3. 第 2 次全量：同一条用例通过，`875 passed; 0 failed`（用例总数与第 1 次一致：875）。
4. 观察：该用例的库路径来自各自的 `tempfile::tempdir()`，与 S9b 无关；S9b 不触碰 `open_existing_read_only`、
   compact 报告与存储打开路径。**静态判断**：并行度下的偶发（reader 打开瞬时失败），非本切片引入——但我没有
   在父提交上复现该 flake（见 §5.2），因此不做“必然无关”的强断言。

## 5. 未验证与不确定项

1. **`periLoadGeneration` 的门控与任务描述冲突（需要裁决）**：任务文本把 `periLoadGeneration` 列为未协商连接
   「通知与响应里不得出现」的字段；但既有契约（前序切片 + `docs/design/peri-acp-protocol.md`）把它门控在
   `caps.replay`（`peri.replay` 扩展）上，**与分页无关**。实测：只声明 `peri.replay`、不声明 `peri.historyPagination`
   的连接仍会拿到 `_meta.periLoadGeneration` 与通知上的同名戳（用例 3 记录此事），但**没有任何分页私有字段**。
   本切片选择**不改门控**：改它等于改动前序切片已冻结的 wire 行为，并会让既有断言（`dispatch/session_replay_test.rs:87,106`、
   `host/session_lifecycle_replay_test.rs:21`、`host/history_page_wire_test.rs:706,733`）失效，超出本切片授权范围。
   若要求「未协商分页 ⇒ 一律无 `periLoadGeneration`」，请确认后再改门控为 `caps.replay && history_pagination`
   （并同步上述 4 处断言 + 协议文档）。
2. **全量 lib 的 flake**：未在父提交/前序切片状态上复现对照（需要 git 写操作切换状态，本切片禁止），
   所以「与本切片无关」目前由「单跑通过 + 第二次全量通过 + 代码路径不相交」支撑，不是对照实验。
3. **`Unsupported` 发生在已交付之后**的分支（→ 读失败）**无测试覆盖**：真实门面不会中途改变支持能力，
   替身也只覆盖首段前返回 `Unsupported`。该分支是代码里明确写出的选择（宁可可见失败，不重复交付）。
4. **生产后端的 `Unsupported` 路径未在真实非覆盖后端验证**：S9a 的 SQLite 与 remote 两个后端**都**实现了
   `load_history_segment`，因此回退路径只在替身下被跑过；真实世界里「后端不支持」等价于第三方/未来后端的默认实现。
5. **`tail` 上界论证是静态判断**：依据 `host/prompt.rs:609-613` 的既有注释与 `finish_prompt_turn` 的写入顺序
   推理，未做并发实验；「进行中的 turn 里已流式发出但未落盘的正文不在本次交付内」这一点在改动前后一致。
6. **继承区不分页**：本切片不改，首屏里继承区仍可能一次给出大量 payload（S9a 契约的已知边界）。
7. **`prepare_existing` 仍把有效集合装进内存**：本切片只改「回放源」，未改冷恢复的内存装载量；
   「未协商连接首屏时内存峰值 = 一段」成立，但会话建立本身仍读有效集合。

## 6. 风险

1. **未协商连接的 `session/load` 现在会读存储**（此前纯内存）：每次 load 多一次（首段）到多次段读取。
   有界（≤ 50 行 / 512 KiB 每段），但慢盘/远端后端上首屏延迟会比纯内存略高——这是 REQ §5.3 的取舍。
2. **`tail` 之上是「本次交付之后的新增」**：若客户端把首屏边界当作「某时刻的快照」使用，需依赖实时通知通道补齐；
   既有客户端语义不变（改动前同样以快照交付）。
3. **失败关闭更严格**：非 `Unsupported` 的段读取错误现在会让整个 `session/load` 返回 `-32603`（此前未协商路径
   是内存回放，基本不会失败）。对客户端而言这是新的可见失败面，需要 TUI/SDK 侧确认能按「load 失败可重试」处理。
4. **常量与历史页默认值同值但独立**：未来若只改一侧，两条出口的内存上限会分叉（注释已写明理由，但无编译期约束）。
