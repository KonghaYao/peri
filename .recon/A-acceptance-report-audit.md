# A 验收报告的独立复核审计

- 审计对象：`.recon/A-acceptance-report.md`（2026-10-14 08:13，363 行）
- 审计者：独立复核者（只读；未改生产代码、未改报告本体、未做 git 写操作、未跑 DDL）
- 工作区：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`（分支 `feat/transcript-shrink-history-pagination`）
- 审计日期：2026-10-14
- 证据纪律：本文每条标注 **实测**（本次跑过的命令 + 输出片段）或 **静态**（file:line 只读核对）或 **引用**（只核对了源报告文本）；没跑的一律写「未验证」

---

## 0. 结论

1. **判定：需修订（修订后可作交付验收依据）。** 报告主体诚实、可核对：25 条验收项无合并/淡化，7 条 PASS 全部能追到命令或 file:line，FAIL/未验证判定偏保守（方向正确），两类成本与「计划 + 扫描量」双证据齐备，全文无未经基准支持的固定毫秒承诺。
2. **必须修订 4 项**（第 6 节）：R9 现状陈述已不成立、SDK 失败归因写错、`8/8` 表述过强、未验证清单缺 2 条（其中 1 条是审计报告点名的 `peri.sessionWorkspaceV1`）。
3. **建议修订 5 项**（第 7 节）：1 处无留档的毫秒数、1 处统计口径过窄、1 处关键字命中遗漏、1 处二手数字、4 处行号指针偏移。
4. **复跑验证**：报告 §0.3 声称的 5 条复跑，我逐条重跑——4 条逐字复现（14 passed / 69 passed / 3 passed / 25 pass，其中含 F1 探针「通过 = 缺陷在」），1 条（200 行性能档）**结构量逐位复现、毫秒数不复现**（见 §2.5）。

---

## 1. 复核方法（跑了什么）

### 1.1 实测（本次亲自执行的命令）

| # | 命令 | 本次结果 | 报告声称 |
| --- | --- | --- | --- |
| 1 | `cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_page_wire` | `14 passed; 0 failed; 871 filtered out`（含 `zz_audit_initial_cursor_skips_archive_when_the_oldest_effective_own_row_is_system ... ok`） | 14 passed / 0 failed；探针通过 = 缺陷在 |
| 2 | `cargo-rmcp-patched.sh test --locked -p peri-resources --test zz_audit_history_anchor_probe` | `3 passed; 0 failed`（三条探针名与报告 §5.9.3.1 描述一致） | 3 passed / 0 failed |
| 3 | `cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- rewind` | `69 passed; 0 failed; 816 filtered out` | 69 passed / 0 failed |
| 4 | `PERI_PERF_ROWS=200 PERI_PERF_ITERS=3 PERI_PERF_PAGES=3 … -- history_perf_restore_and_paging_evidence --ignored --nocapture` | 行数探针 `203`（excluded 200）、解码正文字节 `196971`、计划形状同 `perf-evidence.md` §3；**恢复 cold p50 1.545 ms / hot p50 1.065 ms** | 行数探针 203（excluded 200）、196 971 B、计划形状一致；**cold p50 1.426 ms / hot 0.945 ms** |
| 5 | SDK `bun test tests/history-merge.test.ts tests/history-pagination.test.ts` | `25 pass / 0 fail`，`Ran 25 tests across 2 files` | 25 pass / 0 fail |
| 6 | 只读核对：`git diff --stat`、DDL token 扫描、`Cargo.toml`/`package.json` 改动、`session/*` 方法名、`final-matrix-logs` 计数、`content_size` 全仓、`session_history_segment_test.rs:4`、`tool_projection.rs:33-37`、`session_replay.rs:635-646`、`session_resources.rs:714-717`、`store/history.rs:43`、`full.rs:86-101`、`transcript.rs:802/813`、`spawn.rs:348-352`、`handoff.rs:144-160`、`session_load.rs:14-24/101-152`、`REPLAY_SEGMENT_ROWS=50`、`peri.historyPagination` 默认 false、P0-77 登记 | 见 §3、§6、§7 各条 | — |

> 「871 filtered out」+ 14 = **885**，与报告 §0.3 计数说明「审计与本次复跑后为 885」**实测吻合**（`final-test-matrix.md` 的 884 是审计新增探针之前的数）。

### 1.2 未实测（本次没跑，因此不断言）

1. 三规模性能档（10k/100k/1M，约 36 分钟夹具）——未跑，故性能数字仍按报告声明视为「引用 `perf-evidence.md`」。
2. 五 crate 全量 `--lib`（报告 §8.1 的 4 863 passed）——未跑，只核对了 `final-test-matrix.md` 的算术自洽。
3. 云后端 21 条 `#[ignore]`、`e2e/`、TUI 渲染帧、人工用例 P0-77——未跑（与报告声明一致）。
4. 字节码级判读（R2/R3/R1 的 `Column p5`、`SeekGT/IdxGT`、rowid 复用反例）——未复跑，只核对了 `S9-boundedness.md` 原文。

---

## 2. 逐条抽查（PASS / PARTIAL / FAIL / 未验证）

### 2.1 七条 PASS 的证据可追溯性（无「无证据 PASS」）

| 项 | 报告结论 | 我核到的证据 | 判定 |
| --- | --- | --- | --- |
| A02 | PASS | `compact_cancel_test.rs:512/536/559/594`（**实测行号精确命中**测试函数与 `#[tokio::test]`；文件在 `peri-agent/src/session/exec/executor_helpers/`）、`transcript_test.rs:196`（命中 `test_commit_compaction_lifecycle_is_atomic_or_uncertain`）、`compact_failure_adversarial_test` 21 passed（**实测核对日志** `final-matrix-logs/it-peri-agent-compact_failure_adversarial_test.log:27`） | 证据成立 |
| A08 | PASS | wire 用例名存在；SDK 两条用例在本次复跑内 **实测通过**（25/0） | 证据成立 |
| A11 | PASS | wire 拒绝路径（`a_cursor_from_another_session_is_its_own_error` 等）；SDK 三条用例名逐条命中且 **实测通过**；S7c-tui D2（`S7c-tui-verification.md:19/67`，PASS 实测） | 证据成立 |
| A16 | PASS | `compact_progress_test.rs:192` **实测命中**（文件在 `peri-agent/src/agent/stages/`）；`compact_pressure_adversarial_test` 10 passed **实测核对日志**（同上 `:16`） | 证据成立（报告已自述「未逐条复跑，引用矩阵日志」） |
| A23 | PASS | 两条 SDK 用例名命中（`history-merge.test.ts:132/153`）且 **实测通过** | 证据成立（TUI 渲染层未跑已写明） |
| A24 | PASS | SDK 用例命中（`:254` 与 `:239`）；TUI REQ-C01-6 PASS（`S7-tui-verification.md:21`） | 证据成立 |
| A25 | PASS | `peri-acp/src/host/history_page_test.rs` **静态计数 = 10 个 `#[test]`** ✓；wire **实测 14 passed** ✓；`S9c-verification.md` 判据表 6 项全 PASS ✓；F2 口径与 `final-adversarial-audit.md` §2.1 一致 ✓ | 证据成立（另见 §7.2 关于 `session_load.rs:14-24` 指针） |

### 2.2 关键 PARTIAL / FAIL / 未验证 的抽查

1. **A05（FAIL）**：报告把审计的「未验证」**加严为 FAIL**。依据链可核：`perf-evidence.md` §4 表（11.109/98.182/1081.927）与 §8 表逐字一致；报告 §3.1.3 已写明「1M 档未复跑、只引用历史证据 + 200 行档确认计划形状」。**判定：加严合理且披露充分**（不是把静态判断写成实测）。
2. **A15（FAIL）**：F1 两层探针 **本次实测均通过（= 缺陷在）**；根因静态核对成立：`session_load.rs:115-120` 的 `anchor = effective.payloads.first()` 确无可展示性检查；`full.rs` 的 System 豁免（实测命中约 `:90` 的 `.filter(... System ...)`）、`transcript.rs:813` 的 System 保留（**实测行号精确命中**）均成立。
3. **A03（PARTIAL）**：报告「用户输入（Human）形态无专门用例」与 `final-adversarial-audit.md` §3.5 缺口 1 一致；用例 `test_full_report_arriving_during_summary_stays_available` 存在（`peri-agent/src/agent/compact_v2/full_report_test.rs`）。✓
4. **A04（PARTIAL）**：结论与 `perf-evidence.md` §8「扫描量只算部分满足」一致；报告未把「计划形状通过」写成「触达行数有界」，反而在 §4.3.4 显式区分。✓
5. **A06/A07（PARTIAL）**：数字与 `perf-evidence.md` §4.1/§5.2/§5.3 逐项一致（`items=50`、`bytes=57700`、首屏 `251209`、`position 999954→999004`、`oversize=true`、`position 6→5→4`）；A07 的夹具规模（6 条 vs 100 条）缺口如实写出。✓
6. **A09/A10/A12/A13/A17/A18/A19/A20/A21/A22（PARTIAL）**：每条都写明「哪一半成立、哪一半没有」；A12 的 SDK 摘要过强（见 §6.3），A10 的 `doc-model.ts` 全局合并风险与 `S7c-sdk-verification.md:166` 逐字一致。✓
7. **A14（未验证）**：判定与审计一致（✓）；但数字「fork 装配测试 11 passed」不可核（见 §7.4）。
8. **未发现「把静态判断写成实测」的项**：全文凡静态处均显式标注（如 §5.8.3「仅静态判定」、A21「仅调用图证明」、§5.9.4「静态推断」）。

---

## 3. 数字一致性对账

### 3.1 与 `perf-evidence.md`

| 报告引用 | 源文件 | 结果 |
| --- | --- | --- |
| 11.109 / 98.182 / 1 081.927 ms；hot 11.032 / 103.699 / 1 084.641 | §4 表 | 一致 |
| 行数探针 10 003 / 100 003 / 1 000 003 | §4、§8 | 一致 |
| 归档正文 11 518 890 / 115 288 890 / 1 153 888 890 B | §4 表 | 一致 |
| 解码正文恒 196 971 B、排除正文解码 0 条 | §4 | 一致 |
| 恢复语句数 7、返回 payload 3（flags 1） | §4.1 输出 | 一致 |
| 计划断言 4 条（无 `SUM(`、无 `SCAN messages`、必现索引、≤0.2 s） | §3 | 一致（4 条、三段全过；0.179/0.153/0.166 s） |
| 1M 20 页 `items=50`、`bytes=57700`、单页 2.0–3.6 ms、`position 999954→999004` | §4.1 | 一致（报告 §4.2 额外补出首屏 251 209，§2 A06 行未提首屏，属简写） |
| 6×1 250 000 B oversize 夹具、`position 6→5→4` | §5.2 | 一致 |
| 200×20 480 B 字节预算夹具（`items=18`、`506076`） | §5.3 | 一致 |
| 探针：对照组失败 / 处理组成功、196 971 B | §5.1 | 一致 |
| 环境（macOS aarch64 / M5 Pro / rustc 1.99.0 / SQLite 3.46.0 / page_size 4096 / 0.52 与 77 MB/s） | §1、§2 | 一致 |
| 未测量 4 项（页级读盘、峰值 RSS、release、远端） | §7（实为 6 项，另 2 项在 §1.7/§7.6 由报告 §1.2、§3.2 覆盖） | 一致（覆盖完整） |
| 20 页 ≈ 1.15 MB 量级 | §8 末尾 | 一致 |

### 3.2 与 `final-test-matrix.md`

| 报告引用 | 源文件 | 结果 |
| --- | --- | --- |
| 576 / 884 / 1089 / 497（26 ignored）/ 1817（6 ignored） = 4 863 passed | §0、§1.2 | 一致；**算术自核 4 863 ✓** |
| 19 个集成目标、117 passed | §1.4 | 一致；**19 项逐条相加 = 117 ✓** |
| 145 files / +9 203 / −853 | §5.2 | 一致；**本次 `git diff --stat` 实测复核 ✓** |
| 基线差 +8 / +21 / 0 / +13~16 / +10 | §3.1 | 一致 |
| SDK 252/2（默认）与 253/1（解除阻塞） | §1.3 | 数字一致，**归因不一致 → 见 §6.2** |
| 34 个原始日志（§8.3） | §5.1 写 33、抬头写 34 | **实测 `ls` = 34**，报告取到的是正确值 |
| 本仓库代码 0 warning、5 crate check exit 0 | §0、§1.1 | 一致（第三方 `hyper-util` 1 条 warning 已注明） |

### 3.3 与 `final-adversarial-audit.md`

| 报告引用 | 源文件 | 结果 |
| --- | --- | --- |
| A01~A25 状态表（部分实测/引用/未验证） | §4 | 一致；**唯一状态变化**：A05 由「未验证」→「FAIL」（加严，已在 A05 行与 §5.2 说明理由） |
| F1（P1）根因、影响面、未修 | §1.1~§1.6 | 一致（含 `spawn.rs:348-352`、`full.rs:86-101`、`transcript.rs:813`、`session_load.rs:115-120`） |
| F2（P2）口径（字面不违反 / 加严不成立） | §2.1 | 一致；根因指针 `tool_projection.rs:33-37`、`session_replay.rs:635-646` **实测命中且语义相符** |
| 「本机 6+1+1 全绿」 | §0 行 4（6 + full_remote 1 + 受众规则 1） | 一致 |
| 884 → 885 的计数说明 | §4.4 第 17 条 | 一致（本次实测 871 filtered + 14 = 885） |
| 未验证清单 17 条 | §4.4 | **报告漏 2 条 → 见 §6.4** |

---

## 4. 遗漏检查

### 4.1 被点名项是否被合并 / 淡化 / 跳过

1. **无合并、无跳过**：A02 / A03 / A08 / A09 / A10 / A13 / A14 / A16 / A18 / A22 / A23 / A24 在 §2 逐条独立成行，各带「缺口（一句话）」列。
2. **未被淡化**的三处（值得肯定）：A05 从「未验证」加严为 FAIL；A15 因 F1 判 FAIL；A20/A06 明确写出「在 System 锚点会话上不成立」。
3. **计数自洽**：PASS 7 + PARTIAL 15 + FAIL 2 + 未验证 1 = 25，与逐条表逐项一致（我按行重数一遍 ✓）。

### 4.2 未验证清单

1. **存在**（§5.13 第 10 条 + §3.1.3 + §8.1「本次未复跑」），覆盖面：性能 5 条 `#[ignore]`、云 21 条、人工 TUI 用例、`e2e/`、真实外部 stdio、真实云端、HEAD 同源对照、峰值 RSS。
2. **漏 2 条**（均出自 `final-adversarial-audit.md` §4.4）：
   1. **第 11 条 `peri.sessionWorkspaceV1` 的门控来源未定位**——全文 grep 报告无此键（**实测**：`grep -n "sessionWorkspaceV1" A-acceptance-report.md` 无命中）；这是协商连接出现、未协商未出现、但未做发射点穷举的一个键，属「未验证」。
   2. **第 14 条**「未协商 + cursor 被拒时不产生回放通知」只有 `history.len()` 不变这一间接证明，未做通知全量断言——报告 A25 行只引用了「拒绝不得触发一次 initial load」的正面断言，未带该强度边界。

### 4.3 §10（不在本任务内）的显式声明

**存在且完整**：§7 逐条表 7 行（旧格式兼容/迁移/双写、未压缩超长的兜底、摘要算法/预算/Micro、物理删除/TTL/GC、Checkpoint/event-sourcing、Yjs 架构替换、resume+独立历史 RPC），每行有依据；另补两条需求边界。抽查「无 DDL」依据可核：本次 `git diff -U0` 的 `CREATE TABLE`/`ALTER TABLE`/`CREATE INDEX`/`DROP TABLE` **实测计数均为 0** ✓。

---

## 5. 口径检查

1. **两类成本确实分开**：§3.1（初始恢复）给三规模实测表 + 「成立的一半 / 未达成的一半 / 未覆盖」；§3.2（累计缓存）明确「零测量」并列出三条缺口，§3.2.4 声明「不宣称累计内存恒定」。**实测核对**：该口径与 `S7c-sdk-verification.md:167`（第 7 条）逐字一致。
2. **有界读取同时给了计划与扫描量**：§4.1 用生产语句原文的 `EXPLAIN QUERY PLAN`（7 + 4 条）；§4.2 用行数探针/触达上界表（恢复 O(线程行数)、分页 ≤ limit+1、索引项随深度线性 R3、回放段 ≈ limit+1、阶段二只取本页）。需求 §9 末段的「不能只看返回行数」被正面回应。✓
3. **无未经基准支持的固定毫秒承诺**：全文 grep「ms 内 / 必须低于 / 承诺」只命中 §1.4、§9.3 的自我声明，以及 §3.2 引用的「SDK 无相反承诺」。所有毫秒数都带 `perf-evidence.md` §1 的 debug / 冷热口径。**唯一需注意**：§4.1 第 2 条④「计划采集 ≤ 0.2 s」是复述性能用例内部的断言阈值（实测 0.179/0.153/0.166 s），不是产品承诺；建议加「（用例内部断言）」以绝歧义。

---

## 6. 必须修订项（4 项）

### 6.1 M1（证据与结论不符）：§5.13 第 6 条 R9 已不成立

1. **报告原文**：「`session_history_segment_test.rs:4` 指向不存在的远端文件（实际在 `remote/session_history_page_test.rs`）」。
2. **实测反证**：`sed -n '1,6p' peri-resources/src/sessions/sqlite_store/session_history_segment_test.rs` → 第 4 行为「……同一份语义在远端 adapter 上由 `remote/session_history_page_test.rs` 断言」；`remote/session_history_page_test.rs` **存在**（`ls` ✓），`remote/session_history_segment_test.rs` 不存在。全文 grep 该文件只有这一处 `remote/` 引用。
3. **结论**：R9（出自 `S9-boundedness.md` §6 第 9 条，2026-10-14 01:16）在当前工作区**已被修正或本就非该行**；报告把它当现状写出，属**未复验的过期偏差**。
4. **修订建议**：删除该条，或改写为「R9 在 S9-boundedness 记录时的引用错误，当前源码第 4 行已指向存在的 `remote/session_history_page_test.rs`（本次核对）」。

### 6.2 M2（数字归因不一致）：§8.1 把 SDK 的两类失败混成一条

1. **报告原文**：「`bun test` **252 pass / 2 fail**（解除环境后 253 / 1；失败为基线 WASM telemetry panic，见 §5.13 第 8 条）」。
2. **源文件**：`final-test-matrix.md` §2.1/§2.2 明确分两层——默认环境 2 条失败均为 `error: sqld exited (1)`（`mise` 未信任 worktree 配置的**环境**问题）；解除阻塞后剩 1 条为 WASM telemetry panic（`langfuse-client/src/batcher/recovery.rs:256`，基线 `7f0de56e8`）。
3. **修订建议**：改为「252/2（2 条为 sqld 环境失败）；解除环境阻塞后 253/1（1 条为基线 WASM telemetry panic）」。

### 6.3 M3（表述过强）：§2 A12 行「SDK 8/8」

1. **源文件**：`S7c-sdk-verification.md` §0 判据表为 **判据 1~6、8 = PASS；判据 7 = 「typecheck、build PASS；bun test 252 pass / 2 fail（环境，非切片）」**。
2. **修订建议**：改为「判据 1–6、8 PASS；判据 7 含 1–2 条环境红牌（见 §6.2）」。同时 §5.11.1 的 A12 口径说明可保留。

### 6.4 M4（未验证清单缺口）：补 2 条

1. `peri.sessionWorkspaceV1` 键的门控来源未定位（`final-adversarial-audit.md` §3.3 补充事实 + §4.4 第 11 条）。
2. 「拒绝不得触发一次 initial load」只有 `history.len()` 不变的间接证明，缺通知全量断言（同文 §3.2 缺口 + §4.4 第 14 条）。

---

## 7. 建议修订项（5 项，不影响 PASS/FAIL 判定）

1. **S1（无留档的毫秒数）**：§0.3 第 4 行的 1.426 / 0.945 ms 在 `.recon/` 无日志；**实测同参数重跑**得 cold p50 1.545 ms / hot 1.065 ms（结构量 203 行探针、196 971 B、计划形状逐位复现）。建议标注「一次性运行、无留档；量级可复现，末位不可复现」。
2. **S2（统计口径过窄）**：§5.13 第 1 条「只有定义与**一个** TUI 测试夹具」——实为 2 处（`peri-tui/src/cli_meta_test.rs:31`、`peri-tui/tests/meta_session_cli.rs:119`），另有 `sqlite_store/filesystem.rs:285-290` 的写入点与 `filesystem_test.rs:264-275`（断言 `> 0`）。「无生产消费者」成立，夹具计数建议改为「2 处测试夹具 + 遗留 filesystem 后端写入」。
3. **S3（关键字命中遗漏）**：§6.3 第 2 条「无迁移/TTL/GC/双写关键字命中」——**实测**：新增行中 `双写` 命中 1 处（`docs/code-index/peri-agent.md`，否定语境「不走 writer 双写」）、`user_version` 命中 1 处（`docs/code-index/peri-resources.md`，说明「服务端拒绝写 `PRAGMA user_version`」）；`TTL`/`CREATE …` 等 0 命中。建议限定为「生产代码与 schema 无 DDL/迁移实现；文档中的否定性表述不计入」。
4. **S4（二手数字）**：A14 行「fork 装配测试 11 passed」的唯一出处是 `final-adversarial-audit.md` §4 A14 的断言，`.recon/` 无对应日志；静态计数与之不符（`child_wire_capture_test.rs` 7 个测试属性；全仓名字含 `fork` 的测试约 10 条）。建议写成「引用审计断言，未复核原始数字」。
5. **S5（行号指针偏移，4 处）**：
   1. A07「两阶段 `history_page.rs:403-552`」——`peri-acp-types/src/history_page.rs:403-552` 是游标/边界/候选代码；两阶段实为 `HistoryPagePlan::plan`（:597）与 `materialize`（:643）。
   2. A06「`session_history_page_test.rs:148`」——该用例 `non_displayable_rows_advance_the_position_without_faking_the_end` 在 **:152**（内容描述正确）。
   3. A19「`session_history_test.rs:778/810/833`」——三条用例实际在 **:780/812/835**（`#[tokio::test]` 在 779/811/834）。
   4. A15「wire `:231-281`」——断言 `fresh["nextCursor"] == Null` 在 **:269-271**，而 `:231-264` 属另一用例（`pagination_fields_from_an_unnegotiated_connection_are_rejected`）；该指针系从 `S6-verification.md` 不变量 5 转引，建议改为「`:265-323` 的 `initial_load_cursor_walks_…`」。

---

## 8. 审计者未验证的部分（本文的诚实边界）

1. 未跑三规模性能档，故「11.109/98.182/1 081.927 ms 与其冷热口径」只做了**文本一致性**核对，未做复现。
2. 未跑五 crate 全量 `--lib`；4 863 只做了算术与来源核对。
3. 未复核 `S9-boundedness.md` 的字节码判读（R1/R2/R3）与 R5~/R6 结论，只核对了报告转述与源文一致。
4. 未跑云后端、`e2e/`、TUI 渲染帧与人工用例；报告在这一点上的自我声明（未跑）与审计一致。
5. 未修改任何生产代码、测试或报告本体；本文所有结论以第 1 节列出的命令与只读核对为限。

---

## 9. 一句话摘要

报告**可以作为验收依据，但需先修 4 处**：R9 过期陈述、SDK 失败归因、A12「8/8」、未验证清单补 2 条；其 7 条 PASS 与 3 类非 PASS 判定经本次独立复跑与只读核对**基本成立**，两类成本与「计划 + 扫描量」口径合格，无伪造实测、无固定毫秒承诺。
