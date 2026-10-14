# S13 · peri-acp wire 测试拆分 + R6/R9/R10 回归用例

工作树（git worktree）：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
分支 `feat/transcript-shrink-history-pagination`，HEAD `3ccf75864`。
（每条命令均以该工作树为 cwd；Rust 构建/测试一律用 `./scripts/cargo-rmcp-patched.sh`。）

---

## 1. 结论

1. **拆分完成**：原单文件 `history_page_wire_test.rs`（拆分前 1935 行）按职责拆为 1 个父模块 + 5 个子模块，全部 ≤1000 行（STD-SIZE-001）。挂载真实成立，可被 cargo 过滤器命中。
2. **R6/R9/R10 已转成持久回归用例**：走真实 `ServerLoop` + MPSC transport + `dispatch_request`，断言错误码、零回放通知、热态与存储事实不变；三条均实测通过。
3. **注入证伪完成 3 轮**：每轮注入后目标用例变红，其余保持绿；生产文件均按 `shasum` + `diff -q` 校验「字节级还原」。
4. **全量绿**：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib` → **893 passed; 0 failed; 0 ignored; 0 filtered out**（单条命令，退出即完成）。
5. **未改生产实现**：本次只新增/搬移测试文件；生产文件 `history_page.rs`、`server_loop.rs` 的 sha256 与开工时一致。
6. 未发现新的生产缺陷。

---

## 2. 改动清单

### 2.1 落地文件（全部在 `peri-acp/src/host/`）

| 文件 | 行数 | sha256（前 16 位） | 职责 |
| --- | --- | --- | --- |
| `history_page_wire_test.rs`（父） | 333 | `ee02d970…` | 连接/存储夹具、跨文件共享 helper、子模块挂载 |
| `history_page_initial_cursor_test.rs` | 557 | `665d9d5a…` | 首屏与初始游标契约（A20/A22 等） |
| `history_page_chain_test.rs` | 168 | `cdb9b072…` | 页链与合并（A08/A09） |
| `history_page_rejection_test.rs` | 518 | `4c1870c8…` | 拒绝路径（A21/A25 表驱动 + 新增 R6/R9/R10） |
| `history_page_rewind_test.rs` | 127 | `9ebb8a0f…` | rewind 与过期游标（A19） |
| `history_page_caps_test.rs` | 542 | `0e5b95fd…` | 能力门控与私有字段 |

`history_page_rejection_test.rs` 拆分后为 266 行，本次追加 R6/R9/R10 后为 518 行。

### 2.2 未改动的挂载点与生产文件

| 文件 | 行数 / sha256 | 状态 |
| --- | --- | --- |
| `peri-acp/src/host/requests_test.rs`（挂载父模块，566-567 行） | 709 / `abbdb7e7…` | **未改**（`git diff --stat` 无该文件） |
| `peri-acp/src/host/history_page.rs`（生产） | `45a180f1…` | **未改** |
| `peri-acp/src/host/server_loop.rs`（生产） | `fabe0783…` | **未改** |

`git diff --stat`（范围内）只有一处：
```
 peri-acp/src/host/history_page_wire_test.rs | 1398 ++-------------------------
 1 file changed, 65 insertions(+), 1333 deletions(-)
```

### 2.3 拆分映射（父文件原 1-based 闭区间）

| 目标文件 | 来自原文件区间 |
| --- | --- |
| `history_page_wire_test.rs`（父） | 14-166、223-268、691-756、1411-1435 |
| `history_page_initial_cursor_test.rs` | 596-647、649-689、919-966、968-1110、1674-1783、1785-1813、1815-1908、1910-1935 |
| `history_page_chain_test.rs` | 757-846、848-917 |
| `history_page_rejection_test.rs` | 168-221、270-334、336-473 |
| `history_page_rewind_test.rs` | 475-594 |
| `history_page_caps_test.rs` | 1112-1374、1376-1409、1437-1672 |

正文全部按区间原样切出，不做重排、不重写断言（语义零改动）。未覆盖 32 行 = 模块文档 1-5 行（改写为父模块说明）+ `use` 导入 6-13 行（6-12 逐字节保留在父模块头部）+ 分区空行 19 行。

### 2.4 子模块挂载方式

父模块内以 `#[path]` 挂载子模块，子模块经 `use super::*` 复用父模块夹具：

```rust
#[path = "history_page_initial_cursor_test.rs"]
mod initial_cursor_tests;
// …chain_tests / rejection_tests / rewind_tests / caps_tests 同形
```

仓库内已有同类先例（`requests_cron_test.rs` → `requests/cron_endpoint_test.rs`）。

### 2.5 拆分脚本与原始文本

- 脚本：`.recon/s13-wire-split-raw/split.py`
- 校验输出：`.recon/s13-wire-split-raw/split-report.txt`
- 注意：`split.py` 只对「拆分前的 1935 行版本」有效，**不可对已拆分树复跑**（父文件已变为 333 行，会触发 `AssertionError`）。

---

## 3. 挂载真实成立的证据

全部由 `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- <filter>` 产出，只摘 `test result:` 行。

### 3.1 任务指定过滤词

| 过滤词 | 结果 |
| --- | --- |
| `history_page` | `ok. 32 passed; 0 failed; 861 filtered out` |

32 = `host::requests::tests::history_page_wire_tests::*` 21 + `host::history_page::tests` 10 + `dispatch::session_replay` 1。

### 3.2 每个新文件的模块路径过滤（全限定名，无重名干扰）

| 过滤词 | 结果 |
| --- | --- |
| `history_page_wire_tests::initial_cursor_tests` | `ok. 6 passed; 887 filtered out` |
| `history_page_wire_tests::chain_tests` | `ok. 2 passed; 891 filtered out` |
| `history_page_wire_tests::rejection_tests` | `ok. 6 passed; 887 filtered out` |
| `history_page_wire_tests::rewind_tests` | `ok. 1 passed; 892 filtered out` |
| `history_page_wire_tests::caps_tests` | `ok. 6 passed; 887 filtered out` |
| 合计 `history_page_wire_tests` | `ok. 21 passed; 872 filtered out` |

6+2+6+1+6 = 21，与父模块过滤一致 → 5 个子模块都被真实编译并挂载（若挂载失败，这些用例根本不会出现在测试清单里）。

计数来源可复核：HEAD 提交版（`git show HEAD:…`，1601 行）含 16 条 `#[tokio::test]`；拆分产物含 21 条。其中 `history_page_rejection_test.rs` 的搬移区（1-266 行）只有 3 条（`#[tokio::test]` 位于第 9、67、134 行），第 268 行起才是本任务新增的 3 条（第 274、349、439 行）——故搬移部分共 21 − 3 = **18** 条，与「拆分前该文件的用例数」一致。

> 单文件代表用例（各取一条，均命中且通过）：

| 文件 | 代表用例 | 结果 |
| --- | --- | --- |
| initial_cursor | `the_initial_cursor_anchors_past_non_displayable_own_rows` | `ok. 1 passed; 892 filtered out` |
| chain | `a_cursor_survives_appends_and_another_full_without_moving_its_page_members` | `ok. 1 passed; 892 filtered out` |
| rejection | `a_page_request_for_a_closing_session_is_rejected_without_a_replay` | `ok. 1 passed; 892 filtered out` |
| rewind | `an_expired_cursor_is_rejected_without_an_initial_load` | `ok. 1 passed; 892 filtered out` |
| caps | `s9c_verification_an_initialize_with_peri_extensions_keeps_every_replay_field` | `ok. 1 passed; 892 filtered out` |

---

## 4. R6 / R9 / R10 持久回归用例

三条都在 `history_page_rejection_test.rs`，均走真实 `spawn_loop(...)` + `client.send_request("session/load", …)`（即真实 `ServerLoop` + `dispatch_request`），不使用下层函数直调。

### 4.1 R6 · 合法 cursor 但缺 `sessionId`

用例：`a_page_request_without_a_session_id_is_rejected_without_an_initial_load`（274 行起）

1. 夹具造出归档历史并取 `initial_cursor()` 得到合法游标。
2. 发送 `{"cursor": ...}`（无 `sessionId`）→ 必须报错，错误码 `-32602`。
3. `error.data == None`（**本类拒绝不带 `data.reason`**，故不套用 `assert_page_rejection` 的「data 只含 reason」规则）。
4. `s9c_drain_notifications` 为空 → 零回放通知。
5. `session_facts` 热态四元不变 + `load_session_history` 长度不变。
6. 阳性对照：同一连接补上 `sessionId` 后，同一游标照常出页（排除「被拒只是因为连接坏了」）。

### 4.2 R9 · 会话不在热态集合（冷恢复视角）

用例：`a_page_request_for_a_session_outside_the_live_set_is_rejected_without_creating_it`（349 行起）

1. 先证明游标可用（出页成功）— 拒绝不能来自「游标本就无效」。
2. 从热态 map `remove` 该会话（存储仍在）。
3. 同游标再请求 → `-32602`、`data == None`、零回放通知。
4. **不得凭 cursor 把会话建回来**：`states.contains_key(&sid) == false`，且 `states.len() == hot_before`（不创建任何会话）。
5. 存储长度不变。

### 4.3 R10 · 会话处于 closing

用例：`a_page_request_for_a_closing_session_is_rejected_without_a_replay`（439 行起）

1. 置 `closing = true` → 请求按「忙/关闭」回答 `-32010`、`data == None`、零回放通知。
2. 热态四元事实与存储长度均不变。
3. 与同文件 A21（持执行锁仍可出页）形成对照：**执行锁不是拒绝理由，只有关闭状态才是**。

---

## 5. 注入证伪（每轮：注入 → 跑 → 还原 → 校验）

注入前生产文件基线（`.recon/s13-wire-split-raw/production-sha256-before-injections.txt`，副本存 `/tmp/s13_orig/`）：

```
45a180f17ac3d1360727fda52ef72e3c6ae63982439e7a89689eb0b3c4f7a7d8  peri-acp/src/host/history_page.rs
fabe0783f1cbb87d39e6ab1e3286ec529931956666031816a05600e58d4d813f  peri-acp/src/host/server_loop.rs
```

| 轮次 | 注入点 | 注入内容 | 结果 | 还原校验 |
| --- | --- | --- | --- | --- |
| 1 | `history_page.rs` `session_id()` | 缺字段放宽为 `unwrap_or_default()` | **R6 红**：`assertion failed: 缺 sessionId：错误码 left: -32011 right: -32602`（rejection 文件 302 行）；其余 3 条绿 | `RESTORED-BYTE-IDENTICAL` |
| 2 | `history_page.rs` `handle_page()` | 存在性校验放宽，结构版本改走兜底默认值 | **R9 红**：`会话不在热态集合时必须被拒绝，实际返回了成功响应`（400 行）；其余 3 条绿 | `RESTORED-BYTE-IDENTICAL` |
| 3 | `server_loop.rs` | 失败路径先 `send_notification("session/update", …)` | **R6/R9/R10 三条同时红**，全挂在「拒绝不得发出任何回放通知」（307 / 406 / 499 行）：`FAILED. 1 passed; 3 failed; 889 filtered out` | `RESTORED-BYTE-IDENTICAL` |

还原校验方式：`shasum -a 256` 与 `/tmp/s13_orig/` 备份逐项比对 + `diff -q` 无差异。当前工作树复核结果：

```
45a180f17ac3d1360727fda52ef72e3c6ae63982439e7a89689eb0b3c4f7a7d8  peri-acp/src/host/history_page.rs
fabe0783f1cbb87d39e6ab1e3286ec529931956666031816a05600e58d4d813f  peri-acp/src/host/server_loop.rs
RESTORED-BYTE-IDENTICAL   （diff -q 对两个文件均无输出）
```

---

## 6. 测试与规模证据

### 6.1 全量（单条命令，wrapper）

```
$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.07s
     Running unittests src/lib.rs (target/debug/deps/peri_acp-ec56433c3d2253bf)
running 893 tests
…
test result: ok. 893 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 102.95s
```

完整日志归档：`.recon/s13-wire-split-raw/full-lib-run.log`（902 行，sha256 `18f86352…`）。日志内 `grep -E "^(warning|error)"` 无命中。

基线 890 与本次 893 的差 = 本次新增 3 条用例（见 §4）。**该基线已独立复核**——在现树上跳过新增 3 条再跑一次全量：

```
$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- \
    --skip a_page_request_without_a_session_id_is_rejected_without_an_initial_load \
    --skip a_page_request_for_a_session_outside_the_live_set_is_rejected_without_creating_it \
    --skip a_page_request_for_a_closing_session_is_rejected_without_a_replay
test result: ok. 890 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 88.90s
```

即 893 − 3 = 890 = 开工前基线，且被跳过的恰好是本次新增的 3 条。日志归档：`.recon/s13-wire-split-raw/full-lib-run-minus-3-new.log`（899 行，sha256 `bda323f2…`）。

### 6.1b `history_page` 过滤词 32 条的分解（`--list` 直接核对）

| 匹配来源 | 条数 |
| --- | --- |
| `host::requests::tests::history_page_wire_tests::*`（本次拆分产物） | 21 |
| `host::history_page::tests::*`（既有 `history_page_test.rs`，未改） | 10 |
| `dispatch::session_replay::tests::replay_marks_carry_the_same_entry_identity_and_position_as_a_history_page` | 1 |
| 合计 | 32 |

### 6.2 文件规模

```
$ wc -l peri-acp/src/host/history_page*_test.rs peri-acp/src/host/requests_test.rs
     542 peri-acp/src/host/history_page_caps_test.rs
     168 peri-acp/src/host/history_page_chain_test.rs
     557 peri-acp/src/host/history_page_initial_cursor_test.rs
     518 peri-acp/src/host/history_page_rejection_test.rs
     127 peri-acp/src/host/history_page_rewind_test.rs
     199 peri-acp/src/host/history_page_test.rs   ← 既有文件，未改
     333 peri-acp/src/host/history_page_wire_test.rs
     709 peri-acp/src/host/requests_test.rs
```

全部 ≤1000。

```
$ bash scripts/check-file-size.sh
📊 大文件扫描：源码阈值 1000 行 / 测试阈值 1000 行
❌ 源码超阈值（Top 30）：
    1637  peri-tui/src/kit/panels/mcp.rs
    1064  peri-agent/src/session/transcript.rs
    1034  peri-acp-types/src/session_resources.rs
    1013  peri-tui/src/kit/acp_events/system.rs
❌ 测试超阈值（Top 30）：
    1722  peri-acp/src/host/requests_rewind_cases_test.rs
    1361  peri-resources/src/sessions/sqlite_store/session_history_perf_test.rs
    1223  peri-acp/src/host/requests_unnegotiated_replay_test.rs
    1101  peri-agent/src/session/transcript_test.rs
    1038  peri-resources/src/sessions/sqlite_store/schema_test.rs
    1029  peri-resources/src/sessions/remote/full_remote_test.rs
    1021  peri-tui/src/kit/acp_events_test/session_events_test.rs
共扫描 2319 个文件，超阈值 11 个（源码 4 / 测试 7）
EXIT=1
```

退出码 1 来自 11 个**本任务范围外的既有越限文件**；本次拆分产物与 `requests_test.rs` 一个都不在列。

---

## 7. 未验证项与局限

1. **拆分前 1935 行原始副本未落盘留存**，守恒校验（逐行分区、1903 行搬移、32 行未覆盖）只在写盘前于内存中跑过一次，其输出原文保存在 `.recon/s13-wire-split-raw/split-report.txt`，**现在无法复跑复核**。
   - 替代的可复核证据（已验证）：
     - HEAD 提交版 `git show HEAD:peri-acp/src/host/history_page_wire_test.rs` = 1601 行 / sha256 `0bdf5644…`；其 32 个 `async fn` 名字集合**零缺失**地包含于拆分产物（`comm -23` 输出为空）。
     - 全量 893 passed / 0 failed，说明全部用例仍可编译并执行。
   - 因此「字节级守恒」这一条只有当时的输出记录，没有第三方可复算的物证。
2. **拆分前旧二进制已不可得**，基线 890 不是用旧二进制测出来的；改为在现树上跳过本次新增的 3 条重跑，得到 `890 passed; 3 filtered out`（§6.1）。该结果支持「当前 893 = 开工前 890 + 新增 3」；配合 §3.2 的 16 → 18 → 21 用例计数链与 §7.1 的名字集合包含关系，可排除「拆分吞掉用例」。
3. **存储断言只比较长度摘要**（`load_session_history(...).len()`），不比较内容字节；即「拒绝不得改动 canonical 历史」是以长度为代理，理论上存在「等长改写」漏网。
4. **跨 crate 回归未跑**（peri-resources / peri-agent / peri-tui / peri-acp-types / langfuse-client）：本任务只动 `peri-acp` 测试文件，按范围未跑。
5. **未做 `git add` / `commit`**：5 个新文件仍是 untracked，父文件改动仍在工作区未暂存。
6. 工作树内其它会话的既存改动（`langfuse-client/…/recovery.rs`、`peri-acp/src/dispatch/session_fork_test.rs`、`peri-resources/…/connection_open_test.rs`、`peri-tui/…/acp_events/history_page.rs`、`npm-packages/@peri-sdk/tests/wasm-acp-integration.test.ts`）**未被触碰**；全量 893 passed 是在这些改动同时存在的工作树上取得的。
7. R6/R9/R10 的「零回放通知」断言依赖 `s9c_drain_notifications` 排空队列后判断为空；若未来实现改为异步延迟发通知，该断言可能给出假绿（当前实现为同一次 dispatch 内同步发出）。

---

## 8. 证据文件索引

| 路径 | 内容 |
| --- | --- |
| `.recon/s13-wire-split-raw/split-report.txt` | 守恒校验输出原文 + 映射表 |
| `.recon/s13-wire-split-raw/split.py` | 拆分脚本（只对 1935 行原始版本有效） |
| `.recon/s13-wire-split-raw/production-sha256-before-injections.txt` | 注入前生产文件 sha256 清单 |
| `.recon/s13-wire-split-raw/full-lib-run.log` | 全量 `-p peri-acp --lib` 运行日志（893 passed） |
| `.recon/s13-wire-split-raw/full-lib-run-minus-3-new.log` | 跳过新增 3 条的全量运行日志（890 passed / 3 filtered） |
| `/tmp/s13_orig/{history_page.rs,server_loop.rs,sha256.txt}` | 注入前生产文件备份（还原校验参照物） |

---

VERDICT: WIRE-SPLIT-DONE
