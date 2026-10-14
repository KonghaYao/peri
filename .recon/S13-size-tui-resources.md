# S13 · peri-tui / peri-resources 规模拆分（任务 B4）

工作树（git worktree）：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
分支 `feat/transcript-shrink-history-pagination`，HEAD `3ccf75864`。
（每条命令均以该工作树为 cwd；Rust 构建/测试一律用 `./scripts/cargo-rmcp-patched.sh`。）

---

## 1. 结论

1. **拆分完成**：4 个超限文件（1013/1021/1361/1029 行）拆为 **11 个文件**，逐个 `wc -l` 证明全部 ≤1000 行（最大 800）。
2. **职责边界**：生产 `system.rs` 按「交互弹窗/授权」「后台任务」两块切出子模块，父只留系统提示/提醒、rewind/OAuth、plugin/workflow 与共享头部；三个测试文件按用例族（rewind / compact、perf cases / audit、remote rewind）切分，父模块只留共享夹具与文档。
3. **语义零改动**：11 个搬移块逐块字节比对，除 16 行**声明项**（`pub(super)→pub(crate)`、跨模块路径限定）外**逐行字节一致**；原行去向覆盖检查 4/4 通过。
4. **公开路径保持**：`system.rs` 用 `pub use` 重导出 8 个被搬移函数，既有调用路径 `system::handle_bg_task_*`、`system::request_bg_task_snapshot`、`system::handle_{hitl_pending,ask_user,interaction_terminal}` 不变（含 2 个 `#[cfg(test)]` 门控的测试引用路径）。
5. **挂载真实成立**：10 条过滤运行全部 **`EXIT=0` 且非空跑**（最少 1 passed / 522 filtered out），子模块过滤命中 5 / 4 / 3 passed，与拆分前父模块 16 / 7 passed 的分解数吻合。
6. **全量绿且数量一致**：`-p peri-tui --lib` 拆分前 `1859 passed; 0 failed; 6 ignored` → 拆分后**同数字**；`-p peri-resources --lib` 拆分前 `499 passed; 0 failed; 26 ignored` → 拆分后**同数字**，全部 `EXIT=0`。
7. **门禁干净**：`fmt --check -p peri-tui -p peri-resources` → 0 diff / `EXIT=0`；`check --lib --tests` → 0 warning / `EXIT=0`；`clippy -- -W clippy::all` → `EXIT=0`（仅 2 条**既有** warning，与本任务无关，见 §8）。
8. **未触碰范围外文件**：`git status --short` 中本任务仅 4 改 + 6 新增 + 1 新目录；`git diff --stat`（4 个目标）＝ `51 insertions(+), 2194 deletions(-)`，无其它文件被本任务修改。
9. **证据目录**：`.recon/s13-size-tui-resources-raw/`（含 `.orig` 原文快照、拆分/校验脚本、前后测试原始输出、挂载证明、规模扫描）。

---

## 2. 改动清单与拆分映射

（区间均为原文 1-based 闭区间；`system.rs` 原文 1013 行，`session_events_test.rs` 1021 行，`session_history_perf_test.rs` 1361 行，`full_remote_test.rs` 1029 行。）

### 2.1 生产文件

| 文件 | 行数 | 职责 | 来自原 `system.rs` |
| --- | --- | --- | --- |
| `peri-tui/src/kit/acp_events/system.rs`（父） | 502 | 重写头部（模块文档 + import 重分配 + 挂载 + `pub use`）、系统提示/提醒、rewind/OAuth 结果、plugin/workflow 事件 | 33-237、491-663、940-1013 |
| `.../acp_events/system/interaction.rs` | 264 | Interaction 事件：`HitlPending` / `AskUser` 弹窗与授权块 | 239-489 |
| `.../acp_events/system/bg_task.rs` | 294 | §4.7 Background Tasks：快照/增量与任务生命周期 | 665-938 |

### 2.2 测试文件

| 文件 | 行数 | 职责 | 来自原文 |
| --- | --- | --- | --- |
| `peri-tui/src/kit/acp_events_test/session_events_test.rs`（父） | 239 | 发布/视图/计划/预测 + 子模块挂载 | 1-233 |
| `.../acp_events_test/session_rewind_events_test.rs` | 388 | `RewindCompleted` 与跨 turn reasoning | 234-618 |
| `.../acp_events_test/session_compact_events_test.rs` | 407 | Compact 生命周期 | 619-1021 |
| `peri-resources/src/sessions/sqlite_store/session_history_perf_test.rs`（父） | 800 | 文档 + 夹具 + 计划证据 + 统计口径 | 1-791 |
| `.../sqlite_store/session_history_perf_cases_test.rs` | 355 | A04–A07 场景用例 | 792-1143 |
| `.../sqlite_store/session_history_perf_audit_test.rs` | 222 | verification 审计补充 | 1144-1361 |
| `peri-resources/src/sessions/remote/full_remote_test.rs`（父） | 740 | 冷恢复 / 过滤契约 | 1-736 |
| `.../remote/full_remote_rewind_test.rs` | 299 | 远端 rewind 有效视图 | 737-1029 |

### 2.3 挂载方式

1. `system.rs`：Rust 2018+ 原生文件树子模块 `mod interaction;` / `mod bg_task;`（新目录 `peri-tui/src/kit/acp_events/system/`），配 `pub use` 重导出保持既有路径。
2. `session_events_test.rs`：父文件由 `acp_events/mod.rs:698-699` 以 `#[path = "../acp_events_test.rs"] mod acp_events_test;` 装载；两个子文件用同目录 `#[path = "..."] mod ...;` 挂载 + `use super::*;`。
3. perf 三文件：父由 `sqlite_store/session_data.rs:949-950` 的 `#[path = "session_history_perf_test.rs"] mod history_perf_tests;` 装载；子模块 `mod cases;` / `mod audit;` + `#[path]`。
4. `full_remote_*`：父由 `remote/schema_upgrade_test.rs:912-913` 的 `mod full_remote_tests { include!("full_remote_test.rs"); }` 装载；子文件 `#[path = "full_remote_rewind_test.rs"] mod full_remote_rewind_tests;`。

### 2.4 非搬移改动清单（全部为声明项，逐条对应 §3 校验规则）

1. `system.rs` 父头部重写：模块文档补「模块结构」说明；import 按归属重新分配（子模块各自补齐自身 import）；新增挂载与 `pub use` 块。
2. 两个新子模块内 8 个被重导出函数 `pub(super) fn` → `pub(crate) fn`（`pub(super)` 在子模块内仅父模块可见，不足以支持父模块重导出；私有 `mod` 链封顶可达集合，实际可见范围不变）。
3. 子模块内 5 处 `super::render::` → `crate::kit::acp_events::render::`（`interaction.rs` 4 处 + `bg_task.rs` 1 处；路径限定显式化，指向同一 item）。
4. `full_remote_rewind_test.rs` 内 3 处 `super::super::` → `crate::sessions::remote::`（该文件父是 `schema_upgrade_tests`，`super::super` 即 `remote`）。
5. rustfmt 排序修正（`request_bg_task_snapshot` 重导出位置、`BG_TASK_REVISION`/`BG_TASKS` 字母序、`#[path] mod` 按模块名排序）；perf 父模块文档尾部追加 3 行「模块结构」说明。

---

## 3. 语义零改动证据

### 3.1 逐块字节比对

脚本：`python3 .recon/s13-size-tui-resources-raw/verify_moves.py`（对同目录 `.orig` 快照与当前文件比对，允许差异仅限 §2.4 声明项）。
输出（`.recon/s13-size-tui-resources-raw/verify-moves.txt`，本次重跑 `EXIT=0`）：

```
system P1 系统提示/提醒        原文 33-237 (205 行) -> system.rs(parent):49   声明差异行 0  规则命中 0
system interaction           原文 239-489 (251 行) -> system/interaction.rs:14  声明差异行 7  规则命中 7
system P2 rewind/OAuth       原文 491-663 (173 行) -> system.rs(parent):255  声明差异行 0  规则命中 0
system bg_task               原文 665-938 (274 行) -> system/bg_task.rs:21   声明差异行 6  规则命中 6
system P3 plugin/workflow    原文 940-1013 (74 行) -> system.rs(parent):429  声明差异行 0  规则命中 0
system.rs                    原 1013 行 = 覆盖 977 行 + 声明例外 36 行  OK
system 子模块可见性提升（pub(super) -> pub(crate)）与重导出声明一致  OK
session_events 父（发布/视图/计划/预测）  原文 1-233 -> session_events_test.rs:1  声明差异行 0  规则命中 0
session_events rewind        原文 234-618 -> session_rewind_events_test.rs:5    声明差异行 0  规则命中 0
session_events compact       原文 619-1021 -> session_compact_events_test.rs:5  声明差异行 0  规则命中 0
session_events_test.rs       原 1021 行 = 覆盖 1021 行 + 声明例外 0 行  OK
perf 父 模块文档              原文 1-32  -> session_history_perf_test.rs(头部):1  声明差异行 0  规则命中 0
perf 父（夹具/计划证据/统计）   原文 33-791 -> session_history_perf_test.rs:36  声明差异行 0  规则命中 0
perf cases                   原文 792-1143 -> session_history_perf_cases_test.rs:5  声明差异行 0  规则命中 0
perf audit                   原文 1144-1361 -> session_history_perf_audit_test.rs:5 声明差异行 0  规则命中 0
session_history_perf_test.rs 原 1361 行 = 覆盖 1361 行 + 声明例外 0 行  OK
full_remote 父（冷恢复/过滤契约） 原文 1-736 -> full_remote_test.rs(parent):1  声明差异行 0  规则命中 0
full_remote rewind           原文 737-1029 -> full_remote_rewind_test.rs:7  声明差异行 3  规则命中 3
full_remote_test.rs          原 1029 行 = 覆盖 1029 行 + 声明例外 0 行  OK

全部搬移块字节一致（差异仅为声明项）
system.rs 子模块挂载 + pub use 重导出声明齐全  OK
peri-resources 两个父模块的 #[path] 挂载与子模块头部齐全  OK
EXIT=0
```

### 3.2 覆盖性（原行去向）

1. `system.rs`：1013 = 977 行字节覆盖 + 36 行声明例外（6 行子模块间空行分隔 + 30 行头部改写）。
2. 另 3 个文件：原文行 **100% 覆盖**（1021 / 1361 / 1029），声明例外 0。
3. 三处声明差异（7 / 6 / 3 行）分别对应 §2.4 的可见性提升与路径限定，脚本按行号+规则命中双向核对。

---

## 4. 公开路径保持证据

1. 重导出（`system.rs`）：`pub(super) use self::bg_task::{handle_bg_task_cancelled, handle_bg_task_completed, handle_bg_task_snapshot, handle_bg_task_started, handle_bg_task_updated};`、`pub(crate) use self::bg_task::request_bg_task_snapshot;`、`pub(super) use self::interaction::{handle_ask_user, handle_hitl_pending, handle_interaction_terminal};`。
2. 测试引用路径按既有先例 `#[cfg(test)]` 门控（对齐 `acp_events/mod.rs:24-25`）：`#[cfg(test)] pub(crate) use self::bg_task::apply_bg_task_snapshot;`、`#[cfg(test)] pub(crate) use self::interaction::hitl_input_summary;`。
3. 运行时证明（`.recon/s13-size-tui-resources-raw/mount-proof.txt`）：既有调用链过滤运行均非空跑——`acp_events_test::bg_task_live_test::` **12 passed**（走 `system::handle_bg_task_*` / `request_bg_task_snapshot` 路径）；`acp_events_test::interaction_test::test_hitl_input_summary_extraction_matrix` **1 passed**（走 `system::hitl_input_summary` 门控重导出）。

---

## 5. 挂载真实成立（非空跑）证据

脚本：`bash .recon/s13-size-tui-resources-raw/mount-proof.sh` → `.recon/s13-size-tui-resources-raw/mount-proof.txt`（10 条全部 `EXIT=0`）。关键原始输出：

| 过滤目标 | 结果行 |
| --- | --- |
| `acp_events_test::session_events_test::`（父） | `16 passed; 0 failed; 0 ignored; 1849 filtered out` |
| `...::session_events_test::session_rewind_events_test::` | `5 passed; 0 failed; 0 ignored; 1860 filtered out` |
| `...::session_events_test::session_compact_events_test::` | `4 passed; 0 failed; 0 ignored; 1861 filtered out` |
| `acp_events_test::bg_task_live_test::` | `12 passed; 0 failed; 1853 filtered out` |
| `acp_events_test::interaction_test::test_hitl_input_summary_extraction_matrix` | `1 passed; 0 failed; 1864 filtered out` |
| `sessions::remote::schema_upgrade_tests::full_remote_tests::`（父） | `7 passed; 0 failed; 518 filtered out` |
| `...::full_remote_tests::full_remote_rewind_tests::` | `3 passed; 0 failed; 522 filtered out` |
| `history_perf_tests::`（父） | `1 passed; 0 failed; 5 ignored; 519 filtered out` |
| `history_perf_tests::cases::history_segment_statements_are_index_located` | `1 passed; 0 failed; 524 filtered out` |
| `history_perf_tests::audit::audit_segment_lookahead_and_plan_shape --ignored --nocapture` | `1 passed; 0 failed; 524 filtered out` |

合计核对：`session_events_test` 16 = rewind 5 + compact 4 + 父 7；`full_remote_tests` 7 = rewind 3 + 父 4——与拆分前父模块过滤结果（16 / 7）逐数吻合，无丢失、无重复。

---

## 6. 全量 lib 测试（前后数字一致）

命令：`./scripts/cargo-rmcp-patched.sh test --locked -p <crate> --lib`。前后原始输出见 `.recon/s13-size-tui-resources-raw/before-*.txt` / `after-*.txt`。

| 目标 | 拆分前（`.orig` 树） | 拆分后（当前冻结树） |
| --- | --- | --- |
| `-p peri-tui --lib` | `1859 passed; 0 failed; 6 ignored` `EXIT=0` | `1859 passed; 0 failed; 6 ignored` `EXIT=0` |
| `-p peri-resources --lib` | `499 passed; 0 failed; 26 ignored` `EXIT=0` | `499 passed; 0 failed; 26 ignored` `EXIT=0` |

---

## 7. 文件规模（STD-SIZE-001）

1. `wc -l`（11 个文件，本次重跑一致）：`502 / 264 / 294 / 239 / 388 / 407 / 800 / 355 / 222 / 740 / 299`，**全部 ≤1000**。
2. 全库扫描 `bash scripts/check-file-size.sh`：共扫 **2340** 个文件，本任务 4 个目标文件已从超限名单消失；仍余 **2 个存量超限文件**（非本任务范围、未改动）：`peri-tui/src/kit/panels/mcp.rs` 1637、`peri-resources/src/sessions/sqlite_store/schema_test.rs` 1038。**不宣称全库通过**。

---

## 8. 未验证项与局限

1. **未做变异/注入证伪**：以逐块字节比对 + 编译 + 全量数字一致替代；「挂载真实成立」用子模块过滤运行非空跑（5 / 4 / 3 / 1 / 1 passed）证明。
2. **可见范围等价性是静态推理**：`pub(super)→pub(crate)` 的实际可达集合由私有 `mod bg_task;` / `mod interaction;` 封顶，方向性证据为全量编译 0 warning；未写编译期反向探针。
3. **门禁覆盖范围**：clippy/check 仅按 pre-commit 命中 crate（peri-tui / peri-resources 等）运行，未覆盖其它 crate 的 test target 深度；其它 crate 的 lib 测试未跑（其工作树内有并行未提交改动，超出本任务范围）。
4. **既有 warning（非本次引入）**：`peri-acp/src/dispatch/session_replay.rs:55`（large enum variant）、`peri-tui/src/kit/acp_events/history_page.rs:41`（type_complexity）。
5. **文档索引未同步**（按「禁止改范围外文件」未动）：
   1. `docs/code-index/peri-tui.md:74,81,95` 仍只列 `src/kit/acp_events/system.rs`，未登记 `system/{interaction,bg_task}.rs`；
   2. `docs/code-index/peri-resources.md:82` 仍把 `session_history_perf_test.rs` 记为唯一 perf 测试文件，未登记 `_cases_test.rs` / `_audit_test.rs`；`full_remote_rewind_test.rs` 亦未登记。
6. **拆分脚本输入受限**：`.recon/s13-size-tui-resources-raw/split.py` 幂等，但只接受同目录 `.orig` 快照作为输入，对其它树复跑会在边界断言失败（不适用于已拆分树之外的输入）。

---

## 9. 改动文件 sha256

（本次重跑与 `.recon/s13-size-tui-resources-raw/after-sha256.txt` 一致。）

```
62fcec8291b4774afdb5db38da4ef7adf48203a28c54e3f27d155c471b118eee  peri-tui/src/kit/acp_events/system.rs
f9d3e28153579f569204c8ddfa5a705506c9688c23e8a816a9a58aadafab276f  peri-tui/src/kit/acp_events/system/interaction.rs
68e5e638650799612ac83e146c9670deed45a2d2f6419e39d11cb21d60c0b431  peri-tui/src/kit/acp_events/system/bg_task.rs
05372d350c7f29ad2e28c620cd39e00aaba108842bd2f3b713488a2d76cf7d19  peri-tui/src/kit/acp_events_test/session_events_test.rs
2173ec1d4dc53ce3a3c4ceb3948d84c963d51d6e2b4dcde13915855447e87504  peri-tui/src/kit/acp_events_test/session_rewind_events_test.rs
a04741f4b4654080924b82d9ad0aef07d1f8d2e9bf582936775a3bce39333b4f  peri-tui/src/kit/acp_events_test/session_compact_events_test.rs
eaff0bf8b41b15621b8588d9e2e47dabfef3cd799d474c411929307a54a0e0a7  peri-resources/src/sessions/sqlite_store/session_history_perf_test.rs
306e00046f97f31d8b8d45abf378b1c6ee9f8c471ba87b5420e42d278614f6cd  peri-resources/src/sessions/sqlite_store/session_history_perf_cases_test.rs
06e1a03ec62a839b8f76e1fec7b986cef1de870f70f70c8913d6583864e5866a  peri-resources/src/sessions/sqlite_store/session_history_perf_audit_test.rs
692b7da1b25b59d6c541a644c1ac6233f72354d3da6bde5b9f67131157992873  peri-resources/src/sessions/remote/full_remote_test.rs
50e486b531ebc77a783446ddbe4c6522e2fb69d4b4ceff185ea16c68aeea51cf  peri-resources/src/sessions/remote/full_remote_rewind_test.rs
```

VERDICT: SIZE-SPLIT-DONE
