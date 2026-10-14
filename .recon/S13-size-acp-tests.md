# S13 · 任务 B1：拆分 peri-acp 两个超限测试文件（按职责）

工作树：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
分支 `feat/transcript-shrink-history-pagination`，HEAD `3ccf75864`（每条命令均在该工作树执行；Rust 一律用 `./scripts/cargo-rmcp-patched.sh`）。

---

## 1. 结论

1. **拆分完成**：两个超限文件按职责拆为 2 个父模块 + 8 个子模块，10 个文件逐个 `wc -l` 最大 **535**（阈值 1000）。
2. **挂载真实成立**：canonical 过滤词命中数与拆分前逐项一致（`rewind_cases` 16、`unnegotiated_replay` 15），8 个子模块各自独立命中 6/1/4/4/1 + 8/4/3，无空跑（`0 filtered`/`0 passed` 均未出现）。
3. **语义零改动**：37 个来源区间全部逐字核对（33 块搬移到子模块、4 块保留在父模块），独立校验脚本证明「原文无缝覆盖 + 逐字出现 + 产物无凭空内容」；断言、函数体、doc 注释一字未改；生产代码与挂载父模块 `requests_test.rs` 均未改（sha256 与开工一致）。
4. **全量绿且计数一致**：`test --locked -p peri-acp --lib` 拆分前 **893 passed / 0 failed**，拆分后 **893 passed / 0 failed**；测试名叶子集合完全相同，仅 31 条用例的模块路径多了一层。
5. **未动范围外文件**：只改这两个测试文件并新增 8 个子模块文件；`history_page_wire_test.rs` 等 A2 产物、生产文件、文档均未触碰。

---

## 2. 拆分映射（按职责，非按行数切断）

### 2.1 `requests_rewind_cases_test.rs`（拆分前 1722 行）

| 文件 | 行数 | 用例 | 职责 | 来源区间（原文件 1-based） |
| --- | --- | --- | --- | --- |
| `requests_rewind_cases_test.rs`（父） | 243 | 0 | 共享夹具：`enable_rewind_cap` / `archive_prefix_in_storage` / `history_ids` / `payload_ids` / `enable_rewind_and_event_caps` / `agent_events` / `RewindFaultStore` + 门面转发宏；挂载 5 个子模块 | 1-222、1722 |
| `requests_rewind_effective_view_test.rs` | 535 | 6 | **回退后的有效视图**：归档目标可达、冷恢复视图、模型边界一致、存活摘要仍覆盖、`RemoveFrom` 边界含目标、`RewindCompleted` 载荷 | 223-302、303-381、382-482、618-718、840-929、1022-1103 |
| `requests_rewind_cursor_expiry_test.rs` | 136 | 1 | **rewind 与过期游标**：结构版本失效已发出的分页游标（A19） | 483-617 |
| `requests_rewind_failure_paths_test.rs` | 356 | 4 | **失败与拒绝路径**：未知边界不上报热态（F1）、durable 提交失败、能力门控、过期预览不改历史 | 719-839、930-1021、1104-1161、1351-1434 |
| `requests_rewind_routing_test.rs` | 191 | 4 | **请求路由到 dispatch**：candidates / preview / 未知目标 / rewind 三个方法 | 1162-1199、1200-1243、1244-1287、1288-1350 |
| `requests_rewind_inherited_region_test.rs` | 288 | 1 | **继承区口径**：继承区夹具 + 只清自有归档前缀 | 1435-1721 |

### 2.2 `requests_unnegotiated_replay_test.rs`（拆分前 1223 行）

| 文件 | 行数 | 用例 | 职责 | 来源区间 |
| --- | --- | --- | --- | --- |
| `requests_unnegotiated_replay_test.rs`（父） | 517 | 0 | 门面替身（`ReplaySpy`/`SpyMode`/`SegmentCall`）+ 夹具（`ReplayFixture`）+ 共享助手；挂载 3 个子模块 | 1-502、503-504 |
| `requests_unnegotiated_replay_delivery_test.rs` | 296 | 8 | **交付内容与 wire 形状**：逐条等于 canonical、无 Peri 私有字段、`caps.replay` 门控、空会话/仅继承区/超大正文、热态仍读存储、协商连接对照 | 505-547、548-580、581-628、773-791、792-819、820-845、846-883、884-943 |
| `requests_unnegotiated_replay_fail_closed_test.rs` | 145 | 4 | **有界读取与失败关闭/回退**：段读有界且不读全量、失败关闭、锚点中途消失、不支持段读时显式回退 | 629-673、674-701、702-732、733-772 |
| `requests_unnegotiated_replay_verification_test.rs` | 282 | 3 | **S9 独立复核用例**：混合载荷逐条等于 canonical、回退全量读失败仍关闭、协商连接保留条目戳；含复核用 wire 事实助手 | 944-945、946-1041、1042-1065、1066-1145、1146-1223 |

### 2.3 挂载方式

1. 父模块保持**原文件名与模块名不变**（`requests_test.rs` 里的 `#[path] mod rewind_cases;` / `mod unnegotiated_replay_tests;` 无需改动，实测该文件 sha256 未变）。
2. 子模块在父模块内用 `#[path = "..."]` 平铺挂载，子模块以 `use super::*` 复用父模块夹具（与 A2 的 `history_page_wire_test.rs` 拆分**同构**）。
3. 新增行仅三类：子模块头部 `use super::*;`（8 行）、父模块挂载声明与说明注释（33 行）、块间空行。

---

## 3. 证据

### 3.1 挂载真实成立（命令 + 原始输出）

命令（`FILTER` 为表中的过滤词）：

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- "$FILTER"
# 逐子模块结果（.recon/s13-acp-tests-raw/post-split-child-filters.log）
rewind_cases::effective_view_tests      test result: ok. 6 passed; 0 failed; 0 ignored; 887 filtered out
rewind_cases::cursor_expiry_tests       test result: ok. 1 passed; 0 failed; 0 ignored; 892 filtered out
rewind_cases::failure_paths_tests       test result: ok. 4 passed; 0 failed; 0 ignored; 889 filtered out
rewind_cases::routing_tests             test result: ok. 4 passed; 0 failed; 0 ignored; 889 filtered out
rewind_cases::inherited_region_tests    test result: ok. 1 passed; 0 failed; 0 ignored; 892 filtered out
unnegotiated_replay_tests::delivery_tests     test result: ok. 8 passed; 0 failed; 0 ignored; 885 filtered out
unnegotiated_replay_tests::fail_closed_tests  test result: ok. 4 passed; 0 failed; 0 ignored; 889 filtered out
unnegotiated_replay_tests::verification_tests test result: ok. 3 passed; 0 failed; 0 ignored; 890 filtered out
```

1. 6+1+4+4+1 = **16** = 拆分前 `rewind_cases` 的 16；8+4+3 = **15** = 拆分前 `unnegotiated_replay` 的 15。
2. 逐模块 8 条过滤命令全部 `>0 passed` 且 `filtered out` 非全量，排除「空跑」。

canonical 过滤词前后逐项一致（同一命令、同一过滤词）：

| 过滤词 | 拆分前 | 拆分后 |
| --- | --- | --- |
| `rewind_cases` | `16 passed; 877 filtered out` | `16 passed; 877 filtered out` |
| `unnegotiated_replay` | `15 passed; 878 filtered out` | `15 passed; 878 filtered out` |

模块路径的可见变化（`--list` 前后对比，`.recon/s13-acp-tests-raw/list-{pre,post}-split.log`）：

```
pre : host::requests::tests::rewind_cases::test_rewind_candidates_routes_to_dispatch: test
post: host::requests::tests::rewind_cases::routing_tests::test_rewind_candidates_routes_to_dispatch: test
```

1. 用例总数 893 → 893，**测试名叶子集合完全相同**（集合差为空，脚本比对，见 §3.3）。
2. 路径变化者恰为 **31** 条 = 16 + 15（本次搬移的全部用例）；`--leaf` 名不变，故按叶子名或 `rewind_cases` / `unnegotiated_replay` 子串过滤的命令全部照旧命中。

### 3.2 全量 lib 测试（单条命令，退出即完成）

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib
```

| 时点 | 原始输出 | 日志 sha256 |
| --- | --- | --- |
| 拆分前 | `running 893 tests` … `test result: ok. 893 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 76.51s` | `c0521146b7491853…` |
| 拆分后 | `running 893 tests` … `test result: ok. 893 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 78.64s` | `0801ab51e42dbe1b…` |
| 收工复核（工作树含其他 agent 其后的改动） | `running 893 tests` … `test result: ok. 893 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 82.00s` | `0e314badd19e1b3a…` |

1. 三次均 `EXIT=0`，日志内 `^(warning|error)|FAILED` 命中数 **0**（含编译告警）。
2. 另跑 `check --locked -p peri-acp --all-targets` → `Finished`，0 warning / 0 error（`.recon/s13-acp-tests-raw/check-post-split.log`）。
3. **基线口径说明**：任务书给的 890 是 S12 收工时的数字；本工作树上 A2 已先加入 3 条 wire 回归用例，故我在**同一棵树上**实测「拆分前 = 893」并以此为唯一基线，避免跨任务混用数字。两次运行期间工作树其余内容未变（叶子集合相同即为佐证）。

### 3.3 语义零改动（独立校验，脚本与搬移脚本不共享状态）

`python3 .recon/s13-acp-tests-raw/verify.py`（原文取自拆分前快照 `original/`，产物取自当前工作树）：

```
[覆盖] requests_rewind_cases_test.rs: 18 个区间无缝覆盖 1..1722
[覆盖] requests_unnegotiated_replay_test.rs: 19 个区间无缝覆盖 1..1223
[逐字搬移] requests_rewind_effective_view_test.rs: 6 块全部命中（533 行来源）
[逐字搬移] requests_rewind_cursor_expiry_test.rs: 1 块全部命中（135 行来源）
[逐字搬移] requests_rewind_failure_paths_test.rs: 4 块全部命中（355 行来源）
[逐字搬移] requests_rewind_routing_test.rs: 4 块全部命中（189 行来源）
[逐字搬移] requests_rewind_inherited_region_test.rs: 1 块全部命中（287 行来源）
[逐字搬移] requests_unnegotiated_replay_delivery_test.rs: 8 块全部命中（295 行来源）
[逐字搬移] requests_unnegotiated_replay_fail_closed_test.rs: 4 块全部命中（144 行来源）
[逐字搬移] requests_unnegotiated_replay_verification_test.rs: 5 块全部命中（280 行来源）
[父文件保留] requests_rewind_cases_test.rs: 2 块逐字保留
[父文件保留] requests_unnegotiated_replay_test.rs: 2 块逐字保留
[无凭空内容] 10 个文件全部 OK
VERIFY-OK：全部区间逐字搬移、覆盖无缺口、产物无凭空内容
```

1. 「逐字搬移」= 原文件区间文本在产物中作为**子串**出现（非重排、非重写）。
2. 「无凭空内容」= 产物中除空行、`use super::*;`、`#[path]`/`mod *_tests;`、挂载说明注释外，**没有任何原文件之外的行**（行多重集比对，`requests_test.rs` 的夹具行由 `use super::*` 传入，故子模块文件不再出现这些行）。
3. 未改动的挂载点：`peri-acp/src/host/requests_test.rs` sha256 = `abbdb7e7e0e0f823…`（与开工时一致，`git status` 中无该文件）。
4. 空行说明：块尾纯空行在搬移时丢弃、块间统一以 1 个空行分隔（文件 1 影响 15 行、文件 2 影响 18 行）；空行不承载语义，非空行 100% 逐字保留。
5. 原文末尾的孤立分区注释 `// ── session/cancel-bg-task 路由测试（issue 2026-08-05）───`（其对应用例实际在 `requests_workflow_cases_test.rs`；`origin/main` 的 311 行版本里同样存在于文件末行）按「不改内容」原则**原样保留在父文件末尾**，未删除。

### 3.4 文件规模

```bash
wc -l peri-acp/src/host/requests_rewind_*.rs peri-acp/src/host/requests_unnegotiated_replay*.rs
```

```
 243  requests_rewind_cases_test.rs          (拆分前 1722)
 535  requests_rewind_effective_view_test.rs
 136  requests_rewind_cursor_expiry_test.rs
 356  requests_rewind_failure_paths_test.rs
 191  requests_rewind_routing_test.rs
 288  requests_rewind_inherited_region_test.rs
 517  requests_unnegotiated_replay_test.rs  (拆分前 1223)
 296  requests_unnegotiated_replay_delivery_test.rs
 145  requests_unnegotiated_replay_fail_closed_test.rs
 282  requests_unnegotiated_replay_verification_test.rs
```

`bash scripts/check-file-size.sh`（`.recon/s13-acp-tests-raw/check-file-size-post-split.log`）：本次 10 个文件**均不再出现在超限列表**；脚本仍 `EXIT=1`，因**范围外**既存超限文件（源码 2：`peri-tui/src/kit/panels/mcp.rs` 1637、`peri-tui/src/kit/acp_events/system.rs` 1013；测试 4：`peri-resources` 三个 + `peri-tui/src/kit/acp_events_test/session_events_test.rs` 1021）——这些不属于 B1 授权范围，未动。

---

## 4. 未验证项与局限

1. **未做生产代码注入证伪**：B1 明确禁止改动指定范围之外的文件，故没有「故意破坏生产代码 → 断言变红」这一轮。当前「用例真的在跑且仍是原来那些断言」由三条独立证据支撑：逐字搬移校验 + 子模块过滤命中数 + 全量 893 前后一致。
2. **模块路径多一层**：31 条用例的全限定路径由 `...::rewind_cases::test_x` 变为 `...::rewind_cases::<子模块>::test_x`。按叶子名或 `rewind_cases`/`unnegotiated_replay` 子串过滤的命令不受影响；但历史报告里写死的**全限定路径字符串**（如 `.recon/S11-verification.md:397`）现已过期，本次未回头改历史报告（范围外）。
3. **其他任务并发**：工作树内 A1/A2/B2 同时在跑（期间出现过 shell 并发上限、编译被 120s 超时打断后重跑；收工时 `peri-agent`、`peri-acp-types` 也有其他切片在改）。两次全量运行都在「含 A2 改动」的同一棵树上完成，叶子集合完全相同；另在收工前对**当时最新树**再跑一次全量仍为 893 绿。即便如此，我无法排除其他 agent 在我之后继续改动这棵树。
4. **未跑的检查**：未跑 `clippy`/`fmt`（拆分只是搬移，且仓库 pre-commit 由交付方统一跑）；未跑 E2E 与其他 crate 的全量测试（B1 范围只要求 peri-acp 的 `--lib`）。
5. **未验证规模脚本的 CI 语义**：`check-file-size.sh` 仍在整体 `EXIT=1`（范围外超限文件所致），本次只对「本轮改动文件逐个 ≤1000」负责。

---

## 5. 证据归档

`.recon/s13-acp-tests-raw/`：`original/`（拆分前快照 + sha256）、`split.py` 与 `split-report.txt`（分块与字节校验）、`verify.py` 与 `verify-report.txt`（独立校验）、`list-{pre,post}-split.log`、`full-lib-run-{pre,post}-split.log` 与 `full-lib-run-final-check.log`、`post-split-child-filters.log`、`check-post-split.log`、`check-file-size-post-split.log`、`wc-lines-post-split.txt`、`sha256-post-split.txt`。
（`split.py` 只对拆分前快照有效，读源路径已是 `original/`，不会对已拆分的树复读出错误产物。）

VERDICT: SIZE-SPLIT-DONE
