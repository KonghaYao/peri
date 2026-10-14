# S13 规模治理独立复核（任务 C：B1～B4）

## 1. 结论与判据

**按本任务「完整文件集合中所有文件均 ≤1000 行」的硬判据，结论 FAIL。** 实际生产／测试目录的拆分已经达标，但不能据此宣称完整交付集合达标。

1. 独立逐文件执行 `wc -l`：指定集合共 **381 个文件，14 个仍超限**，全部在 `.recon/`。其中 2 个是未忽略的 `.rs` 原始副本；其余为日志或 `.orig` 副本。没有漏掉它们，也没有把日志超限误报为产品源码超限。
2. 指定集合内 **183 个非 `.recon` 源码／测试文件全部 ≤1000 行**。另外补查指定集合遗漏的 3 个已跟踪未提交文件，亦均达标。
3. B1～B4 的职责拆分、测试挂载、已用公开路径和运行时回归通过。未发现顺带修改断言或业务逻辑；未发现拆分范围内残留的行为注入。
4. 五个指定 crate 独立 lib 全量：**4916 passed / 0 failed / 32 ignored**。ACP 为 893，较 S12 的 890 多出的 3 条可归于 A2 补测，不是 B1～B4 增删测试。
5. 另有真实工程门禁失败：`git diff --check` 报 `requests_unnegotiated_replay_test.rs:517: new blank line at EOF.`。

| 判据 | 判定 | 依据 |
|---|---|---|
| 指定完整集合每文件 ≤1000 行（核心） | **FAIL** | 381 文件逐个 wc，14 个超限，见 §3 |
| 非 `.recon` 实际源码／测试规模 | PASS | 183 个全部达标，最大 990 行；补查 3 个 dirty 文件亦达标 |
| B1～B4 各拆分产物 ≤1000 行 | PASS | §3.3 独立 wc 清单 |
| 按职责拆分、不压缩排版凑行数 | PASS | §4 三处抽查；按完整原始代码行核对，无函数体压缩 |
| B1～B4 逻辑／断言零改动 | PASS（静态核对与 native 回归范围内） | §5 对照 git HEAD；函数、属性及非注释行差异仅接线 |
| 既有公开路径可用、下游可编译 | PASS（现有消费者） | re-export 检查、五 crate 编译测试、TUI all-targets check |
| 每个拆分用例文件真实挂载 | PASS | §6 共 23 个文件各 1 passed，非空跑 |
| 五 crate lib 全量及基线核对 | PASS | §7；ACP +3 有逐条来源，无测试丢失 |
| 未还原临时逻辑修改 | 未发现 | B1～B4 对照 HEAD，无多余行为差异；不扩张到 A1/A3 因果证明 |
| 补充 whitespace 门禁 | **FAIL** | `git diff --check` exit 2，§8 |

说明：`STD-SIZE-001` 本身的源码扩展名口径不应强加给普通 `.txt` 日志；本次 FAIL 首先服从用户明确要求的“所有文件”全集口径。即使按源码扩展名筛选，仍有两个 `.recon/.../original/*.rs` 超限；它们是审计副本，不是已挂载的产品模块。**不建议为此机械拆碎原始证据**；应由任务所有者明确证据归档／验收范围，本复核不擅自豁免、删除、改名或写 ignore。

## 2. 范围、基线与方法

工作目录：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`。

实际执行 `pwd && git status --short && git rev-parse HEAD && git branch --show-current`：

```text
/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history
3ccf75864c00475c07ffb735de92e0840c10fbca
feat/transcript-shrink-history-pagination
```

开始时有 15 个已跟踪修改文件及 S12／S13 报告、拆分新增模块；不是干净工作树。先读 S12 八份记录的结论与范围、S12 独立复核的关键验收项，再按实际源码取证。S13 实施者报告未用作通过依据；`.recon/workflows/s13-revision-round.mjs:108-177` 仅用于确定 B1～B4 授权对象。

以下命令均有共同前缀（实际每条 Bash 调用均以此前缀开始）：

```bash
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history &&
```

本复核只写本报告；没有修改生产／测试文件，没有 git add/commit/checkout/stash/rebase，没有裸 cargo 或 `--workspace`。未自行执行数据库 DDL 或 schema 实验；按用户要求运行现有测试，其既有临时数据库初始化会建立夹具 schema，未改生产数据库。

### 2.1 独立性与原始快照

语义基线直接取 `git show HEAD:<path>`，不是实施者保存的 `.orig`。另用字节相等比较验证：

```text
ORIGINAL_EQUALS_HEAD requests_rewind_cases_test.rs True
ORIGINAL_EQUALS_HEAD requests_unnegotiated_replay_test.rs True
ORIGINAL_EQUALS_HEAD transcript.rs.orig True
ORIGINAL_EQUALS_HEAD transcript_test.rs.orig True
ORIGINAL_EQUALS_HEAD system.rs.orig True
ORIGINAL_EQUALS_HEAD session_events_test.rs.orig True
ORIGINAL_EQUALS_HEAD full_remote_test.rs.orig True
ORIGINAL_EQUALS_HEAD session_history_perf_test.rs.orig True
```

这些结果仅说明八份原始副本确实等于对应 HEAD 文件；不把它们的存在当作测试成功或最终源码还原证明。

## 3. 完整行数核实

### 3.1 取集合与逐文件执行的实际算法

通过 `python3` 调用以下 Git 命令获取集合，随后对每一个路径独立调用系统 `wc -l`，不是 Python 行数估算：

```python
get = lambda *a: subprocess.check_output(['git', *a]).decode().splitlines()
committed = get('diff', '--name-only', 'origin/main...HEAD')
untracked = get('ls-files', '--others', '--exclude-standard')
paths = sorted(set(committed + untracked))
for p in paths:
    if not pathlib.Path(p).is_file():
        print('MISSING', p)
        continue
    print(subprocess.check_output(['wc', '-l', p]).decode().strip())
```

关键原始输出：

```text
COMMITTED 242 UNTRACKED 139 UNION 381
OVER_LIMIT 14
MISSING []
ADDITIONAL_DIRTY_TRACKED 3
UNION 381 SOURCE 195 ACTUAL_SOURCE_EXCLUDING_RECON 183
```

完整 381 行逐文件 wc 原始输出保存在本会话工具输出文件（本复核生成，不是实施者日志）：
`/var/folders/d5/gpfmkm2s4sqgwz5wwnj44p500000gn/T/peri-tool-output-26593f5a-9629-4d5f-ac3b-d84e18ee15cb.txt`。

此集合是在写入本报告之前取的；最终复扫含报告共 **382 个文件，仍为 14 个超限**。本报告 `wc -l` 为 **334**，最终 `git status --short` 相比开始仅多本报告路径。没有用全库扫描替代用户指定的交付集合，也没有静默排除 `.recon` 或二进制文件；SQLite sidecar 的 wc 数字不解释为源码质量。

### 3.2 仍超限的全部 14 个文件（原始 wc 数字）

```text
1857 .recon/S7d-raw-tui-tests.txt
1830 .recon/S7e-full-lib-final.txt
1722 .recon/s13-acp-tests-raw/original/requests_rewind_cases_test.rs
1223 .recon/s13-acp-tests-raw/original/requests_unnegotiated_replay_test.rs
1104 .recon/s13-size-agent-raw/after-lib-test.txt
1105 .recon/s13-size-agent-raw/before-lib-test.txt
1064 .recon/s13-size-agent-raw/transcript.rs.orig
1101 .recon/s13-size-agent-raw/transcript_test.rs.orig
1874 .recon/s13-size-tui-resources-raw/after-peri-tui-lib.txt
1892 .recon/s13-size-tui-resources-raw/before-peri-tui-lib.txt
1029 .recon/s13-size-tui-resources-raw/full_remote_test.rs.orig
1021 .recon/s13-size-tui-resources-raw/session_events_test.rs.orig
1361 .recon/s13-size-tui-resources-raw/session_history_perf_test.rs.orig
1013 .recon/s13-size-tui-resources-raw/system.rs.orig
```

前两个是分支已跟踪日志；其余为当前未跟踪文件。不能因为没有暂存就从用户要求的集合中删去。

### 3.3 B1～B4 拆分产物逐个 wc

```text
243 peri-acp/src/host/requests_rewind_cases_test.rs
136 peri-acp/src/host/requests_rewind_cursor_expiry_test.rs
535 peri-acp/src/host/requests_rewind_effective_view_test.rs
356 peri-acp/src/host/requests_rewind_failure_paths_test.rs
288 peri-acp/src/host/requests_rewind_inherited_region_test.rs
191 peri-acp/src/host/requests_rewind_routing_test.rs
296 peri-acp/src/host/requests_unnegotiated_replay_delivery_test.rs
145 peri-acp/src/host/requests_unnegotiated_replay_fail_closed_test.rs
517 peri-acp/src/host/requests_unnegotiated_replay_test.rs
282 peri-acp/src/host/requests_unnegotiated_replay_verification_test.rs
990 peri-acp-types/src/session_resources.rs
66 peri-acp-types/src/session_resources/history.rs
805 peri-agent/src/session/transcript.rs
107 peri-agent/src/session/transcript/entry.rs
191 peri-agent/src/session/transcript/persistence_port.rs
563 peri-agent/src/session/transcript_memory_test.rs
436 peri-agent/src/session/transcript_persistence_test.rs
105 peri-agent/src/session/transcript_reminder_test.rs
33 peri-agent/src/session/transcript_test.rs
299 peri-resources/src/sessions/remote/full_remote_rewind_test.rs
740 peri-resources/src/sessions/remote/full_remote_test.rs
222 peri-resources/src/sessions/sqlite_store/session_history_perf_audit_test.rs
355 peri-resources/src/sessions/sqlite_store/session_history_perf_cases_test.rs
800 peri-resources/src/sessions/sqlite_store/session_history_perf_test.rs
502 peri-tui/src/kit/acp_events/system.rs
294 peri-tui/src/kit/acp_events/system/bg_task.rs
264 peri-tui/src/kit/acp_events/system/interaction.rs
407 peri-tui/src/kit/acp_events_test/session_compact_events_test.rs
239 peri-tui/src/kit/acp_events_test/session_events_test.rs
388 peri-tui/src/kit/acp_events_test/session_rewind_events_test.rs
```

A2 wire 拆分也实际计入完整扫描：`wire=333`、`caps=542`、`chain=168`、`initial_cursor=557`、`rejection=518`、`rewind=127` 行，均达标。

指定集合没有包含的 dirty tracked 文件，额外逐个补查：

```text
457 langfuse-client/src/batcher/recovery.rs
219 npm-packages/@peri-sdk/tests/wasm-acp-integration.test.ts
662 peri-resources/src/sessions/sqlite_store/connection_open_test.rs
```

## 4. 职责与排版抽查（3 处）

1. **Agent 条目／持久化端口**：`transcript/entry.rs:1-4` 明确条目与暂存值模型；实际为 `TranscriptEntry`、`StagedData`、受众判定及 canonical payload 转换。`persistence_port.rs:1-5` 明确前台持久化接线；实际为预算、绑定、flush、shutdown、Drop。内存寻址／staging／标记留在 `transcript.rs`。不是按第 1000 行切断一个函数。
2. **TUI 交互／后台任务**：`system/interaction.rs:1-3` 对应 HITL、AskUser、授权块构建与终态；`system/bg_task.rs:1-4` 对应任务快照、增量、身份与生命周期。与文件名一致；rewind、OAuth、系统提示留在父模块。重导出见 `system.rs:33-48`。
3. **ACP rewind 测试**：`requests_rewind_cases_test.rs:228-241` 按 effective view、cursor expiry、failure paths、routing、inherited region 五组挂载。`requests_rewind_cursor_expiry_test.rs:3-7` 的注释明确“保留锚点但结构版本过期”；其唯一测试确实先建立旧 cursor，再 rewind 并验证拒绝，符合职责名。

排版证据：对 B1～B4 各原文件与合并后的子文件做**保留空白的非注释代码行 multiset 对比**，人工核实所有剩余差异。旧函数体没有多行结构被压成一行；差异仅模块/import/re-export/可见性与等价路径资格化。移除空白再比较可能漏掉压缩，本次没有用这种宽松方式证明排版。导入列表有局部重排，不是压缩业务结构凑行数。

## 5. 语义与公开路径

### 5.1 独立 diff 核实

命令：`git diff -- <B1-B4 原始文件>`；新增子文件用当前文件内容读取。Python 从 `git show HEAD:<path>` 提取每个函数，比较拆分后同名函数；然后用上一节的整行 multiset 补查函数外类型、常量、属性。单行空函数单独处理，避免把下一个函数误归入函数体。

关键原始输出（数量包含 helper，不等于测试数）：

```text
COMPARE peri-agent/src/session/transcript.rs before 71 after 71 missing [] added [] body_changes []
COMPARE peri-tui/src/kit/acp_events/system.rs before 38 after 38 missing [] added [] body_changes []
COMPARE peri-acp/src/host/requests_rewind_cases_test.rs before 25 after 25 missing [] added [] body_changes []
COMPARE peri-acp/src/host/requests_unnegotiated_replay_test.rs before 45 after 45 missing [] added [] body_changes []
COMPARE peri-agent/src/session/transcript_test.rs before 50 after 50 missing [] added [] body_changes []
COMPARE peri-tui/src/kit/acp_events_test/session_events_test.rs before 16 after 16 missing [] added [] body_changes []
COMPARE peri-resources/src/sessions/remote/full_remote_test.rs before 14 after 14 missing [] added [] body_changes ['remote_two_full_fixture']
COMPARE peri-resources/src/sessions/sqlite_store/session_history_perf_test.rs before 38 after 38 missing [] added [] body_changes []
TYPE_BLOCK ActiveSessionSnapshot IDENTICAL True
TYPE_BLOCK RewindBoundary IDENTICAL True
TYPE_BLOCK RewindOutcome IDENTICAL True
ACCESSOR_IDENTICAL True
```

前两组函数比较只规范化首行可见性及 `super::render::` → `crate::kit::acp_events::render::`，其余字节保留。`remote_two_full_fixture` 单独显示的三处差异全部是 `super::super::{environment,session_schema,ledger}` 改为 `crate::sessions::remote::{...}`；解析到同一模块，既有初始化语句未改变，不是新增 DDL。

九组 `test`／`tokio::test`／`ignore`／`serial_test::serial` 属性 multiset 比较均 `ATTRS_UNCHANGED True`。B1～B4 未发现断言内容、条件、数据值、错误分支或同步属性被顺带改动。

### 5.2 每个生产拆分模块

| 原模块 | 公开路径与接线 | 实际验证 |
|---|---|---|
| `peri-acp-types/src/session_resources.rs` | `:248` 的 `pub use history::{ActiveSessionSnapshot, RewindBoundary, RewindOutcome}` 保持原类型路径；类型字段、derive、枚举变体与 accessor 不变 | types、agent、resources、acp、tui lib 全部编译并通过 |
| `peri-agent/src/session/transcript.rs` | `:104` 重导出 `StagedData`／`TranscriptEntry`；`MessageTranscript` 未搬家，另一个子模块中的 inherent impl 仍属于原类型；4 个私有 helper 改为 `pub(super)` 供父模块调用 | agent 全量及 acp 下游全量通过 |
| `peri-tui/src/kit/acp_events/system.rs` | 子模块私有；处理器重导出保持 `system::handle_*`、`system::request_bg_task_snapshot` 既有消费路径；render 引用资格化 | TUI lib 全量与 `check -p peri-tui --all-targets` 通过 |

边界说明：TUI 的 `apply_bg_task_snapshot`、`hitl_input_summary` 两个 **crate 内**旧别名现在仅在 `#[cfg(test)]` 下重导出，函数本体仍供子模块生产调用。当前调用方只有测试使用旧别名，native 非测试 all-targets 编译通过；这不是所有内部可见路径完全不变的证明，但没有删除 crate 外 public API。`session_resources::history` 新增 public 模块扩大了可见路径，原路径未消失。

静态搬移等价及测试通过不是所有运行条件的形式化语义证明；例如 Rust 类型的定义模块身份／`type_name` 文本不承诺不变。本任务未发现现有代码依赖这类路径文本。

## 6. 拆分测试逐文件挂载实测

每行实际执行命令模板：

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p <crate> --lib -- <代表用例名>
```

表中 `F` 是该次原始输出的 `filtered out`；每行均为 **1 passed / 0 failed / 0 ignored / 0 measured，exit 0**。audit 行在命令末尾额外加 `--ignored`，确实执行，没有把 ignored 当通过。

| crate / 文件（相对 src） | 代表用例名 | F |
|---|---|---:|
| peri-acp / host/requests_rewind_effective_view_test.rs | `test_rewind_reaches_target_archived_by_full_compact` | 892 |
| peri-acp / host/requests_rewind_cursor_expiry_test.rs | `test_rewind_expires_previously_issued_history_cursors` | 892 |
| peri-acp / host/requests_rewind_failure_paths_test.rs | `test_rewind_missing_boundary_changes_nothing_and_reports_it` | 892 |
| peri-acp / host/requests_rewind_routing_test.rs | `test_rewind_candidates_routes_to_dispatch` | 892 |
| peri-acp / host/requests_rewind_inherited_region_test.rs | `test_rewind_with_an_inherited_region_only_clears_own_archived_prefix` | 892 |
| peri-acp / host/requests_unnegotiated_replay_delivery_test.rs | `streamed_canonical_replay_delivers_the_archived_bodies_in_canonical_order` | 892 |
| peri-acp / host/requests_unnegotiated_replay_fail_closed_test.rs | `streamed_canonical_replay_reads_bounded_segments_and_never_the_full_history` | 892 |
| peri-acp / host/requests_unnegotiated_replay_verification_test.rs | `s9_verification_unnegotiated_replay_equals_canonical_history_for_mixed_payloads` | 892 |
| peri-agent / session/transcript_persistence_test.rs | `test_flush_persistence_makes_appends_visible` | 1088 |
| peri-agent / session/transcript_memory_test.rs | `test_ancestor_flags_can_restore_but_cannot_mutate` | 1088 |
| peri-agent / session/transcript_reminder_test.rs | `audience_matrix_splits_model_projection_from_client_snapshot` | 1088 |
| peri-tui / kit/acp_events_test/session_events_test.rs | `test_transcript_publication_preserves_stable_history_and_resets_hint` | 1864 |
| peri-tui / kit/acp_events_test/session_compact_events_test.rs | `test_auto_compact_completed_injects_detailed_system_note` | 1864 |
| peri-tui / kit/acp_events_test/session_rewind_events_test.rs | `test_rewind_completed_replaces_committed` | 1864 |
| peri-resources / sessions/remote/full_remote_test.rs | `remote_close_intent_finishes_without_execution_ownership` | 524 |
| peri-resources / sessions/remote/full_remote_rewind_test.rs | `remote_rewind_between_two_fulls_restores_every_retained_body` | 524 |
| peri-resources / sessions/sqlite_store/session_history_perf_audit_test.rs | `audit_segment_lookahead_and_plan_shape --ignored` | 524 |
| peri-resources / sessions/sqlite_store/session_history_perf_cases_test.rs | `history_segment_statements_are_index_located` | 524 |
| peri-acp / host/history_page_initial_cursor_test.rs（A2 补查） | `initial_load_cursor_walks_the_archived_range_without_overlapping_the_first_screen` | 892 |
| peri-acp / host/history_page_chain_test.rs（A2 补查） | `the_same_cursor_replays_equivalent_pages_and_limit_only_changes_page_boundaries` | 892 |
| peri-acp / host/history_page_rejection_test.rs（A2 补查） | `a_page_request_is_answered_while_the_session_holds_the_execution_lock` | 892 |
| peri-acp / host/history_page_rewind_test.rs（A2 补查） | `an_expired_cursor_is_rejected_without_an_initial_load` | 892 |
| peri-acp / host/history_page_caps_test.rs（A2 补查） | `s9_recheck_unnegotiated_loop_load_delivers_full_history_without_private_fields` | 892 |

关键原始输出片段：

```text
test host::requests::tests::rewind_cases::cursor_expiry_tests::test_rewind_expires_previously_issued_history_cursors ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 892 filtered out; finished in 0.07s
test session::transcript::tests::persistence_tests::test_flush_persistence_makes_appends_visible ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1088 filtered out; finished in 0.69s
test sessions::sqlite_store::session_data::history_perf_tests::audit::audit_segment_lookahead_and_plan_shape ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.99s
```

完整逐命令、完整测试名与 result 原始记录：
`/var/folders/d5/gpfmkm2s4sqgwz5wwnj44p500000gn/T/peri-tool-output-9725899d-0c21-417e-9af7-23e22c160356.txt`。

以下父文件拆后只有 helper／挂载，没有自有 `#[test]`：`requests_rewind_cases_test.rs`、`requests_unnegotiated_replay_test.rs`、`transcript_test.rs`、`session_history_perf_test.rs`、`history_page_wire_test.rs`。没有为这些文件伪造代表用例；子测试的完整路径证明父挂载已生效。B1 保留 `rewind_cases`／`unnegotiated_replay_tests` 前缀，Agent 保留 `session::transcript::tests`，perf 保留 `history_perf_tests`。仅子层级增加，不存在未挂载新测试文件。

## 7. 独立全量、基线与下游

### 7.1 五 crate lib 全量

分别实际执行：`./scripts/cargo-rmcp-patched.sh test --locked -p <crate> --lib`。默认测试并发，不加过滤或忽略参数。五次 exit 均 0。

```text
peri-acp:
test result: ok. 893 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 73.31s
peri-agent:
test result: ok. 1089 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.26s
peri-acp-types:
test result: ok. 576 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
peri-tui:
test result: ok. 1859 passed; 0 failed; 6 ignored; 0 measured; 0 filtered out; finished in 5.95s
peri-resources:
test result: ok. 499 passed; 0 failed; 26 ignored; 0 measured; 0 filtered out; finished in 16.02s
```

| crate | 基线核对 | 结论 |
|---|---|---|
| peri-acp | S12 890 → 当前 893 | +3 是 A2；B1 两组原测试不增不减 |
| peri-resources | 499 passed / 26 ignored → 同数 | 一致 |
| peri-tui | 1859 passed / 6 ignored → 同数 | 一致 |
| peri-agent | 从 HEAD 源码独立计数 test 属性 1089；当前运行 1089 | 静态基线与运行数一致；本轮未回滚重跑旧源码 |
| peri-acp-types | 从 HEAD 源码独立计数 test 属性 576；当前运行 576 | 静态基线与运行数一致；本轮未回滚重跑旧源码 |

ACP 增量来源：直接对比 HEAD 与当前 wire 子模块集合，旧 16 条 → 21 条（+5，无删除）；fork 旧 3 条 → 4 条（+1，无删除）。其中 S12 已有 `a_rejected_page_request_never_runs_an_initial_load`、`an_expired_cursor_is_rejected_without_an_initial_load` 与 fork 的 `a_parent_full_after_fork_leaves_the_child_snapshot_and_pages_intact` 共 3 条，权威记录见 `S12-gap-tests.md:22-26`。余下本轮 3 条是：

1. `a_page_request_for_a_closing_session_is_rejected_without_a_replay`
2. `a_page_request_for_a_session_outside_the_live_set_is_rejected_without_creating_it`
3. `a_page_request_without_a_session_id_is_rejected_without_an_initial_load`

因此不能把 893 报成“与 890 数字完全一致”，也不能误判为规模拆分偷偷增测。B1 的 rewind 16 条、unnegotiated replay 15 条；Agent transcript 44 条；TUI session events 16 条；remote 7 条；perf 6 条（含 5 ignored）均由源码独立核实守恒。

### 7.2 非测试构建与文档测试

```text
COMMAND ./scripts/cargo-rmcp-patched.sh check --locked -p peri-tui --all-targets
    Checking peri-tui v0.2.0 (.../.worktrees/peri-transcript-history/peri-tui)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 56.80s
EXIT 0
```

为补 public 文档搬移，另分别执行 `test --locked -p <crate> --doc`：

```text
peri-acp-types: test result: ok. 1 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 2.71s
peri-agent: test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.25s
peri-tui: test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

TUI 无 doc tests，明确属于 0 用例，不能当作测试覆盖；不影响 §6 真测试挂载结果。doc 命令总耗时超过前台限制后由工具转后台，已读取完成日志，不是仅凭启动认定通过。原始输出：`/var/folders/d5/gpfmkm2s4sqgwz5wwnj44p500000gn/T/peri-foreground-shell-d298d07e-b662-421e-8665-a5a3f254025f-stdout.log`。types doc 构建有依赖 `hyper-util` 的 `unused variable: timer` warning，非失败。

## 8. 风险、越界与未验证项

1. **明确失败**：完整文件集合仍超限；`git diff --check` 另有格式失败。实际输出：
   ```text
   peri-acp/src/host/requests_unnegotiated_replay_test.rs:517: new blank line at EOF.
   [Exit code: 2]
   ```
   本复核不帮实施者消除该空行，也未修改／删除证据副本。
2. **未发现 B1～B4 越界行为改动**：保留原断言、常量、测试属性和生产逻辑。当前工作树另有 S12/A1/A2/A3 改动，不能把整个 HEAD→工作树 diff 宣称为“语义零改动”。特别是锁测试重写、WASM 测试夹具及 recovery.rs 变化不属于本次纯拆分等价结论。
3. **内部 API 注意项**：TUI 两个原 crate 内别名现在为 test-only 重导出，见 §5.2；现有消费者编译通过。没有额外构造外部 crate 验证全部理论 API；TUI system 原本不是 crate 外 public API。
4. **没有源码故障注入／还原操作**：本复核不需要临时修改源码。B1～B4 原始副本与 HEAD 相同，当前剩余差异无注入逻辑；没有复做 A1 锁确定性证伪或 A3 WASM 红绿实验，不能替代它们的专项验收。
5. **测试局限**：每 crate 全量各 1 轮；Resources 26、TUI 6 个 ignored 在默认全量中未执行，只有 perf audit 又显式执行一次。perf cases 选默认结构断言验证挂载，没有重跑百万行性能矩阵；不能用本报告改变 A05 性能结论。未验证 WASM、远端真实网络服务或 TUI 人工视觉行为。
6. **基线局限**：Agent/types 的拆前基线是 HEAD 源码属性计数加搬移对比，不是本轮旧版本运行成绩；原工作树禁止 checkout，本复核没有覆盖未提交内容来重跑。S12 的 ACP/Resources/TUI 基线来自用户及权威记录，本轮当前成绩均为独立执行。
7. **证据存续**：报告包含判定所需关键原始输出；完整 wc 与过滤日志指向当前机器临时目录，可能被系统清理。未另写未授权的报告／脚本文件。未执行全仓库规模扫描，因此不宣称全库规模通过。

VERDICT: SIZE-GOVERNANCE-FAIL
