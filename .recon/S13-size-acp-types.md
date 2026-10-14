# S13 · B2：`peri-acp-types/src/session_resources.rs` 规模拆分（历史读取 / rewind 类型下沉子模块）

工作树（git worktree）：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
分支 `feat/transcript-shrink-history-pagination`，HEAD `3ccf75864`（工作树含上一轮 S12/S13 未提交改动）。
（每条命令均在该工作树为 cwd；Rust 命令一律 `./scripts/cargo-rmcp-patched.sh`。原始输出留档在 `.recon/b2-raw/`。）

---

## 1. 结论

1. **拆分完成**：本交付新增的历史读取 / rewind 契约类型下沉到 `peri-acp-types/src/session_resources/history.rs`（66 行）。父模块 1034 → **990 行**，两个文件均 ≤1000（STD-SIZE-001）。
2. **公开路径不变**：父模块 `pub use history::{ActiveSessionSnapshot, RewindBoundary, RewindOutcome};`，`crate::session_resources::X` 与 `peri_acp_types::session_resources::X` 照旧可用；下游（peri-acp / peri-agent / peri-resources）不改一行（这三个 crate 是仓库内引用这三个类型名的全部位置，`grep -rln` 核过）。
3. **语义零改动**：只搬移 + 重挂载。行级守恒证明：输出两个文件相对原文**只多 25 行**（子模块声明、`pub use`、父模块 3 行说明注释、子模块文档 8 行与导入 5 行、4 个空行，含 3 行改写后的链接行）、**只少 3 行**（3 处 intra-doc 链接加 `super::` 前缀后原文行被替换）；未改任何签名、行为、序列化形状或断言。
4. **测试全绿且与拆分前逐项一致**：`peri-acp-types --lib` 576 → **576**；`peri-acp --lib` 893 → **893**。过滤跑 `session_resources::tests::` = **8 passed / 568 filtered out**（真跑，非空跑）。
5. **范围合规**：只改 `peri-acp-types/src/session_resources.rs`（+6 / −50）并新增 `peri-acp-types/src/session_resources/history.rs`；`git status` 中其余改动均属其他任务。
6. **存量超限如实报告**：全库扫描 2333 个文件，超限 6 个（源码 2 / 测试 4），均不在本次范围（复核轮 13:12 重跑：2340 文件 / 超限 2 个，差异来自其他 S13 任务并发完成，见 §9.10）。
7. **复核轮（本轮）**：本轮不采信同树脚本，改用「HEAD 即拆前基线」做独立逐字节复核，并重跑全部测试与门禁，结论不变（见 §9）。

---

## 2. 拆分设计（按职责，不按行数）

本交付在该文件新增 230 行（`origin/main` 807 → 1034），其中**能动的只有类型定义**：trait 方法、enum 变体在 Rust 里不能跨文件，所以拆分的落点必须落在类型上。

| 类型 | 归属 | 位置 |
| --- | --- | --- |
| `ActiveSessionSnapshot`（本交付新增，有界恢复的读取结果） | 读取结果 | 原文 241-259 → 子模块 |
| `RewindBoundary`（既有，rewind 边界） | 定向更新 | 原文 263-279 → 子模块（与 rewind 效力同族，收口在一处） |
| `RewindOutcome`（本交付新增，rewind 实际效力） | 定向更新 | 原文 281-292 → 子模块 |

拆不出去的部分（留在父模块，故父模块只降到 990）：`SessionResources::{load_active_snapshot, load_history_page, load_history_segment, history_page_bounds, load_entry_positions, load_message_flags}` 及其文档、`rewind_history` 的新签名、`SessionResourceErrorKind::Cursor` 变体——trait 与 enum 定义不能跨文件。

子模块名叫 `history`：它是「历史读取视图 + rewind 边界/效力」这一族的落点，与 `crate::history_page`（分页装配规则）、`crate::store::history`（纯变换）互不重叠。

---

## 3. 落盘文件与行数（`wc -l` 逐个证明）

```
$ wc -l peri-acp-types/src/session_resources.rs peri-acp-types/src/session_resources/history.rs
     990 peri-acp-types/src/session_resources.rs
      66 peri-acp-types/src/session_resources/history.rs
    1056 total
```

| 文件 | 行数 | sha256 | 职责 |
| --- | --- | --- | --- |
| `peri-acp-types/src/session_resources.rs`（父，拆前 1034） | **990** | `bcf6e67bd9d1295f4b8dd36ede995fb2d06644317952e1b0082b2aca0e57c1ae` | 门面：能力与准入 / 领域输入 / 读取结果（除 `ActiveSessionSnapshot`）/ 定向更新（除 rewind 两个类型）/ 结果与错误 / 四个 trait + `pub use` 重导出 |
| `peri-acp-types/src/session_resources/history.rs`（子，新增） | **66** | `768c0c46e260122f15047fdb136c28be71a1bea232f67b5e96f9f25f4587b7b5` | 历史读取视图与 rewind 契约类型：`ActiveSessionSnapshot` / `RewindBoundary`(+`message_id`) / `RewindOutcome` |
| 拆前父模块（基线，留档） | 1034 | `2dcfd2a8652f8c4b98389742f4f0fa43b2383aa8dbbdde59725c1dfc49ffa71b` | `.recon/b2-raw/parent-before.sha256` |

改动面（`git diff --stat`，范围内唯一改动）：

```
 peri-acp-types/src/session_resources.rs | 56 ++++-----------------------------
 1 file changed, 6 insertions(+), 50 deletions(-)
```

---

## 4. 公开路径保持方式

1. 挂载：父模块顶部第 38-40 行（两条说明注释 + `pub mod history;`）（Rust 2018 文件模块 + 同名目录，仓库既有先例：`session.rs` + `session/`、`store/mod.rs` + `store/history.rs`）。
2. 重导出：父模块第 248 行 `pub use history::{ActiveSessionSnapshot, RewindBoundary, RewindOutcome};`（就在原「定向更新」分区标题下，即类型原位置），`crate::session_resources::X` 解析到同名条目，与拆前等价。
3. 下游证据（拆分后仍全部编译并跑测，未改下游一行）：
   - `peri-acp/src/host/requests/rewind.rs:32` `use peri_acp_types::session_resources::{RewindBoundary, SessionSnapshot};`
   - `peri-acp/src/host/requests_legacy_test.rs:935` `SessionResourceResult<peri_acp_types::session_resources::ActiveSessionSnapshot>`
   - `peri-acp/src/session/command/rewind.rs:32` `use peri_acp_types::session_resources::RewindOutcome;`
   - `peri-agent/src/session/transcript.rs:23`、`peri-resources/src/sessions/remote/*.rs` 同形引用
   - `grep -rln 'ActiveSessionSnapshot\|RewindOutcome\|RewindBoundary' --include=*.rs` → 仅 `peri-acp` / `peri-acp-types` / `peri-agent` / `peri-resources` 四个 crate
4. 子模块内部对父模块条目的引用（`ActiveSessionSnapshot` 文档链接、`BindingState`/`FrozenState` 字段类型）经 `super::` 解析，不产生环。

---

## 5. 语义零改动证据

拆分脚本：`.recon/b2-raw/split.py`（含锚点断言与行级守恒断言，退出码 0）；运行日志 `.recon/b2-raw/split-run.log`。

1. **锚点断言**：搬移前后逐行核对 16 个结构锚点（`}` / 分区标题 / 类型首行等），任一不符即中止。
2. **切片可拼回**：`head + body_a + blank240 + 三段搬出区间 + 三个分区空行 + tail == 原文 1034 行`（脚本断言通过，证明切分无遗漏、无重复）。
3. **行级守恒**（`Counter` 差值，日志原文）：

```
仅新增的行 25 条：+4 '' / +2 '//!' / 子模块文档正文 6 行 / 导入 5 行 / 'pub mod history;' /
                 'pub use history::{ActiveSessionSnapshot, RewindBoundary, RewindOutcome};' /
                 父模块 3 行说明注释 / 3 行改写后的链接行
仅消失的行 3 条：3 行原文链接行（[`SessionSnapshot`]、[`SessionResources::load_session_history`]、[`SessionSnapshot::payloads`]）
```

4. **唯一正文改写**：搬进子模块的 `ActiveSessionSnapshot` 文档里 3 处 intra-doc 链接加 `super::` 前缀（`[`super::SessionSnapshot`]` 等），否则在子模块作用域内链接失效。只是文档链接定址，不是语义、签名或序列化改动。
5. **未破坏契约**：`#[derive]` 集合、字段名/类型、enum 变体、方法签名一字未动（见 `.recon/b2-raw/parent.diff`）。
6. **rustdoc 告警逐条定位**（`.recon/b2-raw/after-downstream2.log`）：`cargo doc -p peri-acp-types --no-deps` 共 13 条告警，来源文件逐条为 `command_name.rs`(2)、`command_registry.rs`、`command.rs`、`compact.rs`、`peri_caps.rs`、`plugin.rs`(2)、`runtime.rs`、`command_route.rs`、`permission.rs`(3)——**没有一条落在 `session_resources.rs` 或 `session_resources/history.rs`**，即子模块里的 `super::` 文档链接被正常解析。

---

## 6. 测试证据

### 6.1 过滤跑（挂载真实成立，不是空跑）

```
$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib -- session_resources::tests::
running 8 tests
test session_resources::tests::access_mode_capabilities_and_execution_are_independent_facts ... ok
test session_resources::tests::frozen_snapshot_bytes_stay_opaque ... ok
test session_resources::tests::rewind_boundary_reports_its_target ... ok
test session_resources::tests::error_display_keeps_effect_but_leaks_no_identity ... ok
test session_resources::tests::meta_patch_distinguishes_untouched_from_cleared ... ok
test session_resources::tests::workspace_failure_keeps_local_semantics_and_source ... ok
test session_resources::tests::persistence_uncertain_is_the_only_unknown_effect ... ok
test session_resources::tests::load_history_segment_defaults_to_unsupported ... ok
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 568 filtered out; finished in 0.00s
```

（原始输出：`.recon/b2-raw/after-filtered-session-resources.log`；本 crate 共 576 条用例，过滤后 8 条实跑、568 条被过滤——不是 0 passed 的空跑。）

其中 `rewind_boundary_reports_its_target` 直接调用搬到子模块的 `RewindBoundary::message_id`，`load_history_segment_defaults_to_unsupported` 的 mock 实现用 `RewindBoundary` / `RewindOutcome` 签名——经父模块 `pub use` 解析到子模块条目，证明挂载真实生效。

### 6.2 全量 lib（拆分前 / 拆分后）

| 命令 | 拆分前 | 拆分后 | 结果 |
| --- | --- | --- | --- |
| `... test --locked -p peri-acp-types --lib` | 576 passed / 0 failed | **576 passed / 0 failed / 0 filtered out** | 一致 |
| `... test --locked -p peri-acp --lib`（下游消费者） | 893 passed / 0 failed | **893 passed / 0 failed / 0 filtered out** | 一致 |

原始输出：`.recon/b2-raw/peri-acp-before.txt`、`.recon/b2-raw/after-full-tests.log`。

> 与任务书基线对照：任务书写「peri-acp 890」，实际本工作树拆分前是 893——差 3 条来自上一轮 S13 wire 拆分新增的 R6/R9/R10 用例（见 `.recon/S13-wire-split.md`），与本拆分无关；本次以「拆分前 == 拆分后」判定。

### 6.3 下游编译（超出必须项的加固）

```
$ ./scripts/cargo-rmcp-patched.sh check --locked -p peri-agent -p peri-resources --lib
    Checking peri-agent v0.2.0 (.../peri-agent)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 28.03s

$ ./scripts/cargo-rmcp-patched.sh check --locked -p peri-agent -p peri-resources --all-targets
    Checking peri-agent v0.2.0 (.../peri-agent)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 58.72s
```

下游（peri-acp 全量用例 + peri-agent / peri-resources 的 lib 与测试目标）在拆分后全部编译通过，未改下游任何一行。

### 6.4 规模门

```
$ bash scripts/check-file-size.sh
❌ 源码超阈值（Top 30）：
    1637  peri-tui/src/kit/panels/mcp.rs
    1013  peri-tui/src/kit/acp_events/system.rs
❌ 测试超阈值（Top 30）：
    1361  peri-resources/src/sessions/sqlite_store/session_history_perf_test.rs
    1038  peri-resources/src/sessions/sqlite_store/schema_test.rs
    1029  peri-resources/src/sessions/remote/full_remote_test.rs
    1021  peri-tui/src/kit/acp_events_test/session_events_test.rs
共扫描 2333 个文件，超阈值 6 个（源码 2 / 测试 4）
```

本次两个文件均不在超限清单中（990 / 66）。

复核轮重跑（13:12）：扫描 2340 文件 / 超限 2 个（`peri-tui/src/kit/panels/mcp.rs` 1637、`peri-resources/src/sessions/sqlite_store/schema_test.rs` 1038），本例两文件仍未入列；与上表的 6 个差异来自其他 S13 任务并发完成（见 §9.10）。

---

## 7. 未验证项与局限

1. **余量只有 10 行（990/1000）**：本交付 230 行里的大头（6 个 trait 方法及文档、`rewind_history` 签名、`SessionResourceErrorKind::Cursor`）因 Rust 的 trait/enum 跨文件限制无法搬出。若该文件再增长，需另立拆分任务（自然落点是「结果与错误」整段下沉为 `session_resources/error.rs`，约 190 行）；本次未做，以免超出授权范围。
2. **rustdoc 未做拆前/拆后逐条对照**：拆后 doc 输出 13 条告警已逐条定位到其他模块（见 §5.6），无一条指向被搬移类型、子模块或 `super::` 链接；但未跑拆前那一次做严格集合比对。→ 复核轮已补等价证据：doc 全量输出中 `session_resources` 出现次数为 **0**（§9.7）。
3. **并发干扰**：同一工作树内有其他任务在并发改文件（`peri-acp`、`peri-agent`、`peri-resources` 的测试与 `peri-tui`）。`peri-acp` 的两次全量跑之间隔了约 8 分钟，实测前后同为 893，未见差异；但若其它任务在两次测量之间落地新用例，对照会失真——本次未观察到该情况。→ 复核轮 13:10–13:12 第三次重跑仍为 893 / 576，未观察到漂移（§9.6）。
4. **未跑**：doc tests（未改 doc 示例）、E2E、workspace 全量测试（超出本任务范围）。
5. **未提交**：遵循任务约束，未 `git add` / `git commit`，改动以工作树状态交付。
6. 公开面新增了一个公共子模块路径 `peri_acp_types::session_resources::history`（仓库既有先例 `crate::store::history`、`command::command_registry`），属加性变化，未移除或改名任何既有条目。
7. `split.py` 只对拆前的 1034 行版本有效，**不可对已拆分的树复跑**（结构锚点会断言失败）；本次复跑前用 `git show HEAD:peri-acp-types/src/session_resources.rs` 还原原文，还原后的 sha256 与留档一致（`2dcfd2a8…`）。

---

## 8. 证据留档

| 路径 | 内容 |
| --- | --- |
| `.recon/b2-raw/split.py` | 拆分脚本（锚点 + 守恒断言，退出码 0） |
| `.recon/b2-raw/split-run.log` | 拆分运行与守恒报告原始输出 |
| `.recon/b2-raw/parent-before.sha256` / `.wc` | 拆前父模块 sha256 / 行数 |
| `.recon/b2-raw/parent.diff` | 父模块最终 patch（+6 / −50） |
| `.recon/b2-raw/diff-vs-origin-main.txt` | 本交付对该文件的全部新增（230 行）对照 |
| `.recon/b2-raw/peri-acp-before.txt` | 拆前 `peri-acp --lib` 全量输出 |
| `.recon/b2-raw/after-full-tests.log` | 拆后两个 crate 的全量输出 |
| `.recon/b2-raw/after-filtered-session-resources.log` | 拆后过滤跑（8 passed / 568 filtered out） |
| `.recon/b2-raw/after-downstream.log` / `after-downstream2.log` | doc / 下游 check 输出 |
| `.recon/b2-verify/verify.log` | 复核轮：过滤跑 + 两个 crate 全量 lib（13:10–13:11） |
| `.recon/b2-verify/verify2.log` | 复核轮：doc / 下游 `--all-targets` check / fmt |
| `.recon/b2-verify/doc-full.log` | 复核轮：rustdoc 全量输出（13 条告警定位） |
| `.recon/b2-verify/byte-equivalence.py` / `.log` | 复核轮：diff 删除行 vs 子模块正文逐字节比对 |
| `.recon/b2-verify/size-gate.log` / `wc.log` | 复核轮：规模门重跑 / 两个文件行数 |

## 9. 独立复核轮（2026-10-14 13:10–13:12，本轮新增）

**方法**：不采信同树既有脚本的断言。关键事实：`git show HEAD:peri-acp-types/src/session_resources.rs` = 1034 行、sha256 `2dcfd2a8…`，与 §3 留档的拆前基线一致 → `git diff HEAD` 就是本次拆分的**全部**改动面，可被逐字节核对。

1. **改动面只有搬运**（`git diff HEAD -- peri-acp-types/src/session_resources.rs`，共 2 个 hunk / `+6 −50`）：删除 50 行 = 3 个类型块；新增 6 行 = 3 行说明注释 + `pub mod history;` + 1 空行 + `pub use history::{ActiveSessionSnapshot, RewindBoundary, RewindOutcome};` —— 无一行含逻辑。
2. **逐字节等价**（独立脚本 `.recon/b2-verify/byte-equivalence.py`，比对 diff 删除行 vs 子模块第 17–66 行，仅归一化 `super::`）：

```
removed lines from parent diff : 50
added   lines to   parent diff : 6
child body lines (17..66)      : 50
removed vs child-body identical (after stripping 'super::'): True
--- 'super::' rewrites inside child body: 3 lines ---
```

→ 被搬走的 50 行与子模块正文**逐字节相同**；全局唯一文本差异是 3 处文档链接加 `super::` 前缀（日志 `.recon/b2-verify/byte-equivalence.log`）。

3. **契约形状对齐**：derive 集合与 HEAD 原文逐一对上（`ActiveSessionSnapshot` = `Clone, Debug` @249；`RewindBoundary` / `RewindOutcome` = `Clone, Copy, Debug, PartialEq, Eq` @267 / @286）。全仓 grep：这三个类型无 `Serialize`/`Deserialize` 实现、无 `type_name` 依赖 → 模块路径变化对消费方不可观察。
4. **行数**（`.recon/b2-verify/wc.log`）：`990` 父 / `66` 子，均 ≤1000。
5. **范围**：`git status --short peri-acp-types/` → 仅 ` M peri-acp-types/src/session_resources.rs`、`?? peri-acp-types/src/session_resources/`。
6. **挂载 + 全量（本轮重跑，`.recon/b2-verify/verify.log`）**：

| 命令 | 关键输出 |
| --- | --- |
| `test --locked -p peri-acp-types --lib -- session_resources::tests::` | `running 8 tests` … `ok. 8 passed; 0 failed; … 568 filtered out` |
| `test --locked -p peri-acp-types --lib` | `ok. 576 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out` |
| `test --locked -p peri-acp --lib` | `ok. 893 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`（76.55s） |

   基线口径：任务书的「peri-acp 890」是 S13 wire 拆分**前**的数字，wire 拆分新增 3 条后为 893（反证：`.recon/S13-wire-split.md` 的 `890 passed; 3 filtered out`）；B2 未改任何测试源文件，计数结构性不变，本轮重跑亦为 893。

7. **rustdoc 严格核对**（全量输出 `.recon/b2-verify/doc-full.log`）：`DOC_EXIT=0`；`warning: peri-acp-types (lib doc) generated 13 warnings`，逐条定位为 `command_name.rs`×2 / `command_registry.rs` / `command.rs` / `compact.rs` / `peri_caps.rs` / `plugin.rs`×2 / `runtime.rs` / `command_route.rs` / `permission.rs`×3（另 1 条属 hyper-util 依赖补丁）；全量输出中 `grep -c session_resources` = **0** → 子模块 3 处 `super::` 链接全部解析成功，补齐 §7.2。
8. **格式门**：`fmt -p peri-acp-types -- --check` → `FMT_EXIT=0`、0 个 `Diff in` 块（`.recon/b2-verify/fmt-full.log`）；`rustfmt --edition 2021 --check <两文件>` → 退出 0。
9. **下游编译**：`check --locked -p peri-agent -p peri-resources --all-targets` → `Finished … in 14.26s`，无错误（`.recon/b2-verify/verify2.log`）。
10. **规模门重跑**（`.recon/b2-verify/size-gate.log`）：`bash scripts/check-file-size.sh` → 扫描 2340 文件，超限 2 个（`peri-tui/src/kit/panels/mcp.rs` 1637、`peri-resources/src/sessions/sqlite_store/schema_test.rs` 1038），本例两文件未入列；`SIZE_GATE_EXIT=1` 由这两个存量超限文件导致，与本任务无关（与 §6.4 的 6 个差异来自其他 S13 任务并发完成，见 `S13-size-tui-resources.md` / `S13-size-acp-tests.md`）。

### 9.1 复核轮局限（叠加 §7，不替代）

1. 未复跑 `.recon/b2-raw/split.py` 的锚点断言（该脚本对已拆树不可复跑，见 §7.7），改用上述独立方法；两者结论一致但方法独立。
2. `peri-acp` 的「拆前 893」取自同轮留档而非本轮新测的旧二进制；依据是 B2 未触碰任何测试源文件，计数结构性不变（§9.6）。
3. 本轮为静态 + 单元级验证；未跑 doc tests、E2E、workspace 全量（超出范围）。
4. 工作树内仍并存其他任务的未提交改动；本轮数字取自 2026-10-14 13:10–13:12 窗口，不代表窗口外状态。

---

VERDICT: SIZE-SPLIT-DONE
