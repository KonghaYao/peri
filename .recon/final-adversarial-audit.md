# 终局对抗审计报告（独立审计者）

- 审计者：独立对抗审计（不复述既有复核结论，默认只读、不修改生产代码）
- 工作区：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`（分支 `feat/transcript-shrink-history-pagination`，改动未提交）
- 需求：`/Users/konghayao/Desktop/peri-transcript-history-requirements.md`（§9 验收 A01–A25，§5.3 标准边界）
- 蓝图/决策：`spec/issues/2026-10-13-transcript-shrink-history-pagination.md`（D1–D14）
- 审计日期：2026-10-13/14
- 证据纪律：每条结论标注 **实测**（命令 + 输出）或 **静态**（file:line）。未跑的写「未验证」。
- 本报告不修改生产代码；审计者新增的探针文件见 §6。

---

## 0. 结论速览

| # | 被审计断言 | 判定 | 关键证据类型 |
| --- | --- | --- | --- |
| 1 | 无 DDL / 无新依赖 / 无迁移 / 无双写 / 无兜底 | **成立** | 实测（git diff 枚举）+ 静态（读取走既有列） |
| 2 | 失败关闭：游标形式错 / 跨会话 / 锚点被删 / 结构版本过期 / 分页带 setup / 未协商带 cursor 均明确拒绝，不降级为初始 load | **成立** | 实测（classify 10 + wire 14 + 装配 19 + 存储 15/16，全绿；含 wire 层「拒绝不得触发一次初始 load」断言） |
| 3 | 未协商标准客户端初始 load 交付完整 canonical 历史、且不含任何 Peri 私有字段 | **不成立（部分）** | 实测 6 字段与 canonical 完整回放成立；**反例 F2**：未协商回放通道仍收到 `_meta.peri.subagentFailure` |
| 4 | SQLite 与 Remote：初始有效视图 / 分页顺序 / 游标失效 / 受众过滤一致 | **成立（本机范围）**，云后端未验证 | 实测（本机对照 6 + full_remote 1 + 受众规则 1，全绿）；21 条 cloud `#[ignore]` 未跑 |
| 5 | 收缩不变量：失败/取消不留「原文已退出但摘要不可恢复」；Full 期间新输入不被旧边界移除 | **成立（有缺口）** | 实测 22 条（compact_cancel 6 + transcript 2 + full_continuation 11 + 原子性 1 + 只读后端 1 + 到达 1）；A03 的**用户输入**形态无专门用例，列为证据不足 |
| 6 | 重复/幂等：相同 cursor+limit 重试等价、追加+再次 Full 后旧页等价、delivery 重投只有一份 | **成立** | 实测 A08/A09 wire 2 条 + A18 4 条 |
| 7 | 前端：Compact 不强制 reload、窗口与 cursor 进度保留、历史投影不推进实时状态 | **部分成立** | 引用 S7 报告（S7d FAIL → S7e PASS，含已声明边界）+ SDK 用例名；未达成项见 §3.7 |
| 8 | 未验证/证据不足清单 | 见 §4 | — |

**两个缺陷（审计者新发现，均已实测复现）**：

1. **F1（P1）**：初始游标锚点取「有效视图最旧一条 own payload」，当这条是**永不被 Full 排除的 System 条目**（真实来源：subagent/fork 会话的身份条目）时，`nextCursor = null` 或锚点之后的被排除原文永不交付 → 归档不可达。
2. **F2（P2，口径相关）**：未协商的 ACP 客户端在 `session/load` 回放中收到 `_meta.peri.subagentFailure`（含子线程 ID、provider、request_id、错误详情）。六个受门控字段之外的第 7 个私有字段，蓝图/S9c 已声明为「范围外」，但按本次审计口径（任何 Peri 私有字段）不成立。

---

## 1. F1：初始游标锚点缺陷（P1，实测复现）

### 1.1 断言与判定

- 断言（需求 REQ-H01-1/4/7、A15、A20、A06）：首次 cursor 从「本次有效视图与压缩覆盖边界」派生；首屏与历史页并集应覆盖快照范围内可访问历史；只有确实没有更早可展示内容才返回 `nextCursor=null`。
- 判定：**不成立**。锚点规则在「有效视图最旧 own payload 是不可展示且不被 Full 排除的 System 条目」时失效。

### 1.2 根因（静态）

1. `peri-acp/src/dispatch/session_load.rs:115-120`：
   - `let anchor = effective.payloads.first().map(PersistedPayload::id);`
   - `next_cursor = (bounds.range_head != Some(anchor)).then(...)`
   - 锚点不检查「可展示性」，也不检查「锚点是否位于被排除归档区间之后」。
2. System 条目永不进入排除集合：`peri-agent/src/agent/compact_v2/full.rs:86-101` 的 flag 采集显式 `.filter(|entry| !matches!(entry.as_message(), Some(BaseMessage::System { .. })))`；`peri-agent/src/session/transcript.rs:813` 收缩同样保留 System。
3. System own 条目的真实生产来源：`peri-agent/src/session/subagent/factory/spawn.rs:348-352`（`guard.append(BaseMessage::system(identity))`）——subagent / fork / bg 子会话在 own 区**第一条**写入身份 System。全仓 grep：这是唯一把 System append 进生产 transcript 的路径。
4. 存储侧 `rowid` 语义：游标从锚点**向前（更早 rowid）**前进（`HISTORY_PAGE_ROWS_SQL`：`rowid < (子查询按 message_id)`），因此锚点之后、摘要之前的被排除原文不可达。

### 1.3 实测证据

命令（探针文件 `peri-resources/tests/zz_audit_history_anchor_probe.rs`，审计者新增）：

```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --test zz_audit_history_anchor_probe
→ running 3 tests ... 3 passed; 0 failed
```

探针 1 `zz_probe_system_entry_at_own_head_hides_the_whole_archive`（对应真实 subagent 形状：own = `[System(identity), h1, h2]`，Full 排除 h1/h2 并追加摘要）：

- 断言通过：`active = [System, 摘要]`、`anchor = <System>`、`range_head = <System>`、`next_cursor = None`；
- 同时断言存储里 `h1/h2` 原文可读（`load_history_page(None)` 返回 ≥2 条归档）→ **初始 load 给 `nextCursor: null`，但存储里有归档**。

探针 2 `zz_probe_archive_rows_newer_than_the_anchor_are_unreachable`（更一般形态：own = `[hold, System, hnew]`，排除 hold+hnew）：锚点=System，页面从 System 向前只交付 `hold`；**位于锚点之后的被排除原文 `hnew` 永不交付**（双向缺口）。

探针 3 `zz_probe_control_without_system_entry_pages_the_archive`（对照：无 System）：`anchor = <摘要> ≠ range_head`，`next_cursor = Some(...)`、正常翻页 → 证明缺陷与 System 条目存在性直接相关，不是夹具问题。

另有 peri-acp 层探针（`peri-acp/src/host/history_page_wire_test.rs` 末尾，审计者新增）：

```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::history_page_wire
→ 14 passed; 0 failed（含 zz_audit_initial_cursor_skips_archive_when_the_oldest_effective_own_row_is_system）
```

该探针经真实 `initial_history_cursor` API 断言 `initial.next_cursor.is_none()` 而 `load_history_page(None)` 返回 ≥2 条归档。

### 1.4 影响面

1. **subagent / fork / bg 子会话**（凡经 `spawn.rs` 写入身份 System 的会话）在发生一次 Full Compact 后：初始 load 的 `nextCursor` 为 `null`，客户端（TUI/SDK）不再请求任何历史页 → **该会话的全部被压缩原文在 UI 上不可达**；同时这是需求明令禁止的「用结束/空页掩盖」与 §5.2「不降级」同向的静默错误。
2. 主会话（无 System own 条目）：按当前代码路径不受影响（静态推断；全仓仅 spawn.rs 一处 append System）。若后续新增「会话级 System 条目」来源，缺陷面会扩大到主会话。
3. 若锚点 System 不在 own 区开头（构造形态，见探针 2），缺口是双向的：锚点之后、摘要之前的被排除原文永不交付 —— 即便 `nextCursor` 非 null 也会漏内容，违反 REQ-H01-4「除重试外，不重叠、不遗漏」。
4. 关联验收：A15（nextCursor 语义）、A20（稀疏首屏逐页合并）、A06（20 页遍历无遗漏）在 System 锚点会话上不成立。

### 1.5 最小复现（可复制）

```
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --test zz_audit_history_anchor_probe -- --nocapture
```

输出可见 `active=[...] anchor=<System> range_head=<System> next_cursor=None` 与 `storage-first-page=[...]` 并存。

### 1.6 处置与边界

- 按用户指令「不自行修」，审计者未改生产代码；仅新增探针复现。
- 修复方向（供参考，不属本次实施）：锚点应取「有效视图中最旧的**可展示**条目，且其位置必须在压缩覆盖边界之后」，或直接以「压缩覆盖边界（被排除区间的末尾/摘要前一条的 rowid 上界）」为游标起点；并补一条「System own 条目 + Full」的回归用例。

---

## 2. F2：未协商回放通道泄漏 `_meta.peri.subagentFailure`（P2，口径相关，实测复现）

### 2.1 断言与判定

- 需求 §5.3 原文：「未协商的通用 ACP 客户端仍使用标准完整回放投影，不得收到**分页私有语义**并被误告知已完成标准全量回放」；A25：「标准客户端仍遵守完整回放契约」。
- 本次审计口径（用户指令）：未协商连接初始 load「**不含任何 Peri 私有字段**——不只是分页四项，`periReplay`/`periLoadGeneration` 也在内」。
- 判定：
  - 按需求 §5.3 **字面**（分页私有语义 + 完整回放契约）：**不算违反**（canonical 正文完整，泄漏字段不是分页语义）；
  - 按本次审计口径（任何 Peri 私有字段）：**不成立**——`_meta.peri.subagentFailure` 是第 7 个私有字段，且出现在**回放通道**（session/load 的通知），不是仅 live 通道。

### 2.2 根因（静态）

1. `peri-acp/src/event/tool_projection.rs:33-37`：`project_tool_completion` 在 `failure: Some(_)` 时**无条件**写 `_meta.peri.subagentFailure`，不看连接 caps。
2. `peri-acp/src/dispatch/session_replay.rs:552-575`：回放的 `BaseMessage::Tool` 分支把 `subagent_failure.as_ref()` 原样传入同一投影函数。
3. `peri-acp/src/dispatch/session_replay.rs:635-646`：`replay_tool_update` 只在 `replay_meta(caps, marks)` 有值（`caps.replay == true`）时 **extend**；caps 关闭时**只做加法、不做剔除**，已写入的 `peri` 键保留在 wire 上。

### 2.3 实测证据

探针（已跑通；探针源码与输出留档，文件本身已按审计一次性用途删除，见 §6）：

```
.recon/zz-probe-subagent-failure-output.txt
→ test host::requests::tests::zz_adversarial_probe_tests::zz_probe_unnegotiated_load_wire_fields ... FAILED
  反例：未协商连接收到 Peri 私有字段 ["peri", "subagentFailure"]（六字段命中 = []）
  offending _meta = {"peri":{"subagentFailure":{"child_thread_id":"child-1","category":"http_status",
      "message":"HTTP 429, request id: req-1, provider: provider.example", ...}}}
→ 对照 zz_probe_negotiated_load_wire_fields ... ok（协商连接出现 periReplay 等，证明夹具非空转）
```

场景：真实 `ServerLoop` + MPSC；标准 `initialize`（仅 `clientCapabilities.fs`）→ `session/load`；夹具历史里有一条带 `subagent_failure` 的 Tool 结果（`SafeSubagentFailure::new`，非 LLM 调用）。

关键结论：**六个受门控字段（`periReplay`/`periLoadGeneration`/`nextCursor`/`snapshotToken`/`periEntryId`/`periPosition`）命中为 0**，说明 S9c 的门控修复有效；但 `_meta.peri` 命名空间本身无整体门控。

### 2.4 影响面与既有记录

1. 影响：未协商客户端会看到 Peri 私有工具失败诊断（含 child_thread_id / provider / request_id / 错误文本），属 §5.3「不得收到 Peri 私有语义」的扩展解释；不泄漏分页语义，不影响完整回放与游标。
2. 既有记录（避免误判为新回归）：`.recon/S9c-verification.md:153` 已静态定位该字段「回放与 live 两条路都无条件发射」，判定为「前序切片既有行为、不在本任务点名的 6 字段内、范围外」；本次增量是**实测**（真实 ServerLoop 的未协商 load 回放确实收到）与「它在回放通道而非仅 live 通道」的确认。
3. 用户已点名的另外两类范围外（蓝图 D12 / S9c §4.3，本次未新增实测，属引用）：
   - 命令表广播面（`send_available_commands_update` / `session/resume`）仍用会话级 caps 注册表（`session_restore.rs:541/637-643`）；
   - live 通道 reminder 形状仍读会话级注册表（`session/event_sink.rs:180-197`），未协商连接在某会话被 peri 连接注册过后可能收到结构化 `system-reminder`。
4. 处置边界：审计者未修（非一行笔误）。最小修复方向：在 `replay_tool_update`/回放出口按连接 caps **剔除** `peri` 键（或在未协商路径改用不带 `failure` 的投影）。

---

## 3. 必查八条逐条

### 3.1 必查 1：无 DDL / 无新依赖 / 无迁移 / 无双写 / 无兜底

**结论：成立。**

实测 1（依赖与 schema 变更枚举）：

```
git status --porcelain=v1 | grep -Ei 'Cargo\.(toml|lock)|package\.json|bun\.lock|\.sql$|schema|migrations?'
→ 只命中 peri-resources/src/sessions/remote/schema_probe_test.rs、schema_upgrade_test.rs（测试文件，本身无 schema 定义变更）
git diff --name-only | grep -Ei 'Cargo\.(toml|lock)|package\.json|bun\.lock'   # exit 1，无命中
```

实测 2（新增行中的 DDL token 扫描）：

```
git diff -U0 | grep '^+' | grep -Ei 'CREATE TABLE|ALTER TABLE|ADD COLUMN|DROP TABLE|CREATE INDEX|user_version'
→ 唯一命中：peri-resources/src/sessions/remote/full_remote_test.rs（新增测试夹具，用既有 session_schema::initialization_plan() 建库）
→ 未跟踪新文件（含 15 个新 .rs）中无任何 DDL token
```

静态 3（读取走既有列）：历史页三条语句只使用既有 `messages` 表的 `thread_id`/`message_id`/`rowid`/`content`/`excluded` 列（`peri-resources/src/sessions/sqlite_store/session_data/history.rs`），无新列、无新表、无迁移分支；`read_active_snapshot`/`read_history_page` 都是纯 SELECT。

静态 4（无兼容层/双写/兜底）：新代码未引入「旧路径 fallback」；`resolve_browsing_range` 无兜底分支（任一校验失败即 `Expired`）；`InitialHistoryCursor` 无「游标不可用则退回全量」的分支。

**缺口（如实记录）**：本项只证明「本切片未引入 DDL/依赖」；不证明既有 schema 定义（`session_schema::initialization_plan()`）中的版本常量与云后端一致（云用例未跑，见 §4）。

### 3.2 必查 2：失败关闭（不降级为初始 load）

**结论：成立（本机范围内）。**

实测（全部命令原样，输出为通过摘要）：

| 层级 | 命令（前缀 `./scripts/cargo-rmcp-patched.sh test --locked`） | 结果 | 覆盖点 |
| --- | --- | --- | --- |
| ACP 参数层 | `-p peri-acp --lib -- host::history_page` | 10 passed / 0 failed | cursor 缺失=初始 load、`limit` 单独出现拒绝、未协商带 cursor 拒绝、malformed 形态拒绝、结构版本过期（`a_cursor_from_an_earlier_structure_version_expires_before_storage`）、loadGeneration 规则、无 sessionId 拒绝、跨会话拒绝、limit 越界/非整数拒绝、分页带 setup 参数拒绝 |
| ACP wire 层 | `-p peri-acp --lib -- history_page_wire` | 14 passed / 0 failed | 未协商 + cursor → `-32601` 且 **`history.len()` 不变（拒绝不得触发一次初始 load）**；首屏失败不冒充分页可用 |
| 装配层 | `-p peri-acp-types --lib -- history_page` | 19 passed / 0 failed | 游标四类失败模式可区分；锚点/range_head 消失即 `Expired`；段证据缺失即失败关闭（不冒充结束）；零 limit 失败关闭 |
| SQLite 存储 | `-p peri-resources --lib -- history_page_tests` | 15 passed / 0 failed | 锚点被删 / range_head 变化 → `Expired`；跨会话拒绝；非展示行推进位置不假终点 |
| Remote 存储 | `-p peri-resources --lib -- session_history` | 16 passed / 0 failed | 锚点被删 → `Expired`（与 SQLite 同语义）；rewind 未知边界拒绝 |

静态补充：错误码映射 `peri-acp/src/dispatch/session_load.rs:128-152`（Malformed→`-32602`、CrossSession→`-32011`、Expired→`-32012`、其他→`-32603`）；跨会话在 `HistoryCursorEnvelope::decode` 用会话归属校验，不依赖存储。

**缺口**：`-32601` 的「不降级」断言只看 `sessions` 里 history 长度不变（wire 用例），未断言「响应体不含首屏通知」的完整清单（用例已断言无回放通知吗？见 `pagination_fields_from_an_unnegotiated_connection_are_rejected` 仅断言 error 与 history 长度）。此处为**证据强度边界**，非缺陷证据。

### 3.3 必查 3：§5.3 / A25 标准客户端完整回放

**结论：六字段与 canonical 完整回放成立；「不含任何 Peri 私有字段」的加严口径不成立（F2）。**

成立部分（实测）：

| 命令 | 结果 | 断言要点 |
| --- | --- | --- |
| `-p peri-acp --lib -- history_page_wire`（含 5 条 s9/s9c） | 14 passed | ① 未协商 loop load：文本序列 == canonical 全量、响应无 `nextCursor`/`snapshotToken`、通知无 6 私有字段；② 会话级 caps 注册表被预置 `pagination_caps()`（D12 成因）后仍不泄漏 6 字段；③ 真实 `initialize`（无 peri 扩展）→ 无私有字段；④ 真实 `initialize`（带 peri 扩展）→ 回放字段齐全；⑤ 从未 `initialize` 的进程内连接 → `PeriCaps::all_enabled()` 兜底，仍有 `periReplay`/`periLoadGeneration`、无分页四字段 |
| 同一批用例中的 canonical 断言 | 同上 | 未协商 load 的 `texts == canonical`（含 Full 归档原文），证明「完整回放」成立 |

不成立部分：**F2**（§2）——第 7 个私有字段 `_meta.peri.subagentFailure` 在未协商回放通道出现，6 字段命中为 0。

补充事实（静态 + 实测观察）：协商连接额外出现 `peri.sessionWorkspaceV1`（workspace 相关键，探针输出 `.recon/zz-probe-subagent-failure-output.txt:15`）；未协商连接输出中**未出现**该键。该键不在 6 字段清单内，其门控状态不由本次审计覆盖（未定位发射点，列为未验证）。

另注：TUI/SDK 显式声明 `peri.replay: true` 与 `peri.historyPagination: true`（`npm-packages/@peri-sdk/src/agent/session.ts:166`；TUI `peri-tui/src/acp_client/client/requests.rs:66` 以 `PeriCaps::all_enabled()` 为基座初始化），因此 Peri 客户端走协商路径，而非未协商回放路径。

### 3.4 必查 4：SQLite 与 Remote 一致性（A13）

**结论：成立（本机范围）；云后端未验证。**

实测（本机，两条后端对照/同源夹具）：

| 命令 | 结果 | 一致性证据 |
| --- | --- | --- |
| `-p peri-resources --lib -- history_page_tests` | 15 passed | `the_local_backend_agrees_with_the_shared_page_assembly_on_the_same_rows`（本地后端与共享装配同结果） |
| 同上 | — | `bounds_answer_the_range_head_and_agree_with_the_first_page_cursor`（边界与首页游标互证） |
| `-p peri-resources --lib -- session_history` | 16 passed | `remote_pages_the_same_fixture_as_the_local_backend`、`remote_history_segment_replay_matches_the_local_backend`、`remote_bounds_answer_the_range_head_and_empty_sessions_have_none`、`remote_history_segment_returns_archived_and_system_rows` |
| `-p peri-resources --lib -- remote_active_snapshot` | 1 passed | `remote_active_snapshot_and_bounded_flags_filter_at_the_storage_layer`（有效视图/排除过滤在存储层同语义，SQLite 扮演远端服务端） |
| `-p peri-acp-types --lib -- history_page` | 19 passed | `client_visibility_follows_the_existing_egress_rule`（受众过滤）；游标失效语义由共享装配决定（唯一权威） |

静态：两后端共用 `HistoryPagePlan::plan/materialize`、`resolve_browsing_range`；差异只在 SQL 方言与传输（`remote/session_history.rs`）。

**未验证**：21 条 cloud `#[ignore]` 用例（需 `.env` 授权库，`-p peri-resources --lib` 中 26 ignored 的一部分）——A13 的 Remote 侧只由上述本机 remote 单测覆盖，**未在真实云库上验证**。

### 3.5 必查 5：收缩不变量（A02 / A03）

**结论：成立（有缺口：A03 用户输入形态无专门用例）。**

实测（串行）：

| 命令 | 结果 | 断言要点 |
| --- | --- | --- |
| `-p peri-agent --lib -- compact_cancel` | 6 passed | 提交前取消/失败：历史保留、无 store 提交；提交后 SQL 已落/确认丢失/错误后：要求 reload、摘要保留（`assert_durable_summary`）；取消后恢复 canonical payloads；提交确认后 reload 错误失败关闭 |
| `-p peri-agent --lib -- test_full_commit` | 2 passed | `test_full_commit_releases_excluded_own_entries`：**容器容量按保留数重建**（`entries.capacity() == kept.len()`、`id_index.capacity()` 下降、`flags.capacity() < released`），保留祖先/System/未排除条目与顺序，释放 id 的 entries/id_index/flags 三处都不留痕（A01 的容器容量不变量）；`test_full_commit_keeps_excluded_system_entry` |
| `-p peri-agent --lib -- full_continuation` | 11 passed | provider 失败保留历史；预算耗尽不提交；未闭合 end_turn 拒绝；regeneration 语义 |
| `-p peri-agent --lib -- test_commit_compaction_lifecycle` | 1 passed | 提交原子或不确定态（不产生「原文退出但摘要不可恢复」） |
| `-p peri-agent --lib -- history_read_only_backend` | 1 passed | 只读后端：内存与 store 均不变（失败关闭） |
| `-p peri-agent --lib -- arriving_during` | 1 passed | `test_full_report_arriving_during_summary_stays_available`：摘要期间到达的新报告不在旧快照排除集合内；旧报告释放、新报告可见且未被标记 excluded（A03 后台结果形态） |
| `-p peri-acp --lib -- rewind` | 69 passed | 含 `test_rewind_expires_previously_issued_history_cursors`、`test_rewind_rejects_stale_preview_without_mutating_history`（A19 的旧 cursor 失效） |

静态：收缩位置在「store 提交成功后、`mark_committed` 之前」（`peri-agent/src/session/transcript.rs:734-853`）；`shrink_after_full_commit` 早退条件 `ancestor_len >= entries.len()`，只释放 `excluded && !System` 的 own 条目。

**缺口（证据不足）**：

1. A03 的「**用户输入**在 Full 期间到达」无专门用例；当前实测是 `SystemReminder`（报告）形态。是否有同一 `append_messages_to_transcript` 路径覆盖 Human？需补一条 Human 形态用例才能算完整覆盖（本次未做，属未验证）。
2. 「Full 期间到达的后台结果不被**重复注入**」：`arriving_during` 用例断言不重复吗？用例只断言新报告可见且未 excluded、旧报告释放；**未断言「不重复注入」的计数**（需看更细的 delivery 三态用例，A18 部分覆盖）。列为证据不足。

### 3.6 必查 6：重复 / 幂等（A08 / A09 / A18）

**结论：成立。**

实测：

| 命令 | 结果 | 断言要点 |
| --- | --- | --- |
| `-p peri-acp --lib -- history_page_wire` | 14 passed | `the_same_cursor_replays_equivalent_pages_and_limit_only_changes_page_boundaries`（相同 cursor+limit 多次重试等价；不同 limit 只改页边界，逻辑遍历顺序不变）；`a_cursor_survives_appends_and_another_full_without_moving_its_page_members`（追加消息 + 再次 Full 后旧页成员、顺序、nextCursor 等价；不按新 flags 改变成员） |
| `-p peri-agent --lib -- stable_terminal_delivery` | 1 passed | 稳定 delivery id 跨 receive 运行只记一次 |
| `-p peri-agent --lib -- durable_terminal_delivery` | 1 passed | 顺序保持 + 排除后去重（不重投） |
| `-p peri-agent --lib -- committed_delivery` | 2 passed | 已提交但热态缺失 → 补装不重发；已退出有效上下文 → 不补装不重发（A18 三态） |

**缺口**：A18 的「冷恢复后重投」在 delivery 用例中覆盖的是 receive 热/冷状态转换（`cold_terminal_restore_tracks_pending_delivery_without_started_event` 等，属 `async_tasks` 面，未由本审计逐条复跑，见 §4 未验证清单）。SDK 侧「相同 cursor 并发合并」由 `history-pagination.test.ts`（`concurrent calls with the same cursor and limit share one request`）覆盖，**未由本审计复跑**（引用 S10 矩阵）。

### 3.7 必查 7：前端（A12 / A23 / A24，以 S7 报告与代码为准）

**结论：部分成立；未达成项如下。**

1. **已达成（引用 S7 证据链）**：
   - TUI「Compact 不再强制完整 reload」：S7c D8 转 PASS（复核者在自行修复编译错误后，断言指向自有通道）；滚动触发 D1（6 个新调用点）、迟到页丢弃 D2、首屏 reminder 区间 D3、工具终点错配 D4 均 PASS（含证伪）；页 RPC 超时 D5 PASS（虚拟时钟 `start_paused`，无证伪步骤）。
   - A20 锚点：S7d 判 FAIL（跨层槽位坐标混用），S7e 复测 PASS（判据 1/2 四形态 + 场景 A/B + 拆组/折叠组，锚点单元屏幕行逐一不变；短路补偿后三条用例全 FAIL 的证伪成立）。
   - SDK：S7c 的 8/8 判据 PASS（真实 Compact 失败路径的失败展示、`compactionStatus` 离开 running、跨路径块身份统一 `blockId === "h:{messageId}:text:0"`、A22 暂存/丢弃/重试一次安装）。
2. **未达成 / 已声明边界（必须如实列出）**：
   - **TUI A20 的两条边界**（S7e 判据 8 的限定条件）：锚点单元**自身**被并入新折叠组、或锚点身份是折叠组而该组被拆开时，`relocate` 返回 `None`（不补偿，安全方向）；实测一种形态位移 0 行、另一种 +4 行。实现者已在模块文档声明；若把「阅读位置不跳」读成无条件，则此项不成立。
   - **SDK P3 边界**（S7c §2.1）：`compact_started` **之前** EarlyReturn 的真实失败（含空历史 `/compact` 返回 "no history to compact"）在 SDK 侧仍无任何失败展示（非本轮引入、相对 HEAD 非回归）；若把 REQ-C02「Compact 失败只更新失败展示」读成「任何失败都要有展示」，这一条不成立。
   - **SDK 文档边界**（S7c P3）：README 未记录上述两条边界。
   - **TUI D7 PARTIAL**：`docs/verification/tui-integration-cases/` 集成用例登记未完成（S7c 记录 code-index 已同步、集成用例未登记）。本次审计未复核该文档现状。
   - **TUI 证据文件与交付状态不对应**（S7c §7 发现，已由复核者修正）：`.recon/S7c-raw-tui-pass-tail.txt` 记录的成功轮不在当前文件内容下运行（当时编译不过）；S7 最终数值以复核者实跑（1807 passed）为准。该问题属「实现者自述证据不可信」，已记录。
3. **审计者未做的部分**：TUI 渲染帧 / 整组件树 / 人工用例（`docs/verification/tui-integration-cases/`）、SDK 测试复跑 —— 均未由本审计执行（见 §4）。

### 3.8 必查 8：未验证 / 证据不足清单

见 §4.4。原则：宁多勿少，不用「应该没问题」收尾。

---

## 4. 验收条件 A01–A25 逐条证据状态

标记：「实测」= 本次审计跑过命令；「引用」= 有既有测试/报告但本次未复跑；「未验证」= 无充分证据。

| 编号 | 状态 | 证据 / 缺口 |
| --- | --- | --- |
| A01 | 部分实测 | 实测：`test_full_commit_releases_excluded_own_entries`（entries/id_index/flags 容量按保留数重建、被释放 id 三处不留痕）。缺口：跨 turn 引用计数、100 次连续 Full 的累计增长未复测（`history_perf_tests` 5 条 `#[ignore]`）。 |
| A02 | 实测 | `compact_cancel` 6 条（提交前/后、确认丢失、错误、取消恢复）+ `full_continuation` 11 条 + `test_commit_compaction_lifecycle`。后续 turn 取消路径由 compact_cancel 的 pre/after 组合覆盖。 |
| A03 | 部分实测（缺口 2 处） | 实测：`test_full_report_arriving_during_summary_stays_available`（报告形态）。缺口：① 用户输入（Human）形态无专门用例；② 未断言「不重复注入」的计数。 |
| A04 | 未验证（引用历史证据） | `history_perf_tests` 5 条 `#[ignore]` 未跑；被排除 own 正文读取为 0 条、查询计划扫描量只有 `.recon/perf-evidence.md` 历史证据（本次未复测）。 |
| A05 | 未验证（引用历史证据） | 同上；冷/热 p50/p95 数字未复测。 |
| A06 | 部分实测 | 实测：`initial_load_cursor_walks_the_archived_range_without_overlapping_the_first_screen`（遍历断言无重叠/遗漏、页内正序；夹具规模小）。百万条 20 页数字未复测。 |
| A07 | 部分实测 | 实测：acp-types `an_oversized_first_row_occupies_a_page_alone_and_still_advances`、`a_body_over_the_remaining_budget_stops_the_page_before_decoding`；sqlite `a_payload_over_the_budget_occupies_its_own_page_and_the_cursor_advances`；阶段二只取本页行（`the_second_phase_is_asked_for_the_page_rows_only`、`the_candidate_row_beyond_the_page_never_leaves_storage`）。1 MiB×100 的峰值内存数字未复测。 |
| A08 | 实测 | wire `the_same_cursor_replays_equivalent_pages_and_limit_only_changes_page_boundaries`。 |
| A09 | 实测 | wire `a_cursor_survives_appends_and_another_full_without_moving_its_page_members`。 |
| A10 | 引用 | SDK `history-merge.test.ts`：`a tool result that arrives in a later page completes the earlier call without faking failure (A10)`、`the awaiting-result tool status is consumable as a constant`。本次未复跑 SDK。 |
| A11 | 引用 + 部分实测 | 实测（Rust）：wire `a_failed_initial_replay_fails_the_load...` 与 classify 的跨会话拒绝；引用：SDK `a page from a different snapshot is rejected whole (A11)`、`a page that arrives after rewind is rejected and the window is cleared`；TUI S7c D2。 |
| A12 | 引用（含边界） | S7c：SDK 8/8 判据 PASS、TUI D8 PASS；边界见 §3.7（pre-start EarlyReturn 无失败展示、锚点单元自消失不补偿、README 未记录边界）。 |
| A13 | 部分实测 | 见 §3.4：本机 5 条对照 + 受众规则实测；云 21 条未验证。 |
| A14 | 未验证 | 有 fork 相关装配测试（`child_wire_capture_test` fork 系列 11 passed），但「父会话后续 Full 不改变子快照或分页内容」无直接用例；分页内容面未测。 |
| A15 | 不成立（F1 场景） | 普通新会话 `nextCursor=null` 实测成立；**F1**：System 锚点会话有更早归档却返回 `null`（实测，§1）。SDK `a first screen with no earlier content reports an exhausted window`（引用）仅在无 System 形态成立。 |
| A16 | 引用 | `compact_pressure_adversarial_test`（10 passed）、`compact_progress_test` 的 watermark/shrink 用例（final-matrix it-* 全绿）。未逐条复跑。 |
| A17 | 引用 | `compact_session_manual_adversarial_test`（7 passed）、`compact_session_adversarial_test`（8 passed）等。未复跑。 |
| A18 | 部分实测 | 实测 4 条（`stable/durable/committed/excluded_committed`）；`async_tasks`/`external_settlement` 的冷恢复重投面（`test_cold_restore_*`）未复跑。 |
| A19 | 实测 | `-p peri-acp --lib -- rewind` 69 passed（含旧 cursor 失效、stale preview 拒绝、缺失边界不改历史）；remote rewind 3 条。 |
| A20 | 部分成立 | S7e：锚点单元存活的四形态 PASS（含证伪）；已声明边界：锚点单元自身被折叠/拆组时不补偿。**F1** 会使 System 锚点会话完全拿不到历史页 → A20 在这类会话上不成立。 |
| A21 | 实测（部分） | wire `a_page_request_is_answered_while_the_session_holds_the_execution_lock`；「不持有全局 sessions 锁跨 I/O」由 `handle_page` 短锁 + 锁外存储读的静态结构保证（`peri-acp/src/host/history_page.rs` 的 `handle_page`）。并发另一会话的实测未做。 |
| A22 | 实测 + 引用 | 实测：wire `a_failed_initial_replay_fails_the_load_instead_of_answering_a_cursor`；引用：SDK 4 条（`replay is staged until the load response installs it in one pass`、`replay from a superseded generation never reaches the window`、`a failed load discards its staged replay, and a retry installs exactly one copy`）。 |
| A23 | 引用 | SDK `sub items of one entry keep their own identity when they share a messageId (A23)`、`assistant and tool entries render without a Human prefix or an active turn (A23)`。 |
| A24 | 引用 | SDK `history pages never advance the live plan, tasks, queue, or current turn (A24)`。 |
| A25 | 实测 + 反例 | 见 §3.2（全部拒绝路径实测）；**F2**：完整回放契约的「无私有字段」加严口径不成立。 |

### 4.4 未验证 / 证据不足清单（宁多勿少）

1. `history_perf_tests` 5 条 `#[ignore]` 未运行 → A04/A05/A06/A07 的数字只有历史证据（`.recon/perf-evidence.md`），包含查询计划与扫描量。
2. 云后端 21 条 `#[ignore]` 未运行 → A13 的 Remote 侧未在真实云库验证（仅本机 SQLite-扮演-远端单测）。
3. TUI 渲染帧 / 整组件树 / `docs/verification/tui-integration-cases/` 人工用例未执行。
4. `e2e/` crate 未运行。
5. 基线同源对照未做（HEAD 源码上跑同一批命令；受禁止 git 写操作与磁盘限制）。
6. SDK 测试未由本审计复跑（引用 S10：252/253 pass、1–2 fail，归因基线 telemetry-spool panic，`langfuse-client/src/batcher/recovery.rs:256`，非本切片）。
7. A03 的**用户输入**（Human）形态未实测；「不重复注入」的计数未断言。
8. A14 的「父会话后续 Full 不改变子快照/分页内容」无直接用例。
9. A18 的冷恢复重投（`async_tasks`/`external_settlement` 面）未复跑。
10. A16/A17 的对抗用例只引用 final-matrix（未由本审计复跑）。
11. `peri.sessionWorkspaceV1` 键的门控来源未定位（协商连接出现、未协商未出现，均来自单次探针输出，未做发射点穷举）。
12. D12 记录的三类「范围外」只有静态定位（命令广播面、live reminder 形状），本审计未新增实测（F2 除外）。
13. F1 的「主会话不受影响」是静态推断（全仓仅 `spawn.rs` 一处 append System）；未构造主会话反例。
14. 「未协商 + cursor 被拒时不产生回放通知」只由 `history.len()` 不变间接证明，未做通知全量断言。
15. A21 的「并发加载另一会话」未实测（只有持锁场景）。
16. 磁盘/环境限制（S10 记录 `No space left on device` 历史事件）可能影响未来复现；本次审计期间磁盘剩余 16 GiB（`df -h` 实测）。
17. 本次审计只跑定向用例，**未复跑五个 crate 的全量 `--lib`**；`peri-acp --lib` 的 S10 计数（884）在审计者新增 zz_audit 探针之前产生，当前含探针为 885（定向 `history_page_wire` 实测 14 passed 已覆盖新增项）。

---

## 5. 复现命令汇总（本次审计实际执行）

```bash
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history

# 缺陷 F1（3 passed；探针断言缺陷存在）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --test zz_audit_history_anchor_probe

# F1 的 ACP 层形态 + 必查 2/3/5/6/7 的 wire 层（14 passed，含 zz_audit 探针）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_page_wire

# 必查 2（参数层，10 passed）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::history_page

# 必查 4（装配层 19 passed；存储 15 + 16 + 1 + 8 passed）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib -- history_page
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- history_page_tests
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- session_history
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- history_segment_tests
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- remote_active_snapshot

# 必查 5/6（6 + 2 + 11 + 1 + 1 + 1 + 69 passed）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- compact_cancel
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- test_full_commit
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- full_continuation
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- test_commit_compaction_lifecycle
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- history_read_only_backend
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- arriving_during
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- rewind

# 必查 6（A18：1 + 1 + 2 passed）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- stable_terminal_delivery
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- durable_terminal_delivery
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- committed_delivery

# 必查 1（只读）
git status --porcelain=v1 | grep -Ei 'Cargo\.(toml|lock)|package\.json|bun\.lock|\.sql$|schema|migrations?'
git diff -U0 | grep '^+' | grep -Ei 'CREATE TABLE|ALTER TABLE|ADD COLUMN|DROP TABLE|CREATE INDEX|user_version'
```

F2 的探针（已删除，源码/输出留档 `.recon/zz-probe-subagent-failure.{rs.txt,output.txt}`）当时命令：

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- zz_probe_unnegotiated_load_wire_fields --nocapture
# → 1 passed; 1 failed（未协商泄漏 subagentFailure；协商对照 passed）
```

---

## 6. 审计者对工作区的改动（全部为测试探针，未改生产代码）

| 文件 | 状态 | 说明 |
| --- | --- | --- |
| `peri-resources/tests/zz_audit_history_anchor_probe.rs` | **新增（保留中）** | F1 的 3 条探针；断言「缺陷存在」形态为通过。保留与否由用户裁决：作为回归证据建议保留（改名移除 `zz_` 前缀）或删除。 |
| `peri-acp/src/host/history_page_wire_test.rs` 末尾 `zz_audit_initial_cursor_skips_archive_when_the_oldest_effective_own_row_is_system` | **新增（保留中）** | F1 在 ACP 层的形态；现有 `-- history_page_wire` 从 13 条变 14 条，S10 矩阵的 884 计数未含它。 |
| `peri-acp/src/host/zz_adversarial_probe_test.rs`（F2 探针，含挂载） | **已删除** | 一次性用途；源码与输出留档 `.recon/zz-probe-subagent-failure.rs.txt` / `.recon/zz-probe-subagent-failure-output.txt`。已确认当前 `git status` 无该文件、无挂载残留（grep `zz_adversarial` 无命中）。 |

未执行任何 git 写操作、DDL、依赖变更；未修改生产代码。

---

## 7. 审计结论（三条要点）

1. **两条实测缺陷**：F1（P1，初始游标锚点在 System 条目上，subagent/fork 会话归档不可达或谎称无更早历史）与 F2（P2，未协商回放通道泄漏 `_meta.peri.subagentFailure`，按需求 §5.3 字面不违反、按加严口径不成立）。两者都未修（按用户指令），复现路径见 §1/§2/§5。
2. **本次审计的加严核对通过项**：无 DDL/依赖；失败关闭（28 条用例全绿，含「拒绝不触发初始 load」）；SQLite/Remote 本机一致性；收缩与幂等的核心不变量；六字段能力门控（含会话级注册表已写的 D12 成因）。
3. **证据强度的真实边界**：性能（A04–A07）、云后端（A13 真库）、TUI 人工用例、e2e、A03 用户输入形态、A14/A16/A17/A18 的部分面均未由本次审计实测；「全绿」只覆盖本报告 §5 列出的命令范围，不外推。
