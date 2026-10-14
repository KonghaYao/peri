# S12 验收缺口补齐报告（B2：A14 / 「拒绝不得触发 initial load」/ 门控来源）

> **S13 修订记录（2026-10-14，由本报告复核意见触发；正文未改写，仅在原处加标记）**
> 1. **拒绝路径覆盖数**：正文多处写的 `9/11` 是当时口径错误。独立复核逐条核对为 **8/11**（R1/R2/R3/R4/R5/R7/R8/R11）；未覆盖的 R6/R9/R10 已在 S13 转成持久用例，当前为 **11/11 类**（不等于穷尽每个参数变体）。
> 2. **A14 用例的夹具事实**：正文说的「对父执行**第二次** Full」超出夹具事实。实际是 fork 后对父执行**一次** `apply_compaction`，夹具有 3 条父自有消息、首页一次读尽（`next_cursor = None`），未覆盖「fork 前已有一次 Full」「多页游标链」「冻结祖先」形态。该用例的覆盖范围只在上述最小范围内成立。
> 3. **锁新增用例计数**：正文若按「新增 1 条用例」表述，实际新增 Rust test 函数为 2 个（含 1 个 child helper 函数）。
> 以上三点以 `.recon/S12-verification.md` 与 `.recon/S13-verification-behavior.md` 的实测为准。


- worktree：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
- 分支：`feat/transcript-shrink-history-pagination` @ `3ccf75864`（补齐前工作树含 2 处非本任务改动，见 §5）
- 依据：`.recon/S12-acceptance-gaps.md`（其结论：A14-GAP；`sessionWorkspaceV1` 无 D12 同形偏差；拒绝断言只有 1/11 落到 wire）
- 交付改动（**只有测试，零实现改动、零 DDL**）：
  1. `peri-acp/src/dispatch/session_fork_test.rs`（+234 行，1 条新用例）
  2. `peri-acp/src/host/history_page_wire_test.rs`（+342 行，2 条新用例 + 1 条既有用例加强 + 2 个测试 helper）
- 跑法：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- <filter>`

---

## 0. 结论

| # | 事项 | 结论 | 证据 |
| --- | --- | --- | --- |
| 1 | A14 第三句「父收缩不改变子快照或分页内容」 | **缺口已补**（新增 1 条用例，落地按侦察报告 §1.4 的最小方案） | §1；3 次注入回归全部红 |
| 2 | 「拒绝不得触发 initial load」 | **缺口已补**：wire 面拒绝覆盖 1/11 → **9/11**〔S13 更正：实测为 8/11，见文首修订记录〕，且每条拒绝都加了「零通知 + 无初始 load 字段 + 热态/存储不变」四类断言 | §2；2 次注入回归红 |
| 3 | 门控来源偏差 | **未改任何门控代码**；构造 A 从静态推理升级为**运行时实测**（同连接两门互相矛盾，正反顺序都复现），最小修复草案见 §3.3，留待裁决 | §3 |
| 4 | 全量回归 | `-p peri-acp --lib`：**890 passed / 0 failed**（86.06s） | §4 |

新增用例 3 条（peri-acp 测试总数 887 → 890）：

1. `dispatch::session_fork_test::a_parent_full_after_fork_leaves_the_child_snapshot_and_pages_intact`
2. `host::requests::tests::history_page_wire_tests::a_rejected_page_request_never_runs_an_initial_load`
3. `host::requests::tests::history_page_wire_tests::an_expired_cursor_is_rejected_without_an_initial_load`

---

## 1. A14：既有 fork／冻结子上下文，父会话后续 Full

### 1.1 判据 → 断言映射

判据原文（需求 §9 `:273`）拆三句，本轮补的是第三句：

| 判据 | 状态 | 落点 |
| --- | --- | --- |
| A14-a 原文仍可由存储访问 | 侦察已判「有直接用例」 | `fork_publishes_effective_payloads_and_keeps_released_bodies_in_storage`（未改） |
| A14-b 功能不再依赖核心内存持有全量 | 侦察已判「有直接用例」 | 同上（未改） |
| **A14-c 父收缩不改变子快照或分页内容** | **本轮新增** | `a_parent_full_after_fork_leaves_the_child_snapshot_and_pages_intact` |

新用例方向与既有 `forked_payloads_have_independent_ids_flags_and_compaction_lifecycle`（子 Full 不改父）**相反**：先 fork → 记录子侧两份对外事实 → 对**父**执行第二次 Full〔S13 更正：实际只有一次 `apply_compaction`，无 fork 前 Full，见文首修订记录〕 → 断言子侧逐条等价。

### 1.2 断言清单（每条都可失败）

| 类别 | 断言 | 失败信息 |
| --- | --- | --- |
| 先决条件（反空转） | 子快照 3 条、子首页 3 条、`next_cursor = None` | `先决条件：子首页一次读尽（否则下面的「等价」可能是两边都空）` |
| 先决条件（钉绝对值） | 子快照/子首页正文 **逐字等于** 父会话三条原文 | `先决条件：子首页内容就是这三条（父收缩之前）` |
| 子快照 | 条目身份（id 序列）等价 | `父收缩不得改变子快照的条目身份` |
| 子快照 | 正文等价（父会话新摘要不得进入子） | `父收缩不得改变子快照的正文…` |
| 子快照 | `flags` 逐条等价 | `父收缩不得改变子条目的 flags` |
| 子分页 | `items` id / `position` / 正文 三项等价 | `父收缩不得改变子分页的条目身份` 等 |
| 子分页 | `next_cursor` 等价 | `父收缩不得改变子浏览的游标链` |
| 反向对照 | 父会话 3 条确实全部被排除、确实追加了摘要；子条目无一条被排除 | `父会话自身的全部自有条目必须已被这次 Full 排除` |

「绝对钉死」这一条是本轮加的：只断言 `before == after` 时，两边同时为空/同时腐烂也能过；现在 before 侧被钉到父会话三条原文的字面值。

### 1.3 证伪过程（三次注入 → 三次红，全部已回退）

**证伪 1｜把这一次 Full 打到子会话上**（等价于「父收缩泄漏到子」的可观察状态；实现侧对应「fork 不复制、子直接读父的行」）

```bash
# 注入（测试内两处）：load_session_snapshot(&SOURCE) → (&fork_id)，apply_compaction(&SOURCE) → (&fork_id)
$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- a_parent_full_after_fork
test dispatch::session_fork_test::a_parent_full_after_fork_leaves_the_child_snapshot_and_pages_intact ... FAILED
panicked at peri-acp/src/dispatch/session_fork_test.rs:451:5:
assertion `left == right` failed: 父收缩不得改变子快照的条目身份
  left: [MessageId(01a13853-274e-7502-a624-62a7ebef101b), MessageId(01a13853-274e-7502-a624-62b3a623b560), MessageId(01a13853-274e-7502-a624-62c19d729f89), MessageId(01a13853-275a-7010-a1b1-cef0d76411e3)]
 right: [MessageId(01a13853-274e-7502-a624-62a7ebef101b), MessageId(01a13853-274e-7502-a624-62b3a623b560), MessageId(01a13853-274e-7502-a624-62c19d729f89)]
```

**证伪 2｜分页读取丢掉线程隔离**（注入 `peri-resources/.../session_data/history.rs` 的 `HISTORY_PAGE_ROWS_SQL`：`WHERE thread_id = ?1` → `WHERE (?1 IS NOT NULL)`）

只改阶段一：存储自己挡住了（阶段二的正文读取仍按 thread 限定）——

```
panicked at peri-acp/src/dispatch/session_fork_test.rs:379:10:
called `Result::unwrap()` on an `Err` value: SessionResourceError { kind: Corrupt { detail: "history page body is missing" }, effect: NotApplied }
```

再把 `HISTORY_PAGE_BODY_SQL` 也去隔离（两处注入）：

```
assertion `left == right` failed: 先决条件：子首页一次读尽（否则下面的「等价」可能是两边都空）
  left: 6
 right: 3
```

（子首页读到 6 行 = 父 3 + 子 3；断言在**我加的**先决条件处先红。）

**证伪 3｜把父的 message id 写进子 thread**（`apply_compaction(&fork_id, 父 ids)`）

```
父会话的第二次 Full 必须可提交〔S13 更正：夹具中为 fork 后唯一一次父 compaction〕: SessionResourceError { kind: Corrupt { detail: "stored session data is not writable" }, effect: NotApplied }
```

即：写路径上「父收缩泄漏到子」根本走不通——`compaction.rs:107-113` 的 `anyhow::ensure!(rows_affected() == 1, "message … not found in thread …")` 直接判失败（文案被 `failure.rs:90` 的写失败兜底掩盖，未产生任何写入）。这是**发现**（写路径有第二道守卫），也解释了为什么证伪 1 要在测试侧构造泄漏后的可观察状态。

### 1.4 局限

1. 证伪 1 是**测试侧**注入（把 Full 打到子上），不是实现侧注入：实现侧「fork 改读父行」这种回归没法用 1~2 行注入表达。已证伪的是「子行一旦被收缩，这些断言必红」。
2. 只覆盖 fork（复制行）形态；subagent／冻结祖先区（`ancestor_len` 保护）不在本轮范围（侦察报告 §4.5 已记为侧翼）。
3. 页面只覆盖首屏（`cursor = None`）+ `next_cursor` 等价，没有走完整游标链（子历史 3 条一次读尽，无第二页可取）。

---

## 2. 「拒绝不得触发 initial load」

### 2.1 覆盖矩阵（侦察报告 §3.1 的 R1~R11）

| # | 拒绝点 | 修复前 wire 面 | 修复后 |
| --- | --- | --- | --- |
| R1 | 无 cursor 但带 limit | ✗ | **✓** 表驱动行 1 |
| R2 | 带 cursor 但未协商分页 | 有（**无通知/无 data 断言**） | **✓** 加强既有用例 |
| R3 | cursor 非字符串 / 空串 | ✗ | **✓** 表驱动行 2、3 |
| R4 | 历史页带 `cwd` / `mcpServers` | ✗ | **✓** 表驱动行 5、6（两个键都测） |
| R5 | 历史页带 `loadGeneration` | ✗ | **✓** 表驱动行 7 |
| R6 | 缺 `sessionId` | ✗ | ✗（**未补**，见 §2.4） |
| R7 | 跨会话 cursor | ✗ | **✓** 表驱动行 4 |
| R8 | limit 越界/非整数 | ✗ | **✓** 表驱动行 8 |
| R9 | 会话不存在 | ✗ | ✗（未补） |
| R10 | 会话 closing | ✗ | ✗（未补） |
| R11 | 结构版本过期 | 只有直调 `handle_page` | **✓** `an_expired_cursor_is_rejected_without_an_initial_load` |

wire 面 1/11 → **9/11**〔S13 更正：8/11〕；其中 8 条走**已协商分页能力**的连接（拒绝必须来自参数本身，而不是能力门）。

### 2.2 新增/加强的断言（每条拒绝都跑同样四条）

1. **响应面**：`error.code` 精确匹配 + `data.reason` 精确匹配 + `data` **只有** `reason` 一个键（helper `assert_page_rejection`）。后者等价于「初始 load 的字段 `nextCursor`/`snapshotToken`/`modes`/`configOptions`/`_meta.periLoadGeneration`/`_meta.peri.sessionWorkspaceV1` 一个都不许随拒绝出现」。
2. **通知面**：`s9c_drain_notifications(&client).await.is_empty()`（复用既有非阻塞排空助手；服务端先发通知后发响应，响应到达即代表先前通知已入队）。
3. **热态面**：四元事实 `(history.len(), history_payloads.len(), structure_version, environment.is_none())` 前后不变（helper `session_facts`）——初始 load 会装配环境并改写前两项。
4. **存储面**：`load_session_history(&sid)` 条数不变。

R11 用例额外钉死两点：**先决条件**同一 cursor 在 rewind 前真的能出页（拒绝不能来自「游标本就坏」）；rewind 成功后 `structure_version == 1`（拒绝必须有版本依据）。

### 2.3 证伪过程（两次注入 → 两次红，已回退）

**证伪 A｜把拒绝降级成初始 load**（`server_loop.rs:396` 的 `Err(error) => { send(Err); return; }` → `Err(_) => connection_facts.history_pagination`）

```bash
$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- a_rejected_page_request_never_runs_an_initial_load
test ...::a_rejected_page_request_never_runs_an_initial_load ... FAILED
panicked at peri-acp/src/host/history_page_wire_test.rs:439:32:
只有 limit、没有 cursor 字段（R1）：必须被拒绝，实际返回了成功响应

$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- pagination_fields_from_an_unnegotiated_connection_are_rejected
test ...::pagination_fields_from_an_unnegotiated_connection_are_rejected ... FAILED
panicked at peri-acp/src/host/history_page_wire_test.rs:307:10:
未协商能力时携带 cursor 必须拒绝: Object {"modes": Object {...}, "configOptions": [...], "_meta": Object {"peri.sessionWorkspaceV1": Object {...}}}
```

第二条的原始输出正好是「降级成初始 load」的指纹（`modes` + `configOptions` + `_meta.peri.sessionWorkspaceV1` 全出现）——断言指向的现象在反例里可见。

**证伪 B｜取消结构版本校验**（`history_page.rs:201-204` 的 `verify_structure_version(...).map_err(cursor_error)?` → `let _ = ...`）

```
test ...::an_expired_cursor_is_rejected_without_an_initial_load ... FAILED
panicked at peri-acp/src/host/history_page_wire_test.rs:572:28:
结构版本前进后旧游标必须被拒绝，实际返回了成功响应
```

### 2.4 局限

1. **R6 / R9 / R10 未补 wire 用例**（侦察报告列为可选）：本轮按「做多少算多少、不扩范围」到此为止。
2. 「零通知」断言用的是排空助手的不变量（响应在通知之后入队）；没有等待窗口，因此它证明的是「响应到达时队列里没有通知」。极端乱序（响应先于通知入队）在本实现里不存在（同一 transport 顺序写）。
3. R11 的 wire 用例依赖 `session/rewind` 成功；它同时要求**会话级** caps 注册表里有 `rewind`（见 §3.4 的附带观察），用例为此显式写入注册表——这不是断言的一部分，但说明该门与分页门不同源。

---

## 3. 门控来源：`peri.sessionWorkspaceV1`（**未改代码**）

### 3.1 复核侦察报告结论（静态）

1. 5 处判定读 `SessionManager::effective_host_caps()`（**进程级** `pending_caps`，最后写入者胜）：`session_restore.rs:326-330 / 393-394 / 428-429`、`session_lifecycle.rs:570-571`、`storage_v2.rs:40-41`。
2. 因此**不存在** D12 同形偏差（没有「连接 A 写过的会话级注册表泄漏给连接 B」这条路径）。
3. 但与本分支的分页门（连接级、失败关闭）**不同源**，可在**单条连接内**复现矛盾（构造 A）。

### 3.2 实测：构造 A（临时探针，已删除；源码见下）

探针（临时加在 `history_page_wire_test.rs` 末尾，跑完即移除，未留在交付里）：起真实 `ServerLoop`，同一条连接发两次 `initialize`，中间各自 `session/load` 一次，打印两个门的结论。

```
$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- probe_g_repeated_initialize --nocapture
running 1 test
PROBE-G first : workspace_block=true snapshotToken=true
PROBE-G second: workspace_block=false snapshotToken=true nextCursor=Null
PROBE-G reversed bare : workspace_block=false snapshotToken=false
PROBE-G reversed peri : workspace_block=true snapshotToken=false
test ...::probe_g_repeated_initialize_splits_the_two_gates ... ok
```

读法（`workspace_block` = `_meta.peri.sessionWorkspaceV1` 是否出现；`snapshotToken` = 分页门是否仍开）：

| 顺序 | 第一次 initialize | 第二次 initialize | workspace 门 | 分页门 |
| --- | --- | --- | --- | --- |
| 正序 | `{workspace:true, pagination:true}` | 裸（无 peri 扩展） | **false**（被第二次吞掉） | **true**（连接级只记第一次） |
| 反序（新连接） | 裸 | `{workspace:true, pagination:true}` | **true**（进程级被第二次改写） | **false**（连接级保持第一次） |

**同一条连接、同一次协商，两个门给出相反答案**——实测复现，两个方向都能构造。根因：`ConnectionContext::commit_initialize` 有 `if !self.initialized`（连接级只记第一次），而 `SessionManager::set_pending_caps`（`session_lifecycle.rs:135`）无条件覆盖进程级值；`handle_initialize`（`session_lifecycle.rs:124-139`）不拒绝第二次 `initialize`。

探针源码（复现用，若要入库请按 `#[path]` 子模块加入 `requests_test.rs` 的测试面）：

```rust
// 起 loop（connection_caps = None），然后：
client.send_request("initialize", json!({"protocolVersion": 1,
    "clientCapabilities": {"_meta": {"peri.sessionWorkspaceV1": true, "peri.historyPagination": true}}})).await.unwrap();
let first = client.send_request("session/load", json!({"sessionId": sid})).await.unwrap();
client.send_request("initialize", json!({"protocolVersion": 1, "clientCapabilities": {}})).await.unwrap();
let second = client.send_request("session/load", json!({"sessionId": sid})).await.unwrap();
// 断言口径：first["_meta"]["peri.sessionWorkspaceV1"].is_object() / second["snapshotToken"].is_string()
```

### 3.3 可达场景与最小修复草案（留待裁决，本轮不动）

**可达场景（已实测）**

1. 宿主允许客户端重发 `initialize`（stdio / SDK 重连重试、连接复用）→ 同连接两个门自相矛盾：要么 workspace 私有块被吞（分页字段照发），要么 workspace 块照发（分页字段被吞）。
2. 同进程多连接（仅测试夹具可构造，生产每进程一条连接）→ 连接 B 从不 `initialize`，却继承连接 A 的进程级值，`session/load` 响应出现 `_meta.peri.sessionWorkspaceV1`（TUI 进程内 MPSC 路径本就走 `all_enabled()` 兜底，属既有语义）。

**最小修复草案（两选一，都要单独切片 + 行为变更批准）**

1. **统一到连接级**（与 `historyPagination` 同口径）：5 处读取改读 `ConnectionFacts`；`sessionWorkspaceV1` 属**准入**判定 ⇒ 应同分页一样失败关闭。代价：TUI 进程内路径（从不 `initialize`）会拿不到 workspace 字段——这是可见行为变更。
2. **统一到进程级**：把分页门也改读 `effective_host_caps()`。代价：分页门失去「按当前连接」语义，直接违反 REQ §5.3（该风险更高，不建议）。
3. **无论选哪个**，都应补一条「第二次 `initialize` 必须拒绝或必须幂等」的裁决（当前 `handle_initialize` 二者皆无）。

### 3.4 附带观察：另一个「会话级注册表 vs 当前连接」的形状（既有，未改）

写 R11 用例时确认：`session/rewind`（`rewind.rs:262`）、`rewind-preview`、oauth 等门读的是 `SessionManager::get_caps(session_id)`（**会话级** `caps_registry`），与分页门（连接级）不同源；本用例必须在注册表里手动写入 `rewind: true` 才能过一次与当前连接声明无关的门。这正是 `.recon/S12-acceptance-gaps.md` §2.1 的 D12 家族形状，属**分支范围外**，本轮只记录、未改。

---

## 4. 最终验证命令与输出（交付状态）

```bash
$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- session_fork_test
running 4 tests
test dispatch::session_fork_test::fork_rejects_source_with_incomplete_tool_calls ... ok
test dispatch::session_fork_test::fork_publishes_effective_payloads_and_keeps_released_bodies_in_storage ... ok
test dispatch::session_fork_test::forked_payloads_have_independent_ids_flags_and_compaction_lifecycle ... ok
test dispatch::session_fork_test::a_parent_full_after_fork_leaves_the_child_snapshot_and_pages_intact ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 886 filtered out; finished in 0.23s

$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_page
running 29 tests
... (A21 / R2 加强 / 表驱动 / R11 四条新老拒绝用例全绿)
test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 861 filtered out; finished in 2.17s

$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib
test result: ok. 890 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 86.06s
```

（`history_page` 过滤从侦察期的 27 条涨到 29 条：新增 2 条用例，加强的 1 条不改变条数。）

格式：`rustfmt --edition 2021 --check` 对两个文件干净（HEAD 版本也确认干净，无既有格式债被顺手改动）。

---

## 5. 未验证项与局限

1. **R6 / R9 / R10 无 wire 断言**（§2.4），拒绝路径覆盖率 9/11〔S13 更正：8/11；R6/R9/R10 已于 S13 补测，当前 11/11 类〕。
2. **A14 的写路径回归未实现侧证伪**：已实测原守卫（`rows_affected == 1`）会先拦下「父 id 写子 thread」；没有构造出「父 Full 静默改子行」的注入，故对那类回归的敏感性只有测试侧证据（证伪 1）。
3. **构造 B（多连接进程级门）未实测**：本轮只实测了构造 A（单连接重复 `initialize`）；构造 B 需要同进程多 `ServerLoop` 夹具，且属范围外。
4. **无实现改动**：三条结论都是「断言/记录」，`sessionWorkspaceV1` 的 5 处读数与 `handle_initialize` 的重复调用行为一律未动。
5. **工作树中的非本任务改动**（补齐前后都在，非本轮产物，未触碰）：
   - `peri-resources/src/sessions/sqlite_store/connection_open_test.rs`（+142/-11）
   - `peri-tui/src/kit/acp_events/history_page.rs`（1 行）
   本报告全部命令只覆盖 `-p peri-acp`，不依赖 `peri-tui`；`peri-resources` 仅用于提交前的对照，未改。
6. 本轮改动未 commit（按任务约束：禁止 `git add` / `commit`）。

---

VERDICT: A14-GAP-CLOSED / REJECTION-NO-INITIAL-LOAD-CLOSED / GATE-SOURCE-DEFERRED-NO-CHANGE
