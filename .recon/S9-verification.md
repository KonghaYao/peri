# S9（D7）独立复核报告：未协商（标准）ACP 客户端的 `session/load`

- 复核者：独立 verification（不采信实现者报告，也不采信上一轮报告；结论全部从代码与实测重新得出）
- 日期：2026-10-13（round 2）
- Worktree：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
- 分支：`feat/transcript-shrink-history-pagination`；HEAD `5be107603`；复核对象 = HEAD 之上的**未提交工作区**（前序切片 + S9a/S9b 全部未提交）
- 需求依据：`/Users/konghayao/Desktop/peri-transcript-history-requirements.md` §5.3（D7）、§5.1/§5.2、§9、A25
- 上一轮报告已保留为 `.recon/S9-verification.round-1.md`（本文件按任务指定路径覆盖写入；如需可删除保留件）
- 复核者改动：**仅** `peri-acp/src/host/history_page_wire_test.rs` 追加 2 个复核用例与 2 个私有 helper（见 §6）；**未改任何生产代码**；未 commit / 未动 git 状态

---

## 0. 判定摘要（逐条）

| # | 复核项 | 判定 | 依据 |
|---|--------|------|------|
| 1 | 改动前/后行为对照（独立从 `git show HEAD:` + 工作区读出） | PASS | 静态（S） |
| 2 | 未协商连接回放 == `load_session_history` canonical 输出（含 Full 归档夹具；条数/顺序/正文） | PASS | 实测（P）：`s9_recheck_unnegotiated_loop_load_delivers_full_history_without_private_fields` + 15 例未协商回放用例 |
| 3 | 无误告知：段读取失败 / 锚点消失 / 回退全量失败 → load 失败（`-32603`），无成功响应 | PASS | P：`streamed_canonical_replay_failure_closes_the_load`、`..._fails_closed_when_the_anchor_vanishes_mid_stream`、`s9_verification_fallback_full_read_failure_still_closes_the_load`；S：HEAD 的 warn+继续 已删除 |
| 4 | 无私有语义：未协商连接通知/响应无 `nextCursor`/`snapshotToken`/`periEntryId`/`periPosition`/`periLoadGeneration` | **PARTIAL** | P：标准客户端场景全部无私有字段；**发现 1 个反例**——会话级 caps 注册表被先前 peri 连接写过后，未协商连接收到 `periReplay`（4 条通知）与 `periLoadGeneration`（响应 + 4 条通知）；分页四项从未泄漏。见 §7.1、§8.1 |
| 5 | 协商路径回归：测试 diff 无放宽/删除断言；相关面实跑 | PASS | P：全量 879 passed / 0 failed；S：逐条核对（§5.5） |
| 6 | 无 DDL、无新依赖 | PASS | P：三条命令输出 0（§5.4） |
| 7 | 反例与残余观察 | 1 个反例（第 4 条）+ 2 项观察（§8.2/§8.3） | P/S |

**结论：§5.3（D7）主目标达成**——标准 ACP 客户端拿到的仍是完整 canonical 回放、读取失败必使 load 失败、分页私有字段按**当前连接**门控；协商路径行为符合 §5.1/§5.2 且无回归。
**唯一未达标点**是第 4 条的边界：`periReplay` / `periLoadGeneration` 仍以**会话级** caps（`SessionManager::ensure_session_caps`）为门，未协商连接在「该会话此前被 peri 连接注册过」时会收到这两个字段（不是 `nextCursor`/`snapshotToken`/`periEntryId`/`periPosition`）。

---

## 1. 范围

### 1.1 覆盖
- `session/load` **未协商**路径：模式分流、canonical 流式回放（`dispatch::replay_canonical_history_streamed`）、失败关闭、wire 输出（通知 + 响应）。
- `session/load` **协商**路径回归：热态有效集合回放、`periEntryId`/`periPosition`/`periLoadGeneration`、`nextCursor`/`snapshotToken` 页模式、`-32601`/`-32602`/`-32011`/`-32012` 错误面。
- 能力协商与**按连接**判定（`initialize` → `commit_initialize` → `ConnectionContext{history_pagination,facts}` → `classify`）。
- 存储支撑：`load_history_segment`（SQLite + Remote）、`history_page_bounds`、`load_entry_positions`、`load_active_snapshot`。
- 构建面：无 DDL、无新依赖。

### 1.2 不覆盖（未验证，见 §9）
- 真实外部 ACP 客户端经 stdio 的端到端（本轮只到 MPSC 真实 `ServerLoop`）。
- Remote 后端在 ACP 层的端到端（只到 `peri-resources` 存储层与契约测试）。
- 客户端侧（TUI/SDK）代次暂存与安装（S7 切片范围）。
- 性能预算（A04/A05/A07），另有 `perf-evidence.md`。

---

## 2. 改动文件清单（D7 直接相关，绝对路径 + 一句话）

> 路径根 = `/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history/`；下表中路径均相对该根。

| 文件 | 一句话 |
|------|--------|
| `peri-acp-types/src/peri_caps.rs` | `PeriCaps` 新增 `history_pagination`（`peri.historyPagination`，缺省 false；`all_enabled()` 含 true） |
| `peri-acp-types/src/history_page.rs`（新） | 分页契约：游标封装/解码、`HistoryPagePlan`（页装配唯一规则）、`HistoryForwardCursor`/`HistorySegment` |
| `peri-acp-types/src/session_resources.rs` | 新增 `load_history_page` / `load_history_segment` / `history_page_bounds` / `load_entry_positions`（默认 `Unsupported`/空 map，失败关闭） |
| `peri-acp/src/host/connection.rs` | 连接级协商事实：`negotiated_caps`、`history_pagination()`（失败关闭）、`ConnectionFacts` |
| `peri-acp/src/host/server_loop.rs` | 请求级取 `ConnectionFacts`；`session/load` 在生命周期锁**之前**分流；协商连接成功后附首游标 |
| `peri-acp/src/host/requests.rs` | 生产入口 `handle_request_for_connection`（收连接事实）；`handle_request` 降为 `#[cfg(test)]` 包装（按未协商处理） |
| `peri-acp/src/host/history_page.rs`（新） | `classify`（InitialLoad / HistoryPage）+ 参数规则 + `handle_page` + `attach_initial_cursor` |
| `peri-acp/src/host/requests/session_restore.rs` | `handle_load` 按 `history_pagination` 分流：协商=热态有效集合+位置戳；未协商=存储 canonical 流式；回放失败 → `-32603`（删除 warn+继续） |
| `peri-acp/src/dispatch/session_replay.rs` | 投影唯一实现 `project_payload_updates`；`ReplayMarks`/`write_entry_marks`（未协商不打戳）；`replay_canonical_history_streamed` + `Unsupported` 显式回退 |
| `peri-acp/src/dispatch/session_load.rs` | `load_active_session_payloads`（有效集合）；`load_history_page`；`initial_history_cursor`；`load_canonical_session_payloads`；错误映射 |
| `peri-resources/src/sessions/sqlite_store/session_data/history.rs` | SQLite 段/页读取（不过滤 `excluded`；`tail` 与行同源；锚点消失 `Expired`） |
| `peri-resources/src/sessions/remote/session_history.rs`、`session_sql.rs`、`session_data.rs` | Remote 后端同契约实现（同一夹具与本地后端对照） |
| `docs/design/peri-acp-protocol.md`、`docs/code-index/*.md` | 协议与索引同步（未协商=标准完整回放；字段语义） |

其余同工作区改动属于前序切片（rewind/compact/fork/TUI），不属于本轮判定范围，未逐条复核。

---

## 3. 改动前 / 改动后行为对照（独立读 `git show HEAD:` 得出）

| 维度 | 改动前（HEAD `5be107603`） | 改动后（工作区） |
|------|---------------------------|------------------|
| `session/load` 参数 | `cursor` / `limit` **完全未处理**：带 `cursor` 的请求被当普通 load 静默执行（正是「降级为初始 load」） | `classify` 在进生命周期路由**之前**分流；未协商带 `cursor`/`limit` → `-32601` + `data.reason=pagination_not_negotiated`；坏游标/非法 `limit`/setup 参数同现 → `-32602`，绝不降级 |
| 历史来源 | `SessionState.history_payloads`（当时由 `load_session_payloads` = `load_session_history` 写入，**全量 canonical**；内存持有全历史） | 未协商：存储侧 `load_history_segment` 逐段流式读 **canonical 完整历史**（含 `excluded` 归档原文、含 System，不读 `state.history_payloads`）；协商：热态**有效集合**（`excluded` 不重放）+ 位置戳 |
| 回放失败语义 | `if let Err(e) = ... { tracing::warn!(... "session/load: history replay failed, continuing") }` → **warn 后照常返回成功响应**（误告知标准全量回放已完成） | 失败即 `Err`→`-32603`；`Unsupported` 只在**未交付任何通知前**回退到全量 canonical 读取（同一投影、同一不打戳标记），回退再失败同样 fail |
| 私有字段 | 无「连接级能力」概念；`periReplay` 按**会话级** caps（`ensure_session_caps`）；无 `periLoadGeneration`/`periEntryId`/`periPosition`/`nextCursor`/`snapshotToken` | 分页四项按**当前连接**的 `ConnectionFacts::history_pagination` 门控（失败关闭：未 initialize 视为未协商）；`periReplay`/`periLoadGeneration` 仍按会话级 `caps.replay`（见 §7.1/§8.1） |
| 会话热态 | `history_payloads` = canonical 全量 | = 有效集合（冷/热恢复同口径；canonical 只从存储按需读） |
| `session/resume` | 不回放历史 | 不回放历史（未改动，D7 无涉） |

---

## 4. 契约与不变量（复核对象声明 + 复核结论）

1. 「标准 `session/load` 完整回放」由**当前连接**的 `initialize` 协商结果决定，且失败关闭（`ConnectionContext::history_pagination()` 要求 `initialized==true` 且有键；`connection_test.rs` 4 例实测）。
2. 未协商连接的交付**不读热态**：`handle_load` 的未协商分支只调 `replay_canonical_history_streamed`；`AlreadyLive` 也一样（实测：我的复核用例走 AlreadyLive，归档原文仍交付）。
3. 空段 ≠ 结束：`load_history_segment` 的默认实现是 `Unsupported`（不是空段）；`next_cursor=None` 才是结束。
4. 首段固定 `tail`（存储序上界），续读不得越界；继承区只在首段返回——由 `history_segment_*` 用例实测。
5. 回放失败必须可见（`-32603`），不得留「半份首屏 + 成功响应」。
6. 分页四项字段只随连接级协商出现，且只在 `session/load` 上；`session/resume` 无历史回放。

---

## 5. 实测证据（命令 + 结果）

> 全部命令以 `cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history &&` 开头；串行执行；仅用 `./scripts/cargo-rmcp-patched.sh`。

### 5.1 编译（实测）
```
./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp-types   → Finished `dev` profile ... in 1.12s
./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp         → Finished `dev` profile ... in 1.03s
./scripts/cargo-rmcp-patched.sh check --locked -p peri-resources   → Finished `dev` profile ... in 0.58s
```

### 5.2 未协商回放面（实测）
```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::requests::tests::unnegotiated_replay_tests
  第 1 次：14 passed; 1 FAILED
    streamed_canonical_replay_falls_back_to_the_full_read_when_the_segment_read_is_unsupported
    panicked at ...:783: "回退必须留下可区分的日志，实际:"（日志捕获为空）
  单独重跑该例：1 passed
  第 2 次（同命令）：15 passed; 0 failed; 0 ignored
```
- 判定：该失败是**测试基建 flake**（并行用例对同一 `tracing` callsite 的兴趣缓存竞争），非产品缺陷；用例内已注明该风险（`capture_logs` 的 `rebuild_interest_cache`）。三份证据：整模块 1 红 → 单跑绿 → 整模块再跑绿。

### 5.3 复核者的独立完整性用例（实测，本轮新增）
```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::requests::tests::history_page_wire_tests::s9_recheck
  running 2 tests
  s9_recheck_unnegotiated_loop_load_delivers_full_history_without_private_fields ... ok
  s9_recheck_session_caps_do_not_leak_peri_private_fields_to_an_unnegotiated_connection ... ignored（反例探针）
  test result: ok. 1 passed; 0 failed; 1 ignored
```
- 用例走**真实 MPSC `ServerLoop`**（不是直接调 `handle_request`）、真实 SQLite 门面、`AlreadyLive` 会话、两条最旧条目被 Full 归档：断言回放文本序列**逐条等于** `load_session_history` 的 canonical 正文序列（条数/顺序/正文），且响应无 `nextCursor`/`snapshotToken`、全部通知与响应无 `periEntryId`/`periPosition`。

### 5.4 无 DDL / 无新依赖（实测）
```
git status --porcelain | grep -E "Cargo\.(toml|lock)" | wc -l                      → 0
git diff --name-only -- '*Cargo.toml' 'Cargo.lock' '*Cargo.lock' | wc -l           → 0
git diff -U0 | grep -iE "^[+-].*(create table|create index|create unique index|alter table|drop table|drop index|add column|drop column)"  → 0 行
git status --porcelain | grep -iE "schema|migration|\.sql"                         → 仅 2 个 *测试* 文件（schema_probe_test.rs / schema_upgrade_test.rs，diff 内容为测试夹具类型适配，非 schema 定义）
```

### 5.5 协商路径回归（实测 + 静态）
```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib
  → test result: ok. 879 passed; 0 failed; 1 ignored; finished in 266.26s
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::requests::tests::history_page_wire_tests
  → 8 passed; 0 failed; 1 ignored（含 A08/A09/A15/A20/A21/A22/A25 与"回放失败不给成功 cursor"）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::history_page            → 10 passed
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- dispatch::session_replay      → 12 passed
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::connection              → 4 passed
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib                            → 576 passed
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- session_history         → 16 passed（Remote 页/段/回退边界）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- history_segment_tests   → 8 passed（含归档/System 行、tail 越界、锚点消失）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- history_page_tests      → 15 passed（本地 + Remote 同夹具对照）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --test session_resources_contract → 7 passed
```
测试文件 diff 逐条核对（静态，`git diff -U0 -- '*_test.rs' 'peri-resources/tests/*.rs'`，删除行共 50 行）：
1. `peri-acp/src/dispatch/session_fork_test.rs`：删除 `assert_eq!(copied_payloads.len(), source_payloads.len());` → 换为带说明的三参版本，并新增 `fork_publishes_effective_payloads_and_keeps_released_bodies_in_storage`（**收紧**，非放宽）。
2. `peri-resources/src/sessions/remote/schema_upgrade_test.rs`：删除 `sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_owned()))` → 改为 `AssertSqlSafe(sql)`（测试替身字段类型适配，非断言）。
3. 其余 48 行删除均为 import 重排、签名适配（`marks` 参数）与 fixture 字段新增（`structure_version: 0` 等）。

### 5.6 失败路径实测（判据 3）
```
running 15 tests ... ok（模块第 2 次运行）
- streamed_canonical_replay_failure_closes_the_load          → load Err(-32603)、0 条通知、不改存储
- streamed_canonical_replay_fails_closed_when_the_anchor_vanishes_mid_stream → 首段成功、续读失败 → load Err；半份通知存在但**无成功响应**
- s9_verification_fallback_full_read_failure_still_closes_the_load → Unsupported 回退 + 全量读取失败 → load Err
- history_page_wire_tests::a_failed_initial_replay_fails_the_load_instead_of_answering_a_cursor → 真实通知通道失败 → load 失败、不给游标
```

---

## 6. 复核者新增的用例（标注）

文件：`peri-acp/src/host/history_page_wire_test.rs`（本切片新增文件；我的追加在文件末尾「独立复核（S9-recheck）」段）

1. `s9_recheck_unnegotiated_loop_load_delivers_full_history_without_private_fields`（**常规运行**，已通过）
   - 真实 `ServerLoop` + 未协商连接的 `session/load`；夹具含 2 条 Full 归档行；断言回放正文序列 == `load_session_history` 正文序列、响应无分页字段、wire 无四项分页私有字段。
2. `s9_recheck_session_caps_do_not_leak_peri_private_fields_to_an_unnegotiated_connection`（**`#[ignore]` 反例探针**，根因见 §8.1）
   - 同样走真实 `ServerLoop`，但把会话级 caps 注册表预置为 `{replay:true, history_pagination:true}`（模拟「这条会话此前被 peri 连接写过」）；断言未协商连接不得收到任何私有字段。
   - 现状：`--ignored` 运行**失败**，命中 `["periLoadGeneration x5", "periReplay x4"]`（复现命令见 §7.1）。保留为忽略用例是为了让主线测试保持绿色，同时把反例固化为可复现证据。

---

## 7. 反例（最小复现与影响面）

### 7.1 会话级 caps 泄漏 `periReplay` / `periLoadGeneration` 到未协商连接

- 复现命令：
```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- --ignored host::requests::tests::history_page_wire_tests::s9_recheck_session_caps
→ panicked: 未协商分页的连接不得继承会话级 caps 带来的私有字段；实际命中: ["periLoadGeneration x5", "periReplay x4"]
```
- 机制（静态）：`handle_load` 里 `let caps = cfg.session_manager.ensure_session_caps(req_session_id);`（`peri-acp/src/host/requests/session_restore.rs:527`）→ 命中 `caps_registry` 旧值（`peri-acp/src/session/caps.rs:82`）；`dispatch::replay_meta` 与 `TuiReplaySender::send_system_reminder` 都以 `caps.replay` 为门写 `periReplay` / `periLoadGeneration`（`session_replay.rs:596`、`session_restore.rs:814`）。
- 可达路径：同一进程内，peri 客户端（TUI，声明 peri caps）先创建/加载过该会话 → 标准 ACP 客户端（`initialize` 不带 peri 扩展）再 `session/load` 同一会话；或注册表从未被清空的重连场景。这正是 §5.3 第二段「不能继承会话内保存的旧客户端能力」所指的形状。
- 泄漏的只有回放标记两字段；**分页四项（`nextCursor`/`snapshotToken`/`periEntryId`/`periPosition`）在所有实测场景都未泄漏**（它们只由连接事实决定）。
- 影响面：标准客户端的响应 `_meta` 与 4 条回放通知多出未知键。按 ACP 约定 `_meta` 是扩展空间，合规客户端应忽略；不会造成正文丢失、也不会造成「误告知成功」。但它是「未协商连接收到 Peri 私有语义」的实例，且 `periLoadGeneration` 恰在 §5.2 的扩展字段表与 A25 的复核口径内。
- 归属：`periReplay` 的会话级门在 HEAD 已存在（本切片未改）；`periLoadGeneration` 是本轮工作区新增字段，搭在同一门上。**未自行修复**（超出「一行内笔误」范围）。

---

## 8. 残余观察与风险

1. **（观察）`Unsupported` 回退 = 非段能力后端退回全量物化**：`replay_canonical_history_streamed` 在首段读取报 `Unsupported` 时回退 `load_session_history` 全量读取（`session_replay.rs:336-345`）。两个生产后端（SQLite/Remote）都实现了段读取，回退只对第三方/桩后端生效；它有 `tracing::warn!` 与显式注释，不是静默降级，且交付内容仍完整——但「有界读取」的保证在这条路径上不成立。风险等级：低（无生产后端命中），但若未来新增后端未实现段读取，只能靠一条 warn 发现。
2. **（观察）`load_entry_positions` 默认返回空 map**（`session_resources.rs:808-813`）：未覆写的后端会让协商连接的初始回放只打 `periEntryId` 不打 `periPosition`，而不是失败。两个生产后端均已覆写；无运行时强制手段。风险等级：低。
3. **（flaky）新增日志断言用例在并行执行下不稳定**：`streamed_canonical_replay_falls_back_to_the_full_read_when_the_segment_read_is_unsupported` 在第 1 次模块运行失败、单跑通过、第 2 次模块运行通过（§5.2）。属测试基建问题（callsite 兴趣缓存），建议后续把日志捕获改为串行或改用 `#[serial]`/专用订阅器；未自行修改。
4. `peri-resources` 全量 lib 未在本轮重跑（只跑定向模块 + 契约测试 + `check`）；上一轮记录过并行负载 flake，本轮未复现。

---

## 9. 未验证与不确定项

1. 外部真实 ACP 客户端（stdio）端到端：未跑；只到 MPSC 真实 `ServerLoop`。
2. Remote/云端后端在 ACP 层的端到端（跨网络一致性、错误映射）：未跑；只到 `peri-resources` 存储层与两端同夹具对照。
3. 客户端侧（TUI/SDK）对未协商连接「完整回放 + 无游标」与失败代次的处理：未跑（S7 切片）。
4. 性能判据（A04/A05/A06/A07 的字节预算与内存上界）：未跑，见 `perf-evidence.md`。
5. 我未复核前序切片（rewind/compact/fork/agent/TUI）的正确性，仅复核其对 D7 判定路径的影响面（未发现影响）。
6. 反例 §7.1 的修复方案与兼容性代价：未评估（按要求不自行修复）。

---

## 10. 结论

- D7 三条主判据（完整 / 无误告知 / 协商路径不变）**实测达成**：未协商连接经真实 ServerLoop 交付完整 canonical 历史（含 Full 归档原文）且读取失败必失败；分页私有字段按当前连接门控；协商路径 879 例全量通过。
- 第 4 条判定为 **PARTIAL**：发现 1 个可复现的会话级 caps 泄漏（`periReplay` / `periLoadGeneration` 落到未协商连接），已固化为 `#[ignore]` 反例探针并给出最小复现；分页四项字段无泄漏。
- 无 DDL、无新依赖；测试 diff 未见断言放宽。
