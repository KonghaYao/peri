# S9（D7）独立复核报告：未协商（标准）ACP 客户端的 session/load 完整性

- 复核者：独立 verification（非实现者，不采信实现者报告）
- 日期：2026-10-13
- Worktree：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
- 分支：`feat/transcript-shrink-history-pagination`；HEAD `5be1076`（2026-10-13 17:18:31）；复核对象 = HEAD 之上的**未提交工作区**（前序切片 + S9a / S9b 全部未提交）
- 需求依据：`/Users/konghayao/Desktop/peri-transcript-history-requirements.md` §5.3（D7）、§9、A25
- 复核目标（三条，缺一不可）：
  1. 未协商（标准）ACP 客户端在 `session/load` 上拿到**完整** canonical 历史；
  2. **没有被误告知**——只有真交付完整历史才成功响应，读取失败必须让 load 失败；
  3. 通知与响应体里**没有分页私有语义**；协商路径行为完全不变。
- 方法：① 从 `git diff` / `git show HEAD:` 独立读代码，写「改动前/后行为对照」；② 在新测试文件内加**独立复核用例**（不调用实现者的投影函数，独立第二实现推导期望值）实跑；③ 只读复核（除复核用例与其 helper 外未改动任何文件，含生产代码）。

---

## 0. 判定摘要（逐条）

| # | 复核项 | 判定 | 依据（实测 P / 静态 S） |
|---|--------|------|--------------------------|
| 1 | 改动前/后行为对照，独立从 diff 读出 | PASS | S（`git show HEAD:` 逐文件比对） |
| 2 | 未协商回放 = `load_session_history` canonical 输出（含 Full 归档夹具，条数/顺序/正文逐条相等） | PASS | P（复核用例 s9#1 + 实现者 11 例） |
| 3 | 无误告知：段读取失败 / 锚点消失 / 回退全量失败都让 load 失败（`-32603`），不假成功 | PASS | P（复核用例 s9#2 + 实现者 2 例）；S（旧代码 warn+继续 已被删除） |
| 4 | 无私有语义：未协商连接的响应体 + 全部通知无 `periEntryId` / `periPosition` / `periLoadGeneration` / `periReplay` / `nextCursor` / `snapshotToken` | PASS | P（wire 键扫描；协商连接对照仍带这些字段） |
| 5 | 协商回归：测试 diff 无删除/放宽断言；协商相关用例实跑通过 | PASS | P（实跑）+ S（diff 逐条核对） |
| 6 | 无 DDL、无新依赖（Cargo.toml/Cargo.lock 零改动） | PASS | P（三条命令输出为空 / 0） |
| 7 | 反例 | 未发现（残余观察见 §8，均未构成反例） | P |

补充判定：**未协商路径不再读内存热态 `history_payloads`**（含 `AlreadyLive` 场景），一律从存储重读 —— PASS（P，实现者用例 `streamed_canonical_replay_of_a_live_session_still_reads_storage` 与复核用例 s9#1 的 `full_reads=0` + 段读取计数）。

结论：**§5.3（D7）达成**（在上述判定的范围内）。三条目标全部满足；无 FAIL、无 PARTIAL。未验证项（§7）不改变结论，但影响信心边界。附带发现：`peri-resources` 全量存在既有并行负载 flake（三次样本 2 红 1 绿，单独重跑通过，非本切片引入，不改变判定），见文末附 C。

---

## 1. 范围

### 1.1 覆盖
- `session/load` 的**未协商**路径：请求分流、canonical 流式回放（`dispatch::replay_canonical_history_streamed`）、失败语义、wire 输出（响应 + 通知）。
- `session/load` 的**协商**路径回归：热态有效集合回放、`periEntryId` / `periPosition` / `periLoadGeneration` 字段、`nextCursor` / `snapshotToken` 页模式。
- 能力协商与按连接判定（`initialize` → `commit_initialize` → `history_pagination()` / `facts()`）。
- 存储侧支撑（`read_history_segment` / `load_history_page` / `load_entry_positions` / `load_active_snapshot`），SQLite 与 Remote 两个后端。
- 构建面：无 DDL、无新依赖。

### 1.2 不覆盖（见 §7）
- 真实 stdio/TUI 端到端（仅单元与 MPSC ServerLoop wire 级）。
- Remote/云端后端在 ACP 层的端到端（只到 `peri-resources` 存储层）。
- `peri-resources` 全量中既有并行 flake 的**根因修复**（复核只定位到时序敏感点并定性，未改代码，见附 C）。
- 性能（有独立 `perf-evidence.md`，不在本报告范围）。

---

## 2. 改动文件清单（绝对路径 + 一句话）

> 读取方式：`git status --porcelain`（115 项）。
> **路径根 = `/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history/`**；下表中的路径均相对此根（完整路径 = 路径根 + 表内路径）；文件名后带 `（新）` = 本 worktree 新增文件（未跟踪）。
> 全部为工作区未提交改动（本 worktree 累积前序切片 + S9a/S9b）；`.recon/` 是复核报告目录（本报告所在），`spec/issues/2026-10-13-transcript-shrink-history-pagination.md` 是蓝图。

### 2.1 类型契约层 `peri-acp-types/src/`
| 文件 | 一句话 |
|------|--------|
| `peri_caps.rs` | `PeriCaps` 新增 `history_pagination`（`peri.historyPagination`，缺省 false，`all_enabled()` 含 true） |
| `history_page.rs`（新，832 行） | 历史分页契约唯一事实源：游标 / 页 / 段计划与阶段化读取（`plan`/`body_ids`/`materialize`） |
| `history_page_test.rs`（新） | 上述契约单测（19 例） |
| `session_resources.rs` | `ActiveSessionSnapshot` / `RewindOutcome` / `Cursor` 错误类；新读方法 `load_active_snapshot` / `load_history_page` / `load_history_segment` / `history_page_bounds` / `load_entry_positions`（默认实现 = 测试替身回退） |
| `store/history.rs`、`store/mod.rs` | 持久化历史契约（`PersistedPayload` 的 excluded 语义与使用） |
| `session_resources_test.rs`、`store/history_test.rs`、`peri_caps_test.rs`、`lib.rs`、`session/execution.rs` | 上述契约的导出与测试同步 |

### 2.2 ACP 宿主层 `peri-acp/src/`
| 文件 | 一句话 |
|------|--------|
| `host/requests.rs` | 唯一生产入口改 `handle_request_for_connection(..., ConnectionFacts)`；旧 `handle_request` 降为 `#[cfg(test)]` 包装（固定 `unnegotiated()`） |
| `host/server_loop.rs` | `session/load` 进生命周期路由**之前**按 `classify` 分流（页模式不取执行锁）；初始 load 成功后**锁外** `attach_initial_cursor` |
| `host/history_page.rs`（新，408 行） | 页模式实现：`classify` / `handle_page` / `attach_initial_cursor`；常量 50 行 / 100 条 / 512KiB |
| `host/connection.rs` | `ConnectionContext`（`commit_initialize` 只认第一次、`caps`、`history_pagination` 失败关闭、`facts`）+ `ConnectionFacts`（Copy，只装准入判定） |
| `host/requests/session_restore.rs` | `handle_load(..., history_pagination)`：协商走热态有效集合 + 位置表；未协商走存储流式 canonical；**回放失败 `map_err` 为 `-32603`**（不再 warn 后成功） |
| `host/requests/session_lifecycle.rs` | 抽出 `initialize_peri_caps`（协商落库/回报） |
| `host/mod.rs` | `SessionState.structure_version`（进程内，无 DDL）、`mod history_page`、入口导出 |
| `dispatch/session_replay.rs` | `EntryMarks`/`ReplayMarks`/`write_entry_marks`/`entry_id`、`replay_canonical_history_streamed`（分段 + 受限回退）、`replay_meta` 门控、`ReplayError::HistoryReadFailed` |
| `dispatch/rewind.rs`、`dispatch/session_fork.rs`、`dispatch/mod.rs`、`dispatch/session_load.rs` | rewind 增 `clear_excluded`、fork 发布集合对齐、`load_active_session_payloads` / `load_history_page` / 错误码映射 |
| `host/history_page_test.rs`、`host/history_page_wire_test.rs`（新） | 页模式单测与 MPSC ServerLoop wire 级测试（7 例） |
| `host/requests_unnegotiated_replay_test.rs`（新） | 未协商流式回放测试（含**本复核者新增 3 例**，见 §6） |
| `host/requests/{legacy_session,rewind}.rs`、`session/command/{mod,rewind}.rs`、`host/prompt.rs` | 签名同步与有效集合使用 |
| 既有测试文件 40+ 个（`*_test.rs`） | 签名 / 类型同步（`marks` 参数、`commit_initialize(caps)`、`structure_version`、`clear_excluded`），**无删除测试函数、无放宽断言**（§5.3 实测） |

### 2.3 存储层 `peri-resources/src/sessions/`
| 文件 | 一句话 |
|------|--------|
| `sqlite_store/session_data/history.rs` | `read_history_segment`（TAIL/HEAD/ANCHOR/NEXT，锚点缺失 → `Expired`，不兜底）；`load_entry_positions` |
| `sqlite_store/{session_data.rs,context.rs,compaction.rs}` | `load_active_snapshot`（`excluded = 0`）、`read_history_page` 两阶段读、辅助 |
| `sqlite_store/row_mapping.rs` | `THREAD_META_COLUMNS` 的 `SUM(LENGTH(...))` 聚合替换为常量列 `0 as content_size`（保留列位） |
| `sqlite_store/session_history_page_test.rs`、`session_history_segment_test.rs`、`session_history_perf_test.rs`（新） | 页 / 段 / 性能测试 |
| `remote/{session_data.rs,session_read.rs,session_sql.rs,session_history.rs,session_codec.rs,mod.rs}` | Remote adapter 实现同一批读方法（段 / 页 / 范围 / 位置） |
| `remote/session_history_page_test.rs`、`remote/session_history_rewind_test.rs`（新） | Remote 契约测试 |
| `resources.rs`、`data.rs`、`tests/session_resources_contract.rs`、`sqlite_store/session_history_test.rs`、`remote/{full_remote_test.rs,session_shape_test.rs,schema_probe_test.rs,schema_upgrade_test.rs}` 等 | 方法转发与测试夹具同步（两个 schema 测试文件仅夹具类型改动：`Mutex<Option<&'static str>>` → `Mutex<Option<String>>`，新增 `SqliteTransport::for_test(pool)`，**无 DDL 文本变化**） |

### 2.4 上游与前序切片（`peri-agent/src/`、`peri-tui/src/`、文档）
| 文件 | 一句话 |
|------|--------|
| `peri-agent/src/session/transcript*.rs`、`agent/stages/*`、`session/exec/*`、`session/subagent/factory/*` 等 26 个 | 前序切片的 Full 归档（excluded）产生与持久化；本切片 S9 的输入前提 |
| `peri-tui/src/kit/acp_bridge_test.rs`、`acp_bridge_cancel_test.rs` | 仅测试签名同步（`replay_persisted_session_history` 增 `marks` 参数 + import 排序） |
| `docs/design/peri-acp-protocol.md`、`docs/code-index/{peri-acp,peri-acp-types,peri-agent,peri-resources}.md`、`peri-acp/CLAUDE.md`、`spec/issues/2026-10-13-transcript-shrink-history-pagination.md` | 协议 / 索引 / 蓝图文档 |

**复核者改动（唯一）**：`peri-acp/src/host/requests_unnegotiated_replay_test.rs`（+3 用例、+2 helper、`SpyMode` 增一个分支、import）——见 §6。

---

## 3. 改动前 / 后行为对照（独立读 diff 得出）

### 3.1 入口与分流
| 维度 | 改动前（HEAD `5be1076`） | 改动后（工作区） |
|------|--------------------------|------------------|
| 请求入口 | `host/requests.rs:31 handle_request(method, params, cfg, sessions, transport)`（无连接概念） | `host/requests.rs:55 handle_request_for_connection(..., ConnectionFacts)`；旧函数为 `#[cfg(test)]` 包装（固定未协商） |
| `session/load` 分流 | `server_loop.rs:367 dispatch_request(id, method, params)` 直接进生命周期路由，无模式概念 | `server_loop.rs:383-390`：**先** `history_page::classify(&params, connection_facts.history_pagination)`；`HistoryPage` 直接 `handle_page` 返回（不取执行锁、不装配、不广播）；`InitialLoad` 记录 `load_initial_cursor` 供**锁外**后置附加 |
| classify 失败 | 无此概念 | 直接回错误响应（`-32601` + `data.reason="pagination_not_negotiated"` 等；`host/history_page.rs:124-143`） |
| 能力判定 | 无 | 按**当前连接**：`connection.rs:42 commit_initialize`（只认第一次）、`:72 history_pagination()`（未初始化 / 未协商 → false，失败关闭）；`caps()` 未初始化时兜底 `all_enabled()`（进程内路径语义，有注释与单测） |

### 3.2 未协商 `session/load` 的历史来源与失败语义
| 维度 | 改动前 | 改动后 |
|------|--------|--------|
| 历史来源 | `session_restore.rs`（HEAD）`:510 let history_payloads = state.history_payloads.clone();` 回放**内存热态** | 未协商分支调 `dispatch::replay_canonical_history_streamed`（`session_replay.rs:306`）：从存储**逐段读**（不读 `state.history_payloads`），`session_restore.rs:582` |
| 回放失败 | HEAD `:525 tracing::warn!(... "history replay failed, continuing")` —— **照样返回成功响应** | `session_restore.rs:595-599`：`map_err` 成 `-32603`（消息 `session/load: history replay failed: ...`）；不拆除已发布会话状态，下一次 load 走 `AlreadyLive` 重交付 |
| 私有字段 | 无（字段不存在） | `ReplayMarks::without_entry_marks`（`session_replay.rs:104`）→ 不打 `periEntryId`/`periPosition`；`replay_meta` 的 `periReplay`/`periLoadGeneration` 门是 `caps.replay`（未协商为 false） |
| 已有活跃会话 | 直接复用内存 payloads | 仍从存储读（未协商不看 `state.history_payloads`）——实测覆盖 |

### 3.3 协商路径
| 维度 | 改动前 | 改动后 |
|------|--------|--------|
| 回放源 | 内存 payloads（= 当时有效集合） | 热态**有效集合**（excluded 过滤后的 `ActiveSessionSnapshot`，`session_restore.rs:566-572`）+ `load_entry_positions` 位置表（读失败即 load 失败） |
| 条目身份 | 无 | `ReplayMarks::new` 打 `periEntryId = "{sid}:{uuid}"` / `periPosition`（u64 rowid） |
| 分页 | 无 | `attach_initial_cursor`（`history_page.rs:271`）在初始 load 成功后、**执行锁之外**写 `nextCursor`（null = 没有更早可展示内容）与 `snapshotToken` |

### 3.4 canonical 流式回放实现要点（`dispatch/session_replay.rs`）
- 段预算：`REPLAY_SEGMENT_ROWS = 50`、`REPLAY_SEGMENT_BYTE_BUDGET = 512 * 1024`；首段固定 `tail`（当时最大 rowid），向后（更早）逐段；单条超预算独占一段并完整交付。
- 回退：**仅当** `SessionResourceErrorKind::Unsupported` **且** `delivered == 0` 才回退 `load_session_history` 全量（有可区分日志 `falling back to the full canonical read`，`session_replay.rs:341`）；其他错误 → `ReplayError::HistoryReadFailed`（`:347`、`:390`）。
- 终止：`next_cursor = None` 才结束。

---

## 4. 契约与不变量

1. **分页是显式协商的 Peri 扩展**：`initialize` 的 `peri.historyPagination: true`；能力按**当前连接**判定；重连后重新分流；未初始化/未协商 → 关闭（失败关闭）。
2. **未协商连接语义 = 标准 ACP**：不带 `cursor` 的 `session/load` 是初始 load，从存储流式回放**完整 canonical 历史**（含 `excluded` 行的过滤由 canonical 投影决定，见 §5.2 实测）；输出只含标准 `session/update` 通知与标准响应字段。
3. **未协商 + `cursor` 必须拒绝，不降级**：`-32601`，`data.reason = "pagination_not_negotiated"`。
4. **误告知禁止**：段读取失败 / 锚点消失（`Expired`）/ 回退全量读取失败 → load 失败（`-32603`），零通知或已投递通知后中止，**不返回「成功」**。
5. **私有语义门控**（三组，互不越界）：
   - `periEntryId` / `periPosition`：门 = `EntryMarks::merge`（连接协商过分页）；
   - `periReplay` / `periLoadGeneration`（响应与全部通知的 `_meta`）：门 = `caps.replay`；
   - `nextCursor` / `snapshotToken`：门 = 连接协商过分页且为初始 load 的成功响应。
6. **页语义**：页严格更早于上一页、页内按历史正序；`snapshotToken` 内 `position` 才可比较；`nextCursor = null` 仅表示没有更早可展示内容。
7. **存储 fail-closed**：段读取锚点缺失 → `Expired`（不兜底）；只有 `Unsupported` 且本段零交付才允许回退全量。
8. **无 DDL / 无新依赖**：`structure_version` 只存进程内内存；`row_mapping` 去掉聚合计算；`Cargo.toml` / `Cargo.lock` 零改动。

---

## 5. 实测证据

> 命令纪律：所有命令均以 `cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history &&` 开头；仅用 `./scripts/cargo-rmcp-patched.sh`；无 `--workspace`；一次一个 cargo 进程；前台 + 长超时。

### 5.1 编译（实测）
```
./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp-types   → Finished dev profile ... in 0.45s
./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp         → Finished dev profile ... in 0.80s
./scripts/cargo-rmcp-patched.sh check --locked -p peri-resources   → Finished dev profile ... in 1.24s
```

### 5.2 测试（实测，全部通过）
| 命令（过滤串） | 结果 |
|----------------|------|
| `test --locked -p peri-acp --lib -- streamed_canonical_replay` | **11 passed / 0 failed**（实现者 S9b 用例：归档正文按 canonical 顺序交付且 `full_reads=0`、未协商 wire 无 Peri 私有键、`replay` caps 下只有代次、有界分段且无一次返回全量、Corrupt→`-32603` 且零通知、锚点消失→第二段失败且半途即有通知、Unsupported 回退全量 + 日志、空会话、仅继承区、530KB 单条独占一段完整交付、`AlreadyLive` 仍从存储读） |
| `test --locked -p peri-acp --lib -- history_page_wire_tests` | **7 passed**（真 ServerLoop + MPSC：执行锁下翻页成功、未协商带 cursor→`-32601` 且不触发初始 load、首游标遍历归档区不重叠、同 cursor 等价页/limit 只改页长、cursor 在追加与再 Full 后成员不变、回放失败→`-32603` 而非假 cursor、协商首屏带可比身份） |
| `test --locked -p peri-acp --lib -- negotiated_initial_load` | **2 passed** |
| `test --locked -p peri-acp --lib -- s9_verification` | **3 passed / 0 failed**（本复核者新增，见 §6） |
| `test --locked -p peri-acp-types --lib -- history_page` | **19 passed** |
| `test --locked -p peri-resources --lib -- history_segment` | **12 passed**（含 remote adapter 3 例：归档与系统行 / 锚点消失失败关闭 / 与本地后端等价） |
| `test --locked -p peri-acp --lib`（全量） | 见 §5.6 |

### 5.3 复核者的独立完整性用例（关键实测）
`test --locked -p peri-acp --lib -- s9_verification` → **3 passed / 0 failed**。用例逻辑要点：

- **s9#1 `s9_verification_unnegotiated_replay_equals_canonical_history_for_mixed_payloads`**
  - 夹具含：human / AI blocks（reasoning + text + tool_use）/ AI `ai_with_tool_calls`（正文 + tool_calls 派生）/ tool 结果 / System；**归档（excluded）行交错在中间**（索引 1..4，含工具调用与工具结果）。
  - 期望序列由本用例**独立第二实现** `expected_wire_facts`（按 ACP 投影契约手写，不调用 `project_payload_updates`）从 `load_session_history` canonical 输出逐条推导；与 wire 上 `session_update` 事实（`text|messageId|正文`、`tool|callId`、`tool_update|callId`）**逐条相等**。
  - 断言：`full_reads = 0`；响应与全部通知中**无** `nextCursor` / `snapshotToken` / `periEntryId` / `periPosition` / `periLoadGeneration` / `periReplay`。
  - 意义：直接回应「完整 + 无误告知 + 无私有语义」中最小可观测的那一条——**未协商回放序列 == canonical 输出**（含归档交错的边界）。
- **s9#2 `s9_verification_fallback_full_read_failure_still_closes_the_load`**
  - 新 `SpyMode::UnsupportedWithFailingFullRead`（段读取 Unsupported **且**回退全量读取也 Corrupt，模拟真实故障）。
  - 断言：`-32603`、段读取 1 次、全量读取 1 次、**零通知** —— 回退路径不得误报成功。
- **s9#3 `s9_verification_negotiated_connection_still_carries_the_entry_marks`**
  - 协商连接（`replay` + `history_pagination`）在同一含归档夹具上：只回放**有效集合**、逐条 `periReplay = true`、`periLoadGeneration` 与响应一致、`periEntryId == "{sid}:{uuid}"`、`periPosition` 为 u64、归档 id 不出现、`full_reads = 0` 且 `segment_calls = 0`（协商路径完全不碰存储段读）。

### 5.4 无 DDL / 无新依赖（实测）
```
git diff --stat HEAD -- '**/Cargo.toml' 'Cargo.toml' 'Cargo.lock'          → 空
git diff HEAD | grep -E "^\+" | grep -ciE "ALTER TABLE|CREATE TABLE|CREATE (UNIQUE )?INDEX|DROP TABLE"  → 0
git status --porcelain | grep "^??" | grep -iE "\.sql|schema"              → 空
```
- 唯二带 schema 字样的改动是测试夹具（`remote/schema_probe_test.rs`、`remote/schema_upgrade_test.rs`），无 DDL 文本变化。
- 新依赖：`Cargo.toml` 零改动；`Cargo.lock` 零改动。

### 5.5 测试 diff 逐条核对（静态，`git diff -U0` 扫描）
- 各类测试文件新增 `#[test]/#[tokio::test]` 计数（`+n -0`，除注明外）：`peri_caps_test +1`、`session_resources_test +1`、`store/history_test +8`；`session_fork_test +1`、`session_replay_test +2`、`connection_test +2`、`requests_history_lifecycle_test +1`、`requests_rewind_cases_test +10`、`compact_persistence_test +1`；`full_remote_test +4`、`session_shape_test +1`、`session_history_test +6`、`tests/session_resources_contract.rs +1`。
- **没有删除任何测试函数**。
- 对 `peri-acp` 测试文件 `git diff -U0` 扫描「删除行里含 assert」：仅 1 处 = `session_fork_test.rs` 的 `-assert_eq!(copied_payloads.len(), source_payloads.len());`，被**扩写**为带说明的三行断言（`"source 无已排除条目：发布集合与复制结果一致"`）——**未放宽**。
- 其余删除行均为签名 / 类型同步（`rewind_history` 增 `clear_excluded` 与 `RewindOutcome`；`replay_session_history` / `replay_persisted_session_history` 增 `marks`；`commit_initialize()` → `commit_initialize(caps)`；`SessionState` 增 `structure_version`；`session_lifecycle_replay_test.rs` caps 增 `replay: true` 并**新增** `assert_eq!(params["data"]["periLoadGeneration"], "gen-1")`）。
- `requests_legacy_test.rs` 的 M5 故障注入点从 `load_session_history` 移到真正被调用的 `load_active_snapshot`，并把 `load_session_history` 加回转发宏列表（有注释说明）——属于随实现变化的同步，不削弱断言强度。

### 5.6 全量重跑（本轮复核，含复核者新增 3 例）
```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib
→ test result: ok. 878 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 266.15s
```
- 基线（加入复核者用例之前，同一工作区）：`875 passed; 0 failed; ... finished in 267.01s`（原始日志 `/private/tmp/peri-acp-full-lib-run2.log`，该日志中 `s9_verification` 出现 0 次，确认不含复核用例）。
- 差值 **+3 = 复核者新增的 3 个 `s9_verification_*` 用例**；**0 failed** 说明新用例与既有套件不冲突，协商回归在真实全量运行中无失败。
- `peri-acp-types` / `peri-resources` 全量：见文末「附」。

---

## 6. 复核者新增的独立用例（标注）

- 位置：**`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history/peri-acp/src/host/requests_unnegotiated_replay_test.rs`**（文件共 1216 行，复核用例位于 954-1216；标记 `// ── S9-verification：独立复核用例（复核者加入，非实现者用例）──`）。
- 组成：3 个 `#[tokio::test]`（§5.3）+ 2 个 helper（`expected_wire_facts`、`observed_wire_facts`）+ `SpyMode` 增 `UnsupportedWithFailingFullRead` 分支 + 1 条 import。
- 来源声明：**这些用例不是实现者写的**，由复核者为本次判定加入；它们驻留在实现者新建的测试文件内（便于复用其真实 SQLite 门面与 `ReplaySpy`），编译与运行方式与普通 `#[cfg(test)]` 用例一致。除本文件外，复核者未改动任何文件（含生产代码）。
- 复核者未修改任何生产代码（已验证：本文件之外无复核者改动）。

---

## 7. 未验证与不确定项（未跑 = 未验证）

1. **未验证**：Remote / 云端后端在 ACP 层的未协商回放端到端。已跑到 `peri-resources` 存储层（remote 段读取 3 例 + `full_remote_test` 4 例 + 新增页/rewind 测试）；ACP 层复核用例只走 SQLite 真门面。
2. **已补跑（本轮）**：`peri-acp-types` 全量 **576/0**（8.81s）；`peri-resources` 全量三次样本 **2 失败 / 1 通过**，失败者是既有「打开 / 锁」类测试的并行负载 flake（单独重跑通过，定性见文末附 C，非本切片引入，不改变 S9 判定）。
3. **未验证**：真实 stdio / TUI 端到端（仅有单元 + MPSC ServerLoop wire 级）。
4. **静态判断（非实测）**：TUI 的 `register_ui_commands` 以 `PeriCaps::all_enabled()` 为基座（仅关 `oauth`、替换 `ui_commands`），`all_enabled()` 新增 `history_pagination: true` → **TUI 经 initialize 后走协商路径**（会看到分页私有字段）。当前 TUI/SDK **无** `nextCursor` / `snapshotToken` / 分页 items 消费者（workspace 列表有同名无关字段），因此不构成 D7 反例，但属于「协商路径行为发生变化」的边界，值得主流程确认是否可接受。
5. **静态判断（非实测）**：`structure_version` 是进程内计数器，重启归零、无 DDL 是设计选择（注释说明重启后旧游标仍需通过存储锚点 / 范围校验）。
6. **未验证**：`-32601` 未协商带 cursor 的错误码形状在所有客户端 SDK 的兼容性（只验证了 wire 层响应结构）。

---

## 8. 风险与残余观察

| 风险 | 级别 | 说明 / 缓解 |
|------|------|-------------|
| Remote 端到端未实测 | 中 | 存储层已有等价测试；若 Remote adapter 的段读取与 SQLite 语义有偏差，只会在云端暴露。建议在主流程补一条 ACP 层 remote 用例 |
| 未协商路径从存储**重读**（而非内存热态）会带来额外 I/O | 低 | 这是 D7 的必要代价（内容正确性优先）；已有分段预算与 `full_reads=0` 断言；性能面另见 `perf-evidence.md`（本报告不覆盖） |
| TUI 默认协商分页字段（§7.4） | 低 | 无消费者，行为无破坏；若产品要求「标准客户端」默认，应把 `all_enabled()` 的该项关掉或让 TUI 显式声明 |
| 回放失败后的会话态 | 低 | 失败不拆状态，下一次 load 走 `AlreadyLive` 重交付（实测覆盖）；但**半途已投递的通知**无法撤回（协议固有），客户端需按「load 失败即丢弃本次历史」处理 —— 已在 design 文档写明「不返回半回放的『成功』响应」 |
| `structure_version` 重启归零 | 低 | 旧游标仍要过存储锚点 / 范围校验；无 DDL 是明确设计取舍 |
| `peri-resources` 全量并行 flake（既有，非本切片引入） | 中（对 CI，不对 D7） | 打开 / 锁类既有测试在并行全量下偶发 `busy` / `unreadable`（三次样本 2 红 1 绿，单测通过）；根因是 `lock_schema_open(..., Duration::ZERO)` 零预算等待与异步释放时序。建议主流程跟踪（见文末附 C） |

**反例**：本轮未发现反例。两条接近反例的观察均被排除：
- 未协商带 `cursor` → `-32601` + `data.reason`：不是反例，是设计（不降级），且有 wire 实测；
- 协商连接上的 `periEntryId` / `periPosition`：不是反例，是显式协商后的扩展字段，未协商连接一个字段都不写（实测键扫描）。

---

## 附：全量重跑记录（本轮复核，2026-10-13 深夜）

### A. `peri-acp`（含复核者 3 例）
```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib
→ test result: ok. 878 passed; 0 failed; 0 ignored; finished in 266.15s
```
基线（加入复核者用例前）875 passed（`/private/tmp/peri-acp-full-lib-run2.log`，其中 `s9_verification` 出现 0 次）。差值 +3 = 复核者新增 3 例；0 failed。

### B. `peri-acp-types`（全量）
```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib
→ test result: ok. 576 passed; 0 failed; 0 ignored; finished in 8.81s
```

### C. `peri-resources`（全量，含 flake 观察）
三次运行样本（均为完整全量；第三次命令行带 `--skip session_history_perf` 但 `0 filtered out`，参数未命中任何用例，等同完整全量）：

| 次 | 命令 | 结果 |
|----|------|------|
| 1 | `test --locked -p peri-resources --lib`（组合运行中的第二段） | **2 failed** / 495 passed（`sessions::open_tests::explicit_read_only_shares_selection_and_writes_nothing`、`sessions::sqlite_store::connection::open_tests::incompatible_schema_is_rejected_without_wal_or_database_changes`） |
| 2 | 同上（单独重跑） | **1 failed** / 496 passed（仅 `incompatible_schema_is_rejected_...`） |
| 3 | 同上（skip 参数未命中） | **0 failed** / 497 passed（56.96s） |

失败测试单独重跑：**2/2 通过**（各 0.97s / 1.90s）。

**定性（复核者判断）：既有测试的并行负载时序 flake，非本切片引入。** 证据链：
1. 两个失败测试文件（`peri-resources/src/sessions/open_test.rs`、`.../sqlite_store/connection_open_test.rs`）**均不在**本切片改动集（`git status --porcelain` 对该两路径输出为空）；其生产代码路径（`sqlite_store/connection.rs`、`sessions/open.rs`）也无改动。
2. 失败点均为**打开 / 锁**路径：`open_test.rs:465` 只读打开（`ReadOnlyStoreErrorKind::DatabaseUnreadable`，错误文案来自 `connection.rs:84`）；`connection_open_test.rs:377` 的 `lock_schema_open(&path, Duration::ZERO)` —— **零预算**抢占 OS 文件锁（`file.try_lock()`），只要瞬间被占即 `WouldBlock` → `"session database initialization is busy"`（`connection.rs:259-285`）。该写法来自历史提交 `57d42aeac`，先于本切片。
3. 单测通过、第三次全量通过，说明与「确定性回归」无关，而是并行全量下的 I/O 时序敏感。
4. 本切片新增了若干 SQLite 测试文件，**可能**提高了并行负载与触发概率，但根因在上述既有测试自身的零预算等待与异步释放时序（`session_history_perf_test.rs:173-181` 的注释也记录了「上一个池仍在异步关闭」这一既有现象）。
5. 影响面：`peri-resources` 全量在 CI 上可能偶发红；不涉及 S9 的行为正确性。建议主流程单独跟踪（如给该处等待一个小的非零预算，或把只读/锁类测试串行化）。

