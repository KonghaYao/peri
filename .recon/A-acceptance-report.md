# A：验收报告（需求 §9 A01~A25）

- 日期：2026-10-14（CST）；**第三版**（第一版经 `.recon/A-acceptance-report-audit.md` 复核为「需修订后可作验收依据」；第二版按 M1~M4 修订并加入 F1 修复轮；第三版加入 S12 收尾轮与 S13 修订轮：拒绝分支持久化、A14 最小直接用例、文件规模治理、锁回归重做、WASM 基线修复，修订记录见 §0.1）
- worktree：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`；分支 `feat/transcript-shrink-history-pagination`（**核心交付已提交**：`95c1e07d7` 存储/ACP/Agent、`9b96f456a` 前端、`3ccf75864` 文档与证据；S12~S13 收尾轮改动与本报告由第四次提交一并提交、第四版（A05 索引与两项对抗裁决）由第六次提交 `5e691a2ac` 落地，见 §0.1b）
- 需求：`/Users/konghayao/Desktop/peri-transcript-history-requirements.md`（§9 验收表 A01~A25、§5.3 ACP 标准边界、§10 不在本任务内）
- 蓝图与决策：`spec/issues/2026-10-13-transcript-shrink-history-pagination.md` §7（D1~D15、切片状态 S1~S11、编排脚本）
- 证据目录：`.recon/`（R4/S6/R4b 复核、perf-evidence、S9*/S7*/S11 报告、final-test-matrix、final-adversarial-audit、A-acceptance-report-audit）
- 作者声明：本报告是**汇总与核对**，不是新一轮实现的验证者。所有数字照抄 `.recon/` 实测记录；作者只额外复跑了 §0.3 里标注「本次复跑」的命令。未跑的项目一律写「未验证」，计划与推断不写成结果。

---

## 0. 一页结论

### 0.1 逐条计数

| 结论 | 条数 | 编号 |
| --- | --- | --- |
| PASS | 9 | A02、**A05**、A08、A11、A16、**A19**、A23、A24、A25 |
| PARTIAL | 16 | A01、A03、A04、A06、A07、A09、A10、A12、A13、**A14**、A15、A17、A18、A20、A21、A22 |
| FAIL | 0 | —（A05 于第四版由 FAIL 改判达成，见 §5.18.3） |
| 未验证 | 0 | —（A14 在 S12/S13 补了最小直接用例，降为 PARTIAL，见 §2 与 §5.14） |

> 修订记录（2026-10-14，第二版）：本轮修复了审计新发现的 **F1（P1）** 并独立复核（§5.9），A15 由 **FAIL 升为 PARTIAL**（F1 反例消失；剩余缺口是「不触发迁移/自动摘要」缺负向用例）。同时按 `A-acceptance-report-audit.md` 的 4 项必须修订（M1~M4）与 5 项建议修订（S1~S5）改写了本报告。
>
> 修订记录（2026-10-14，第三版）：S12 收尾轮（`S12-*.md` 7 份 + 独立复核 `S12-verification.md`，判定 NEEDS-REVISION）与 S13 修订轮（`S13-*.md` 9 份 + 两份独立复核）后的变化——① 拒绝路径由临时探针转为 **11/11 类持久用例**（§5.14）；② **A14 由「未验证」降为 PARTIAL**（补了 fork 后父压缩不改子快照的最小直接用例，覆盖范围见 §2）；③ **文件规模治理**：本交付触及且超 1000 行的 10 个文件已按职责拆分（§5.15）；④ 锁回归测试重做为确定性握手并统一释放探测入口（能力边界见 §5.16）；⑤ WASM telemetry panic 修复保留，但运行时红→绿证据不完整（§5.17）。**A05 判定不变（FAIL），且 S13 独立实验进一步证明「固定 rowid 下界」会在 rewind 后丢行，必须 DDL 才能收敛。**
>
> 修订记录（2026-10-14，第四版）：用户指令（提交代码、A05 新增索引、第 2/3 项派 subagent 对抗裁决、推送）后的变化——① **A05 索引经用户显式授权并实施**（`canonical::CREATE_INDEXES` 追加 `idx_messages_active (thread_id, excluded)` + 本机 Current 分支幂等索引重放，§5.18.3），复跑三段规模后 **A05 由 FAIL 改判达成**（cold p50 3.299 / 3.379 / 2.435 ms，跨规模持平）；② **`peri.sessionWorkspaceV1` 门控：裁决「实现零改动 + 文档订正」**（§5.18.1；两个立场独立推翻了侦察阶段的「TUI 不 initialize」前提）；③ **锁守卫面：裁决「独立断言下界、拒绝结构性闭合、保留调用点级局限」**（§5.18.2，含本轮注入实验）；④ 计数：PASS 8 / PARTIAL 17 / FAIL 0。

> 修订记录（2026-10-14，第五版）：用户指令（授权其余 DDL 用于闭合 D8、完成工作后新建 PR）后的变化——① **D8 闭合，且不需要任何 DDL**：取代关系作为字段落进**既有**持久化载荷列 `messages.projection`（`MessageProjectionDirective::superseded_by`），Full Compact 归档时逐条写下「取代我的摘要 id」，rewind 据此逐条精确判定；无记录的历史行回退到原前缀结构判定。**没有新增列、没有推进 schema 代数**（本机第 12 代 / 远端 v6 不动），因此「授权其余 DDL」一项在本轮**未被使用**（详见 §5.1）。② **A19 由 PARTIAL 改判 PASS**（D8 字面达成；残留项为证据引用与旧行回退，见 §2 该行）。③ 计数：**PASS 9 / PARTIAL 16 / FAIL 0**。

### 0.1b 交付状态与本轮未提交内容

1. 三个 commit 已落在工作树（`95c1e07d7` / `9b96f456a` / `3ccf75864`），分支已**追平并 rebase 到 `origin/main`（`19215d3a1`）**；追平过程中修掉一处 rebase 语义冲突（上游给 `AcpEventData::LocalUserBubble` 加了 `attachments` 字段，本分支的匹配式补 `..`）。
2. TUI 集成用例编号因上游占用 `P0-77~P0-80` 改号为 **`P0-81`**（README/06 文档/code-index/蓝图已同步）。
3. S12~S13 改动（16 个已跟踪修改文件 + 26 个新增源码/测试文件（含 §5.15 拆分产物）+ 蓝图 spec 修订、`.gitignore` 两行（SQLite 运行期副本不入库）与 `.gitattributes` 一行（`.recon/**` 留档豁免空白门禁）+ 17 份 `.recon/S12-*.md`/`S13-*.md` 报告与 9 个原始留档目录 + 2 个编排脚本）与本报告的第三版**由第四次提交 `c1744d472` 一并提交**（含 `langfuse-client` WASM 修复与 SDK 夹具修复）；本报告随后的两处补记（§5.15 第 6 条、§6.4 第 6 条）由第五次提交落地。
4. **第四版（本轮）**：A05 索引实施、两项对抗裁决、协议/索引文档订正与本报告第四版由**第六次提交 `5e691a2ac`** 落地（12 files / +552 / −32；含新增证据 `.recon/a05-raw/perf-rerun.out`）；随后本节的哈希补记由第七次提交落地，两次提交一并推送到 `origin`（分支原先无 upstream）。
5. **第五版（本轮）**：D8 闭合（取代关系落在既有 `projection` 载荷、无 DDL）+ 相关 doc comment 与三份 code-index 订正 + 本报告第五版，由**第八次提交**落地；本节的哈希补记随后由第九次提交落地，一并推送并**新建 PR**（用户指令：完成之后新建 PR）。本轮改动 26 个文件（+728 / −56 为提交前统计口径）。

### 0.2 必须先看的三个结论

1. **F1（P1，已修复并独立复核）**：初始游标锚点曾取「有效视图最旧一条 own payload」，当这条是不被 Full 排除、又从不进入客户端渲染的 **System 身份条目**（subagent / fork / bg 子会话的真实形状）时，`nextCursor = null` 或锚点之后的归档永不交付 ⇒ **该会话的压缩原文在界面上不可达**。现锚点改为「最旧**可展示**条目」（判据与分页出口同源），并补了两条真实回归用例（§5.9）。受影响的验收：**A15、A20、A06** 的该形态已解除。
2. **D10（已闭合，第四版）**：授权前恢复路径的**内存与解码字节有界**成立，但**触达行数 = O(线程行数)**，实测耗时随归档线性（10k/100k/1M = 11.109 / 98.182 / 1081.927 ms，debug 构建）⇒ A05 当时**未达成**。2026-10-14 用户显式授权新增 `(thread_id, excluded)` 索引（`idx_messages_active`），复跑后 cold p50 **3.299 / 3.379 / 2.435 ms 跨规模持平** ⇒ **A05 达成**（§5.18.3）。保留的支撑证据：S13 独立实验（只读，VM 指令计数，20 万行合成库）10k/100k/200k → `SQLITE_VM_OPS = 50 036 / 500 036 / 1 000 036`、返回行数恒为 3 ⇒ 线性属实；同实验用「固定 rowid 后缀下界」模拟 rewind 后的收窄出现 `lost_rowids=[1,2]` ⇒ **无 DDL 的候选收窄会丢行，不可采用**（`.recon/S13-verification-behavior.md` §5.2）——这正是本条只能靠索引闭合的理由。
3. **两类成本必须分开读**（§3）：初始恢复成本有实测数字（三规模）；**持续浏览后的累计缓存没有任何测量**，「分页已保证客户端累计内存恒定」这句话本任务不支持，任何地方都不得写。

### 0.3 作者本次复跑（新增的一手证据，其余全部引用 `.recon/`）

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `test -p peri-acp --lib -- history_page` | **27 passed / 0 failed**（含 F1 修复后的三条回归用例：`the_initial_cursor_anchors_past_non_displayable_own_rows`、`an_all_non_displayable_own_region_reports_no_earlier_page`、`the_fallback_anchor_keeps_an_archived_row_reachable`） |
| 2 | `test -p peri-acp --lib -- rewind` | 69 passed / 0 failed（A19 面，**第一版**证据） |
| 3 | **F1 证伪**：把锚点实现临时改回 `payloads.first()` | 两条回归用例 **FAILED**（「存储里有压缩原文，不得谎称没有更早历史」/「存储里还有一条更老的可展示行，不得谎称没有更早历史」）；恢复实现后转绿。独立复核者另用 7 个探针复现，旧实现下 **5 个红**、修复后 7 个全绿（`.recon/S11-verification.md` §9） |
| 4 | `test -p peri-acp --lib`（全量） | **887 passed / 0 failed**（884 + 审计探针 1 − 探针 + 回归用例 3） |
| 4b | `test -p peri-tui --lib`（全量，交叉回归） | **1817 passed / 0 failed / 6 ignored**（与终局矩阵一致） |
| 4c | `fmt --all -- --check` | **0 diff**（本轮补跑：修复前交付集里有 42 个文件不符合 rustfmt，全部落在本次改动文件内；已 `cargo fmt --all` 归零，见 §6.4） |
| 4d | `check --locked --all-targets`（5 crate） | **exit 0**（`peri-acp-types`/`peri-acp`/`peri-agent`/`peri-resources`/`peri-tui`，本轮补跑） |
| 5 | `test -p peri-resources --lib`（全量） | **497 passed / 0 failed / 26 ignored**（首轮曾出现 1 条 `incompatible_schema_is_rejected_without_wal_or_database_changes` 偶发失败，单跑与次轮全跑均通过——与 §5.13 第 5 条同类的测试基建 flake） |
| 6 | `PERI_PERF_ROWS=200 ... -- history_perf_restore_and_paging_evidence --ignored` | 恢复冷 p50 1.426 ms / 热 0.945 ms；解码正文 196 971 B；行数探针 203（excluded 200）；计划形状与 `perf-evidence.md` §3 一致（**第一版证据：一次性运行、`.recon/` 无该次日志**；审计同参数重跑得 1.545 / 1.065 ms，量级可复现、末位不可复现） |
| 7 | `bun test tests/history-merge.test.ts tests/history-pagination.test.ts`（SDK） | 25 pass / 0 fail（A10/A11/A15/A20/A22/A23/A24 的 SDK 侧用例） |
| 8 | **S13 复核者独立复跑**（`S13-verification-size.md` / `S13-verification-behavior.md`） | `peri-acp --lib` **893 passed / 0 failed**；`peri-resources --lib` **499 passed / 0 failed / 26 ignored**；五个 crate 合计 **4916 passed / 0 failed / 32 ignored**；`peri-tui --lib` **1859 passed / 0 failed / 6 ignored**（rebase 后基线，上游新增 42 条）；`langfuse-client --lib` **292 passed / 0 failed / 1 ignored** |
| 9 | **S13 规模治理** | 本交付触及的 183 个非 `.recon` 源码/测试文件**逐个 ≤1000 行**（最大 990）；仓库全量扫描（`scripts/check-file-size.sh`）仍列 4 个存量超限文件（`peri-tui/src/kit/panels/mcp.rs` 1637、`peri-resources/src/sessions/sqlite_store/schema_test.rs` 1038 等），均**未在本交付改动集内** |
| 10 | **S13 WASM 侧**（复核者独立执行） | 当前 WASM 产物上，SDK 目标集成文件 **2 pass / 0 fail**；但实施轮的三次构建与「旧实现红灯」日志已被实施者删除 ⇒ **红→绿链不可独立复核**，记 PARTIAL（§5.17） |
| 11 | `git diff --check` | **exit 0**（S13 复核曾报 `requests_unnegotiated_replay_test.rs:517` 文件尾空行，已修） |
| 12 | **第五版（D8 闭合）本轮实跑** | `peri-acp-types --lib` **580 passed / 0 failed**（含 4 条新用例）、`peri-agent --lib` **1089 passed / 0 failed**、`peri-acp --lib` **893 passed / 0 failed**、`peri-resources --lib` **504 passed / 0 failed / 26 ignored**；**反向对照**：把两处清零 SQL 退回旧语义 ⇒ 4 条新断言全红；pre-commit 四步结果见 §6.4 第 8 条 |

> 计数说明：第一版报告时 `peri-acp --lib` 为 884（`final-test-matrix.md`）+ 1 条审计探针 = 885；**本轮修复删掉该探针、新增 3 条回归用例后为 887**。第一版的存储层探针 `peri-resources/tests/zz_audit_history_anchor_probe.rs`（3 条，未跟踪、非交付物）已删除：它复刻的是 ACP 侧的锚点判据，修复后会变成过期副本；同等覆盖改由 `peri-acp` 侧调用真实 API 的回归用例承担。

---

## 1. 口径与证据来源

1. **数字来源**：一律来自 `.recon/`——`perf-evidence.md`（§1 口径、§3 计划、§4/§4.1 三规模、§5 三条补充用例、§8 触达行数）、`S9-boundedness.md`、`S9-verification.md`、`S9c-verification.md`、`S6-verification.md`、`R4-verification.md`、`R4b-verification.md`、`S7*.md`、`final-test-matrix.md`、`final-adversarial-audit.md`。**没有自造数字**。
2. **测量口径（照抄 `perf-evidence.md` §1）**：
   1. profile = `test`（unoptimized + debuginfo）：**下面的毫秒数是 debug 构建的数，不是 release 数**；
   2. 冷 = 每次新开部署句柄（新连接池 ⇒ SQLite 页缓存为空），**OS 页缓存仍热**；热 = 同一句柄连续读取；
   3. 环境：macOS aarch64 / Apple M5 Pro / rustc 1.99.0 / sqlx 捆绑 SQLite 3.46.0（`page_size = 4096`）；本机存储实测 4 KiB 块 + fsync **0.52 MB/s**、1 MiB 块 77 MB/s；
   4. 未测量：页级读盘字节、峰值内存 RSS、release 构建、真实远端后端（`perf-evidence.md` §7）。
3. **证据分级**：本报告在每条的「证据指针」里区分 **实测**（命令/日志/用例）与 **静态**（代码路径、调用图）。「引用」= 有既有报告或既有测试，但**本次未复跑**。
4. **不新增毫秒承诺**：本报告只复述实测数字与其口径，不给出任何「必须低于 X ms」的承诺。

---

## 2. A01~A25 逐条验收表

| 编号 | 结论 | 证据指针 | 缺口（一句话） |
| --- | --- | --- | --- |
| **A01** | PARTIAL | `peri-agent/src/session/transcript_test.rs:262` `test_full_commit_releases_excluded_own_entries`（实测，含 `entries/id_index/flags` 容量与释放断言）；`transcript.rs:802` `shrink_after_full_commit`；`final-adversarial-audit.md` §4 A01 | 单次 Full 的释放与容器重建成立；**100 次连续 Full 的累计增长、跨 turn 引用计数、收缩前后模型请求等价**均无实测 |
| **A02** | PASS | `compact_cancel_test.rs:512/536/559/594`（含 `…after_sql_commit_requires_reload_and_keeps_summary` = 提交后确认丢失）；`transcript_test.rs:196` `test_commit_compaction_lifecycle_is_atomic_or_uncertain`；`compact_failure_adversarial_test` 21 passed（矩阵 it-log） | 无阻塞缺口；未做「确认丢失」的端到端 UI 面 |
| **A03** | PARTIAL | `test_full_report_arriving_during_summary_stays_available`（实测，报告形态）；`full.rs` 边界采集 | **用户输入（Human）形态无专门用例**；「不重复注入」的计数没有断言 |
| **A04** | PARTIAL | `perf-evidence.md` §3（计划断言 4 条，三段全过）、§4（排除正文解码 **0 条**、解码字节恒 196 971 B）、§5.1（不可解码归档探针：对照组 `load_session_snapshot` 失败、处理组 `load_active_snapshot` 成功）；本次复跑 #4 | 正文零解码与聚合消除成立；**扫描量只算部分满足**——三条语句在该线程索引范围内逐行回表，触达上界 = 线程总行数（D10） |
| **A05** | PASS（第四版改判） | 授权前证据：`perf-evidence.md` §4/§4.1/§8 cold p50 **11.109 / 98.182 / 1081.927 ms**、hot 同量级，语句数 7、返回 3 条、196 971 B、行数探针 10 003 / 100 003 / 1 000 003；S13 独立实验（只读、VM 指令计数）10k/100k/200k → `SQLITE_VM_OPS` 50 036 / 500 036 / 1 000 036。**索引实施后复跑**（`.recon/a05-raw/perf-rerun.out`、`perf-evidence.md` §4.2/§8.1）：cold p50 **3.299 / 3.379 / 2.435 ms**、hot p50 2.322 / 2.092 / 2.105 ms，三段同量级；计划三条有效范围语句改为 `USING INDEX idx_messages_active (thread_id=? AND excluded=?)` | **达成**：跨规模耗时持平（对照授权前 11.109 → 1081.927 的线性曲线）。代价：索引 ≈25 B/行（1M 档 +25.3 MB ≈1.7%）；升级首次补建耗时未测量；**峰值内存仍未采样**（§7.2） |
| **A06** | PARTIAL | `perf-evidence.md` §4.1（1M 上 20 页：`items=50`、`bytes=57 700`、`position` 999 954→999 004 严格递减、单页 2.0–3.6 ms）；wire `initial_load_cursor_walks_the_archived_range_without_overlapping_the_first_screen`（审计实测）；`session_history_page_test.rs:152`（空页仍推进游标，不假终点） | 20 页数字是**历史证据**（终局矩阵未复跑）；**R3** 反向读取的索引项触达随深度线性 |
| **A07** | PARTIAL | `perf-evidence.md` §5.2（6×1 250 000 B：独占一页、`oversize=true`、`position` 6→5→4、游标前进）；两阶段 = `HistoryPagePlan::plan`（`history_page.rs:597`）与 `materialize`（`:643`）+ `session_history_page_test.rs:189`（候选行不可解码证明阶段二不取它）；`history_page_test.rs:184/226/311` | 夹具是 **6 条不是 100 条**；峰值内存未采样；阶段一 `length(CAST(content AS BLOB))` 仍把最多 `limit+1` 条候选正文读进引擎寄存器（R2，静态+标志位） |
| **A08** | PASS | wire `the_same_cursor_replays_equivalent_pages_and_limit_only_changes_page_boundaries`（**attempt 2..=3 重放**，逐项等价断言）；SDK `concurrent calls with the same cursor and limit share one request`、`repeated and overlapping pages merge idempotently`（本次复跑 #5：25/0） | 未在 TUI 渲染层做「每条只显示一次」的端到端断言 |
| **A09** | PARTIAL | wire `a_cursor_survives_appends_and_another_full_without_moving_its_page_members`（审计实测）；`R4b-verification.md` §2.9；`S6-verification.md` 不变量 8b（追加/Full 不推进结构版本） | 服务端等价性与不失效成立；**「实时尾部正常更新」与「不重新注入旧历史」的客户端侧未端到端验证** |
| **A10** | PARTIAL | SDK `a tool result that arrives in a later page completes the earlier call without faking failure (A10)`、`the awaiting-result tool status is consumable as a constant`（本次复跑 #5） | 「**同时实时 turn 有同名 call ID**」未构造；`S7c-sdk-verification.md` §4 风险 6：工具卡按原始 `toolCallId` 全局合并（`doc-model.ts` 未改），严格读法未达 |
| **A11** | PASS | wire 跨会话/过期拒绝（`-32011`/`-32012`）；SDK `a page from a different snapshot is rejected whole (A11)`、`a page that arrives after rewind is rejected and the window is cleared`、`a page that arrives after close is dropped`（本次复跑 #5）；TUI `S7c-tui-verification.md` D2（实测+证伪） | 无阻塞缺口 |
| **A12** | PARTIAL | TUI `S7-tui-verification.md` REQ-C02 / `S7c` D8（无强制 reload，PASS）；SDK `S7c-sdk-verification.md` 判据 1–6、8 PASS、判据 7 含 1–2 条环境红牌（见 §8.1 与 §5.11.1），`session-docs.test.ts:535/561/582`（失败/取消收敛、终态不被改写） | `S7c-sdk-verification.md` §2.1：**`compact_started` 之前**的真实 Compact 失败（含 `/compact` 空历史 EarlyReturn）在 SDK 侧无任何展示（P3，非本轮引入） |
| **A13** | PARTIAL | `R4b-verification.md` 项 5/6（同一份页规则、未协商不出私有字段）；`remote/session_history_page_test.rs`、`session_history_rewind_test.rs` 镜像用例；`S9-boundedness.md` §3.5（4 条段 SQL 与本机逐字同形）；`final-adversarial-audit.md` 必查 4（本机 6+1+1 全绿） | **无跨后端差分执行**（两端是镜像期望，不是同夹具对拍）；**云后端 21 条 `#[ignore]` 未跑** |
| **A14** | PARTIAL（原「未验证」） | 相邻证据：`store/history.rs:43` `ForkedHistory::active_payloads`；**S12/S13 新增直接用例** `a_parent_full_after_fork_leaves_the_child_snapshot_and_pages_intact`（`peri-acp/src/dispatch/session_fork_test.rs`，走真实 `apply_compaction`）：fork → 记录子侧快照/首页事实 → 父压缩提交 → 断言子侧逐条等价；复核用「把父压缩目标换成 fork_id」注入使该用例红（`S12-verification.md` §4.4） | 覆盖范围**只在最小夹具内**成立：3 条父自有消息、首页一次读尽（`next_cursor = None`）、无 fork 前 Full、无多页游标链、无冻结祖先；**分页内容面只到首页**（S13 复核逐条核对，见 `S13-verification-behavior.md` §7） |
| **A15** | PARTIAL | 普通新会话 `nextCursor=null` 成立（wire `history_page_wire_test.rs:265-323` 的 `initial_load_cursor_walks_the_archived_range_without_overlapping_the_first_screen`，其中 `:269-271` 断言 `fresh["nextCursor"] == Null`；SDK `a first screen with no earlier content reports an exhausted window (A15)`）；**F1 已修复**：`the_initial_cursor_anchors_past_non_displayable_own_rows`（真实会话形状 own = [System 身份, h1, h2] + Full：锚点=摘要、h1/h2 经页链交付）、`the_fallback_anchor_keeps_an_archived_row_reachable`（兜底锚点分支的证伪器）、`an_all_non_displayable_own_region_reports_no_earlier_page`（§5.9） | F1 反例消失（§5.9）；剩余：**「不触发迁移/自动摘要」无负向用例** |
| **A16** | PASS | `compact_progress_test.rs:192` `test_budget_recovery_watermark_survives_shrink_and_missing_watermark_is_new_work`；`compact_pressure_adversarial_test` 10 passed（矩阵 it-log `final-matrix-logs/it-peri-agent-compact_pressure_adversarial_test.log`）；`final-adversarial-audit.md` §3.5/§4 A16 | 审计未逐条复跑这三组（引用矩阵日志） |
| **A17** | PARTIAL | `compact_session_manual_adversarial_test` 7 passed、`compact_session_adversarial_test` 8 passed（矩阵 it-log）；`compact_pipeline.rs` 有效集合对称校验（`R4-verification.md` §3 A17） | **手动 Compact 仍从完整快照重建临时 transcript（含归档正文）**（D9：判为读取成本、非语义泄漏）⇒ A17「不读取已归档正文」字面未达 |
| **A18** | PARTIAL | 审计实测 4 条：`stable/durable/committed/excluded_committed`（`handoff.rs:144-160` 三态由持久事实判定）；`receive_test.rs::committed_delivery_missing_from_hot_state_is_restored_without_reemitting`、`::excluded_committed_delivery_is_neither_restored_nor_reemitted` | **冷恢复重投面**（`async_tasks`/`external_settlement` 的 `test_cold_restore_*`）未复跑 |
| **A19** | **PASS**（第五版，D8 闭合） | `S6-verification.md` 不变量 8（rewind 后旧游标 `-32012`、`structure_version` 恰好 +1）；`requests_rewind_cases_test.rs:402-520`；`session_history_test.rs:780/812/835`、`full_remote_test.rs:964/991/1011`；本次复跑 #2（69 passed）；**本轮新增**（§5.1 第 5 条）：批次边界/摘要被删/新旧记录混用三条规则用例 + fork 重映射用例 + 本机/远端清零断言 4 条（含反向对照全红） | D8 的「原文／摘要重复覆盖」字面已达成（取代关系逐条判定，且无 DDL）；残留：①取代关系落地前归档的旧行走保守回退（只影响旧数据，方向是不丢内容）；②「重启后有效输入一致」仍只引用既有用例（非本片新增，未逐条复跑） |
| **A20** | PARTIAL | `S7e-verification.md` 判据 1~8 全 PASS（折叠组反例、拆组、场景 A/B 屏幕行不变；证伪成立）；wire 身份同源用例；SDK `a sparse first screen keeps the inherited prefix in front of the older own page (A20)`（本次复跑 #5） | **已声明边界**：锚点单元**自身**被折叠/拆组时不补偿（实测下方位移 0 行 / +4 行）；身份歧义形状仅静态判定 |
| **A21** | PARTIAL | wire `a_page_request_is_answered_while_the_session_holds_the_execution_lock`（审计实测，本次复跑 #1 同一目标内）；`handle_page` 短锁 + 锁外存储读（`S6-verification.md` 不变量 3） | 「MCP/绑定/队列/执行状态不变」**无运行时断言**（仅调用图证明）；「并发加载另一会话」未实测 |
| **A22** | PARTIAL | wire `a_failed_initial_replay_fails_the_load_instead_of_answering_a_cursor`（注入通知投递失败，`-32603`，审计实测）；SDK `replay is staged until the load response installs it in one pass (A22)`、`a failed load discards its staged replay, and a retry installs exactly one copy (A22)`、`replay from a superseded generation never reaches the window`（本次复跑 #5） | **「取消」形态未构造**（只有失败与代次交错）；「旧画面不冒充新会话就绪」属客户端契约，无服务端断言 |
| **A23** | PASS | SDK `sub items of one entry keep their own identity when they share a messageId (A23)`、`assistant and tool entries render without a Human prefix or an active turn (A23)`（本次复跑 #5）；回放投影 `project_payload_updates` 为唯一封装来源 | TUI 渲染层（整组件树 / 渲染帧）未跑 |
| **A24** | PASS | SDK `history pages never advance the live plan, tasks, queue, or current turn (A24)`；`the awaiting-result tool status is consumable as a constant`（断言输入队列不变，本次复跑 #5）；TUI REQ-C01-6 PASS（隔离用例，`S7-tui-verification.md`） | 「旧 TodoWrite 到达时间不得覆盖当前任务基线」只有 SDK 断言，无 TUI 渲染层断言 |
| **A25** | PASS | `history_page_test.rs` 10 例 + wire 14 例（含「拒绝不得触发一次 initial load」，审计实测 + 本次复跑 #1）；`S9c-verification.md` 6/6（三类连接 wire 实测）；错误码映射 `-32601/-32602/-32011/-32012`（`session_load.rs:14-24`） | 3 处保留：①`-32011/-32012` 是 Peri 自定码；②「重连能力变化」为构造性证据 + S9c wire 实测（无跨进程重连端到端）；③**完全跳过 `initialize`** 的进程内连接拿到 `all_enabled`（既有语义，已声明）。另见 F2 口径说明（§5.10） |

---

## 3. 两类成本（分开写）

需求 §9 末段原文：验收报告必须区分「初始恢复成本降低」和「持续浏览后累计缓存大小」。

### 3.1 第一类：初始恢复成本

**机制（静态）**：`load_active_snapshot` 走 7 条语句（meta → binding → frozen → active payloads → active flags → inherited → ancestor，`perf-evidence.md` §3）。读取条件里带 `excluded = 0`，**过滤在存储侧完成**；历史正文大小的 `SUM(LENGTH(content))` 聚合已从两后端元数据投影移除（`content_size` 现为常量 0）。

**实测数字（`perf-evidence.md` §4 与 §4.1，同一次默认运行）**：

| 归档行 | messages 行数 | 归档正文合计 | 恢复 cold p50 / p95 | 恢复 hot p50 / p95 | 恢复语句数 | 返回 payload | 解码正文字节 | 排除正文解码条数 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 10k | 10 003 | 11 518 890 B | 11.109 / 11.675 ms | 11.032 / 11.494 ms | 7 | 3（flags 1） | 196 971 | **0** |
| 100k | 100 003 | 115 288 890 B | 98.182 / 103.924 ms | 103.699 / 130.253 ms | 7 | 3（flags 1） | 196 971 | **0** |
| 1M | 1 000 003 | 1 153 888 890 B | **1 081.927 / 1 230.147 ms** | 1 084.641 / 1 282.421 ms | 7 | 3（flags 1） | 196 971 | **0** |

1. **成立的一半**：内存与解码字节**与归档规模无关**——三段解码正文恒为 196 971 B、被排除 own 正文解码 0 条；`perf-evidence.md` §5.1 的探针（归档正文写成不可解码占位）证明「一条都没进解码器」：对照组 `load_session_snapshot` 失败、处理组 `load_active_snapshot` 成功且字节不变。
2. **未达成的一半**：耗时**随归档行数线性增长**（每 10× 行数 ≈ 10× 耗时），成因见 §4。
3. **未覆盖**：`A04/A05` 的 1M 档在终局矩阵与审计中**均未复跑**（`final-test-matrix.md` §4 第 1 条、`final-adversarial-audit.md` §4.4 第 1 条）（第四版：A05 的 1M 档已由 §5.18.3 复跑覆盖，本段其余未覆盖项不变）；本报告只引用 `perf-evidence.md`，并以本次复跑的 200 行档确认**计划形状与探针口径**未变（恢复冷 p50 1.426 ms / 热 0.945 ms、行数探针 203、其中 excluded 200）。

### 3.2 第二类：持续浏览后累计缓存

1. **服务端侧（现状）**：每次分页是纯读、无跨请求页缓存（静态结构，`S9-boundedness.md` §5.2）。单次要持有的额外正文 = 「本页 + 至多一个候选条目」量级：阶段二只取本页行的正文（`history_page.rs` `body_ids()` 是唯一取正文集合，`R4b-verification.md` §2.4），但阶段一仍会把最多 `limit+1` 条候选行的正文读进 SQLite 引擎寄存器（R2）。
2. **唯一相关量级数字**：1M 规模连续 20 页、每页 50 条 / 57 700 B ⇒ 页内容合计约 `20 × 57 700 B ≈ 1.15 MB`（`perf-evidence.md` §8 末尾）。这是**页内容量级**，不是驻留内存测量。
3. **缺口（必须写清）**：
   1. 前端（TUI / SDK）**累计驻留零测量**——`S9-boundedness.md` §5.2「证据：无」、`perf-evidence.md` §7.6；没有测「翻到几千页后前端驻留多少」。
   2. 服务端跨请求驻留未测（每次纯读）。
   3. SDK 侧已明确记录风险：「分页不保证客户端累计内存恒定；SDK 与文档均无相反承诺」（`S7c-sdk-verification.md` §4 第 7 条），与需求 REQ-C03「不得宣称仅靠分页已经保证所有客户端累计内存恒定」一致。
4. **结论**：本报告**不宣称**分页保证累计内存恒定，也不给出任何「浏览 N 页后内存 = X」的结论。

---

## 4. 有界读取证据：查询计划 + 实际扫描工作量

需求 §9 末段原文：查询计划和实际扫描工作量共同作为有界读取证据，不能只看返回行数。

### 4.1 查询计划（`EXPLAIN QUERY PLAN`，对**生产语句原文**取计划）

1. 恢复链 7 条语句中，三条读 `messages` 的计划都是
   `SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)`：
   `load_active_payloads_on`（`SELECT message_id, content … WHERE thread_id = ?1 AND excluded = 0 ORDER BY rowid`）、
   `load_active_flags_on`（同形 + `truncated = 1 OR projection IS NOT NULL`）、
   `read_active_entry_positions`（`SELECT message_id, rowid … AND excluded = 0 ORDER BY rowid ASC`）。
   → **无 rowid 约束、无 LIMIT**；`idx_messages_thread_id` 只覆盖 `thread_id`，`excluded` / `content` 都在表里。
2. 运行期断言（10k/100k/1M 三段都过，`perf-evidence.md` §3）：① 语句文本与计划行**无 `SUM(`**；② **无 `SCAN messages`/`SCAN m`**（全表扫描）；③ 必出现 `USING INDEX`/`USING COVERING INDEX`/`USING INTEGER PRIMARY KEY`；④ 计划采集 ≤ 0.2 s（**用例内部断言阈值**，实测 0.179/0.153/0.166 s，**不是产品承诺**）。
3. 分页路径 4 条语句同样是索引定位：`read_history_page_bounds`（covering index + 相关子查询）、`read_history_page/阶段一`（`rowid < (锚点子查询) ORDER BY rowid DESC LIMIT ?3`）、`read_page_bodies/阶段二`（主键定位）、`require_session`。
4. 回放段读取（未协商标准客户端）：`S9-boundedness.md` §3.3(a) 字节码级判读——`SeekGT`（从 `(thread_id, after)` 之后定位）+ `IdxGT`（超过 `(thread_id, tail)` 即退出）+ `DeferredSeek` + `Next`，**索引项访问与表行取回都 ≈ `limit + 1`**。

### 4.2 实际扫描工作量（行数探针与触达上界）

| 路径 | 行数探针/触达上界（实测） | 结论 |
| --- | --- | --- |
| 恢复（`load_active_snapshot`） | 该线程索引范围内的行数 = **10 003 / 100 003 / 1 000 003**（= messages 全行数） | **触达行数 = O(线程行数)**；耗时随归档线性（11.109 → 98.182 → 1081.927 ms） |
| 分页（更早方向，表行与字节） | 每页 ≤ `limit + 1` 行；1M 上 20 页 `items=50`、`bytes=57 700`（首屏 251 209）、单页 2.0–3.6 ms | 表行与字节**有界**（与归档规模无关） |
| 分页（更早方向，索引项） | 第 k 页 ≈ `k × limit` 条索引项（`S9-boundedness.md` §3.3(b)：`?2 IS NULL OR rowid < (子查询)` 的 `OR` 让锚点无法成为索引约束） | **索引项触达随浏览深度线性**（R3；表行仍有界） |
| 回放段读取（标准客户端） | 每段索引项与表行 ≈ `limit + 1`（`REPLAY_SEGMENT_ROWS=50`）；段字节 = 写入字节（合计 246 873 B 与夹具逐字节相等，断言） | **两级都定界**（与线程总行数无关） |
| 单条候选行正文是否出库 | 阶段二探针：把「多读的那一条」写成不可解码正文，本段仍成功、下一段才失败 | **阶段二只取本页正文**（候选行正文不进解码器） |

### 4.3 结论与边界（必须一起读）

1. **恢复路径仍是 O(线程行数)**（D10 / `perf-evidence.md` §8、`S9-boundedness.md` §6 R4）：内存与解码字节有界，触达行数无界。**修复所需索引（覆盖 `(thread_id, excluded)` 或等价）属 DDL**——需求 §8 明确不授权直接执行 DDL，仓库根 `CLAUDE.md` 原则 12 要求用户批准；**本任务未改 schema**，批准后另开切片复跑三规模即可闭环。
2. **分页反向读取的索引项触达随深度线性**（R3），表行与字节有界；`perf-evidence.md` §8 原来的「分页不受影响」指的只是表行，`S9-boundedness.md` 已收紧该表述。
3. **标准客户端完整回放的「有界」是有限定的**：canonical 历史不再整份物化、新增向前段读取行触达 ≈ `limit+1` 成立；但**有效集合仍整份物化**（`prepare_existing`），该读取触达行数 = O(线程行数)（`S9-boundedness.md` §0 最后一行、§5.1）。同时：回退分支（后端不支持段读取）仍走全量物化（R7，有 warn、交付内容完整）。
4. **计划形状有界 ≠ 触达行数有界**——这是 `perf-evidence.md` §8 的核心发现，本报告据此把 A04 判为 PARTIAL、A05 判为 FAIL（A05 的 FAIL 已于第四版随索引实施撤销，见 §5.18.3）。

## 5. 已知偏差与未验证清单（单列，不混进 PASS）

### 5.1 D8：两次以上 Full 后 rewind 到排除段内部的保守整段清零（**第五版已闭合，且未使用任何 DDL**）

1. **原判（第四版及以前）**：`excluded` 连续段无法从现有 flags 判定压缩批次边界（`MessageFlags` 只有 `truncated`/`excluded`/`projection`）。实现采保守策略：**整段清零**——内容不丢，但可能「旧摘要与其覆盖的原文同时有效」，与 A19「无原文／摘要重复覆盖」字面不符。当时的修复条件写的是「需新增『覆盖批次』最小元数据 = DDL」，用户已授权该 DDL。
2. **裁决（第五版）：不加列、不推进 schema 代数**——把取代关系作为字段放进**既有**持久化载荷列 `messages.projection`（其 JSON 形状即 `MessageProjectionDirective`）。新增 `superseded_by: Option<MessageId>`（`#[serde(default, skip_serializing_if = "Option::is_none")]`），语义 = 「取代本条的摘要消息 id」，只在 `excluded` 为真时被读取；写入时 `entries` 为空，因此它不产生任何渲染动作。
3. **为什么放弃新列路线（两条独立侦察的结论，均以代码为准）**：① 新列会让**已存在的 Current 库**在只读 probe 阶段就被拒（`schema_shape.rs` 的 `check_strict` 要求等长且逐位同名；`sqlite_store/schema.rs` 的 Current 判定失败后走 input 判定，而 `INPUT_TABLES.messages.required` 复用同一份列清单 ⇒ fail-closed `UnsupportedDatabaseSchema`，到不了 Current 分支），且仓库自述「形状变化一次推进一代」⇒ 需同时推进本机第 12 代与远端 v6，同步点 30+ 处；② 新列还会让**旧二进制**把库读成漂移并拒绝打开，该回归不可消除。`projection` 列则已在本机与远端两侧的 SELECT/INSERT/UPDATE 与解码链上贯通：新字段读写零改造、两向兼容（旧行为 `None`、旧构建经 serde 忽略未知字段，两侧都不拒绝历史）。
4. **实现（四个面，单一规则实现不变）**：① 写入侧 `peri-agent/src/agent/compact_v2/full.rs`：提交前分配摘要 id，`flag_updates` 每条归档条目写 `superseded_by = Some(summary_id)`，追加的摘要消息用**同一个** id（`build_summary_message(summary, id)`）；② 规则侧 `peri-acp-types/src/store/history.rs::rewind_clear_excluded_ids`：有记录 ⇒ 精确判定（`retained_ids.contains(&summary)`），无记录 ⇒ 保留原前缀结构回退；③ 清零侧：本机 `clear_rewind_excluded` 与远端 `CLEAR_EXCLUDED_SQL` 变为 `excluded = 0, projection = NULL`（载荷随清零一并消失，`truncated` 不动）；④ fork 重映射 `remap_projection_references` 连同 `superseded_by` 一起改写。
5. **证据（本轮实跑）**：`peri-acp-types --lib` **580 passed / 0 failed**（新增 `rewind_between_two_fulls_keeps_the_batch_whose_summary_survives`（批次边界，只清摘要被删那一批）、`rewind_dropping_the_summary_clears_the_batch_it_covered`、`rewind_judges_recorded_and_unrecorded_entries_by_their_own_evidence`（旧行与新记录混用互不干扰）、`fork_remap_rewrites_superseded_by_to_the_forked_summary`）；`peri-agent --lib` **1089 passed / 0 failed**（`full_test.rs` 扩展断言：归档条目 `superseded_by()` == 追加摘要 id、摘要自身不被排除、未排除条目为 `None`；`compaction_loop_test.rs` 的旧断言按新语义改写为「被排除行 `entries` 为空 + 取代关系严格向后推进并落到仍可见的摘要」）；`peri-resources --lib` **504 passed / 0 failed / 26 ignored**（本机/远端各两条清零断言；**反向对照**：把两处 SQL 改回只清 `excluded` ⇒ 4 条断言全红）；`peri-acp --lib` **893 passed / 0 failed**。
6. **链式取代是正常形态**（本轮实测发现，已写进代码注释与索引）：第二轮 Full 会把第一轮摘要也归档并指向第二条摘要 ⇒ `superseded_by` 是链而不是一跳。rewind 逐条判定后得到的是「旧摘要复活 + 它覆盖的正文仍归档」（正确），而不是「两条摘要同时有效」。
7. **残留与边界（声明，不影响 A19 判定）**：① 取代关系落地**前**归档的旧行没有记录，仍走保守回退——方向是「不丢内容」（宁可让原文生效），可能重复覆盖，但只影响旧数据、且不需要迁移；② 清零时整条 `projection` 载荷一并清掉：不丢渲染信息（生产里 `excluded: true` 的唯一写入点用 `..Default::default()` **整体覆盖** flags，被排除行本就不带渲染指令）；③ 未新增列 ⇒ 无 schema 代数变化、无升级路径改动、旧构建读到未知 JSON 字段被忽略。
8. **文档同步**：蓝图 §7 D8（`spec/issues/2026-10-13-transcript-shrink-history-pagination.md`）、`.recon/rewind-review-findings.md` F7、`docs/code-index/{peri-acp-types,peri-agent,peri-resources}.md`、以及 `projection.rs` / `store/history.rs` / `session_resources.rs` / `clear_rewind_excluded` / `CLEAR_EXCLUDED_SQL` 的 doc comment。

### 5.2 D10：恢复路径触达行数线性（A05：授权前 FAIL → 索引实施后达成）

1. **授权前实测（基线，保留）**：cold p50 11.109 / 98.182 / 1081.927 ms（10k/100k/1M）；行数探针 10 003 / 100 003 / 1 000 003；三段返回的有效集合完全相同（3 条 / 196 971 B）。
2. **机制（授权前）**：`ACTIVE_PAYLOADS_SQL` / `ACTIVE_FLAGS_SQL` / `ACTIVE_ENTRY_POSITIONS_SQL` 的计划都是 `SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)`，该索引不覆盖 `excluded`/`content` ⇒ 逐行回表判定。
3. **修复与闭环（2026-10-14）**：用户显式授权新增 `(thread_id, excluded)` 索引 ⇒ 落点 `canonical.rs::CREATE_INDEXES`（`idx_messages_active`）+ 本机 Current 分支幂等索引重放；复跑（`.recon/a05-raw/perf-rerun.out`、`perf-evidence.md` §4.2/§8.1）：cold p50 **3.299 / 3.379 / 2.435 ms** 跨规模持平，计划三条语句全部改为 `… USING INDEX idx_messages_active (thread_id=? AND excluded=?)`；代价 ≈25 B/行（1M 档 +25.3 MB ≈1.7%），升级首次补建耗时未测量。细节见 §5.18.3。

### 5.3 R1：回放中途 rewind 删 `tail` 行 → SQLite 复用 rowid（「行集合同源」不成立）

1. **实测**（`S9-boundedness.md` §3.4 第 6 条，反例输出「续段 payloads=1 含新行=true 含被删行=false」）：删除 `tail` 那一行（模拟 rewind 尾部切除）再追加新消息时，**SQLite 把被删行的 rowid 复用给新行**，新行落进 `(after, tail]` 并被本次回放交付，**被删内容静默消失、无任何错误**。
2. **触发条件**：同会话并发（一个连接在流式 load、另一个连接发 rewind/精确删除）。回放游标**没有结构版本**（分页游标有 `structure_version`）。
3. **归属**：修复（回放游标带结构版本 / 每段校验行集合同源）属设计变更，**另开切片**；本轮只把 doc 按事实收紧（蓝图 §7 R1）。**远端同样存在**（规则只有一份），且远端缺「删 tail 行 + 追加」用例。

### 5.4 R2：`length(CAST(content AS BLOB))` 关掉 SQLite 的 length-arg 优化（措辞收紧）

1. **实测（字节码标志位）**：生产语句 `Column p5 = 0`，对照组（自造、非生产）`p5 = 64 = OPFLAG_LENGTHARG` ⇒ 生产语句的长度是**在取回的值上算的**。
2. **后果**：阶段一每条候选行（含永不返回的 lookahead 行）的正文仍被引擎读入寄存器（不越过 SQL 边界、不进解码器、不改变返回集）。`session_resources.rs:714-717` 的「不取正文」字面过强。
3. **状态**：**只报告，未修**；doc 已按事实收紧。

### 5.5 R3：分页反向读取的索引项触达随深度线性（表行仍有界）

1. **实测（字节码）**：`?2 IS NULL OR rowid < (标量子查询)` 的 `OR` 让锚点无法成为索引约束 ⇒ 反向从该线程最大 rowid 起步（第 k 页 ≈ `k × limit` 条索引项）；被跳过的行**不取表行**。
2. **口径**：`perf-evidence.md` §8 的「分页不受影响」指**表行**；索引项触达不是常量。`S9-boundedness.md` 已收紧。

### 5.6 D12：回放标记门控（已修复并复核）

1. **缺陷**：`periReplay` / `periLoadGeneration` 当时读**会话级** caps 注册表 ⇒ 该会话曾被 peri 连接写过之后，未协商标准 ACP 连接也会收到这两个字段（`S9-verification.md` §7.1 实测反例；违背 §5.3「能力版本以**当前连接**的协商结果为准」）。
2. **处置与复核**：改为 `ConnectionContext::caps()`（未 `initialize` 的进程内路径保持 `all_enabled` 的 TUI 语义）；`S9c-verification.md` 判据 6/6 PASS（三类连接 wire 实测 + 负面控制）；分页四项字段**从未泄漏**。

### 5.7 D14：锚点补偿跨层槽位坐标（已重做并复核）

1. **缺陷（S7d 引入）**：`HISTORY_PREPEND_MARK` 存的是桥接侧 `state.committed` 下标，消费方却用它索引发布后的 `snapshot.items` ⇒ 「相邻成功工具卡压成折叠组」时两个方向都会出错（复核实测：桥接 4 槽 vs 渲染 3 槽；`S7d-verification.md` §5.1/§5.4 判 FAIL）。
2. **处置（S7e）**：改按**渲染侧身份重定位**（顶部可见单元身份 + 单元内位移，和合后按同一身份重查）；跨层槽位包络语义从 `HISTORY_PREPEND_MARK` 移除。
3. **复核**：`S7e-verification.md` 判据 1~8 全 PASS；折叠组反例、拆组、场景 A/B 四形态锚点屏幕行不变；**证伪成立**（短路重定位后三条用例 FAIL，跳 1/3/4 行）；跨层槽位路径已无代码引用（全量 grep）。

### 5.8 A20 的锚点边界（设计取舍，已显式声明）

1. **不补偿的形状**：锚点单元**自身**被折叠或拆组时，其身份在新窗口不存在 ⇒ `relocate` 返回 `None`（安全方向，**不补偿**）。实测：一种形态下方位移 **0 行**（高度恰好相等），另一种 **+4 行**（组展开的布局增量）。
2. **不可同时满足**：「锚点内容被压组」与「上方重排时下方不动」二者互斥；模块文档已显式声明代价与移除条件。
3. **静态残留风险**：窗口内出现两枚**内容完全相同**的同类单元时判为歧义 → 不补偿（**未构造端到端反例**，仅静态判定）。

### 5.9 F1（P1，本轮已修复并独立复核）：初始游标锚点落在不可展示条目

1. **根因（修复前）**：`peri-acp/src/dispatch/session_load.rs` 的 `initial_history_cursor` 取 `effective.payloads.first()`，**不检查可展示性**；System 条目永不进入排除集合（`full.rs:86-101`、`transcript.rs:813`）、又永远不进入客户端渲染。
2. **真实来源**：`peri-agent/src/session/subagent/factory/spawn.rs:348-352`（subagent / fork / bg 子会话在 own 区写身份 System；全仓唯一 append System 的生产路径）。
3. **修复**：锚点改为「有效视图里最旧的**可展示**自有条目」，判据用 `PersistedPayload::displayable_to_client()`（`peri-acp-types/src/store/mod.rs:66`）——与分页出口装配 `items` 的过滤**同一条规则**，不在 ACP 层另立判据；有效视图里一条可展示条目都没有时退到**最新**一条自有 payload（同样不在首屏展示，故既不会漏也不会重），是否收尾仍由 `range_head` 判据决定。**不涉及 DDL、不改 JSON 形状、不改错误码与 caps 门控**。
4. **回归用例（`peri-acp/src/host/history_page_wire_test.rs`，调用真实 `dispatch::initial_history_cursor` / `load_history_page`）**：
   1. `the_initial_cursor_anchors_past_non_displayable_own_rows`：own = [System 身份, h1, h2] + 一次 Full（排除 h1/h2、追加摘要）→ 锚点 = 摘要、`nextCursor` 非空、页链按历史正序交付 h1/h2、与首屏零重叠、读到范围起点收尾。
   2. `the_fallback_anchor_keeps_an_archived_row_reachable`：有效视图 = [身份 System, 仅模型受众 reminder]（一条可展示条目都没有）而存储里仍有更老的可展示归档行 → 兜底锚点 = **最新一条有效自有条目**，那行归档正文仍被交付。**形状前提里的「摘要被移除后正文仍被排除」需要绕过 `full.rs` 直接调 `apply_compaction` 才造得出**（见第 6 条边界），它钉的是兜底分支的**选择**而不是一个生产可达路径。
   3. `an_all_non_displayable_own_region_reports_no_earlier_page`：own = [身份 System] → `nextCursor = null`（不编造游标）。
5. **证伪**：把实现临时改回 `payloads.first()` → 用例 4.1、4.2 **FAILED**（"存储里有压缩原文，不得谎称没有更早历史" / "存储里还有一条更老的可展示行，不得谎称没有更早历史"）；恢复实现后转绿。独立复核者另写 7 个探针（真继承前缀子会话、两轮 Full、空页链、rewind、兜底分支、边界形状），**旧实现下 5 个红、修复后 7/7 绿**（`.recon/S11-verification.md` §9），全部临时改动已 sha256 字节级还原。第一版的两层审计探针中，wire 层那条已被转正为上述用例 4.1，存储层 `zz_audit_history_anchor_probe.rs` 已删除——它复刻的是 ACP 侧判据，修复后会成为过期副本。
6. **影响面变化**：A15 的 F1 反例消失（FAIL → PARTIAL，剩余缺口是缺一条"不触发迁移/自动摘要"的负向用例）；A20、A06 中"System 锚点会话拿不到历史页"这一条一并解除。
7. **独立复核（`.recon/S11-verification.md`，判据 1/2/3/4/6/7 PASS、判据 5 PARTIAL 非阻断）**：结论"**该修复可以交付**，无生产可达的反例"；复核者的两条非阻断建议（补兜底分支证伪用例、注释写"最新一条**有效**自有 payload"）**均已采纳**（即用例 4.2 与实现注释）。判定依据还包括：生产不变量「被排除的可展示行 ⊂ 最旧那一段」经 grep（唯一写入者 `full.rs:96`）与探针断言验证；判据同源经函数级比对（锚点与 `assemble_history_page` 都调同一个 `displayable_to_client()`）；同类残留全仓唯一。
8. **本轮登记的既有边界（复核者 §10 第 4 条，与本修复无关但须知悉）**：整套分页契约的页链是「严格早于锚点」的单向链；若将来有调用方写出**非前缀排除**（只归档一条中间消息并追加摘要），比锚点更晚的归档行会不可达。当前唯一写入者 `full.rs` 的排除选择规则 + rewind 的清零规则保证该形状不可达（`S11-verification.md` §2 的 I1 探针）。修复需另开切片（页链改成按位置区间浏览）。
9. **修复轮的实施记录与证伪原始输出**：`.recon/S11-f1-fix.md`。

### 5.10 F2（P2，口径相关，未修）：未协商回放通道泄漏 `_meta.peri.subagentFailure`

1. **实测**：未协商连接在 `session/load` 回放中收到 `_meta.peri.subagentFailure`（含子线程 id、provider、request_id、错误详情）；协商对照连接证明夹具非空转（`.recon/zz-probe-subagent-failure-output.txt`）。
2. **判定**：按需求 §5.3 **字面**（分页私有语义 + 完整回放契约）**不算违反**；按审计加严口径（任何 Peri 私有字段）**不成立**——它是第 7 个私有字段，且出现在**回放通道**（`event/tool_projection.rs:33-37` 无门控；`session_replay.rs:635-646` caps 关闭时只做加法不做剔除）。
3. **同源范围外项**（`S9c` 已点名，本轮不修，均**不是回放字段**）：① `_meta.peri.subagentFailure` 无门控；② 命令广播面（`send_available_commands_update`、`session/resume`）仍读会话级注册表；③ live 通道 reminder 形状仍读会话级注册表（回放出口已改）。

### 5.11 SDK 残留 P3（非本轮引入）

1. **`compact_started` 之前的真实 Compact 失败在 SDK 侧无展示**（含 `/compact` 在空历史上的 EarlyReturn）——`S7c-sdk-verification.md` §2.1。若把 A12 读成「所有 Compact 失败都要有失败展示」，应记为部分成立。
2. 其他 P3（本轮不修，见 `S7c-sdk-verification.md` §4）：归因启发式（Compact 窗口内其它命令的反馈被记为 Compact 终态）；首屏回放占用 `session.stream()` 有界队列预算（溢出为显式 `EventStreamOverflowError`）；旧用例命名与语义不符；README 未记录上述边界；工具卡按原始 `toolCallId` 全局合并（与 REQ-H03「稳定调用实例身份」的严格读法有差距）。
3. **A12 的「已扩大历史窗口 + 未决交互保留」**：SDK/TUI 各有隔离用例，但**未决交互在 Compact 后保留**没有端到端断言。

### 5.12 前端覆盖缺口

1. **人工用例**：`docs/verification/tui-integration-cases/06-transcript-ui.md` 已登记 **P0-77**（滚动加载更早历史 + 迟到页/游标），README 执行顺序同步（S7d 判 PASS）；**但人工用例未执行**。
2. **`e2e/` 仍无翻页自动化用例**（`final-test-matrix.md` §4.第 4 条：`e2e/` crate 未运行；`final-adversarial-audit.md` §4.4 第 4 条）。
3. TUI 渲染帧 / 整组件树 / 真实端到端**未跑**（只到状态层、事件层与决策层的探针）。

### 5.13 其他已记录偏差与观察（不属阻塞项，但须知悉）

1. **`ThreadMeta.content_size` 恒为 0**（`row_mapping.rs`）：**无生产消费者**——只有定义、2 处测试夹具（`peri-tui/src/cli_meta_test.rs:31`、`peri-tui/tests/meta_session_cli.rs:119`）与 1 处遗留 filesystem 后端的写入点（`sqlite_store/filesystem.rs:285-290`，`filesystem_test.rs:264-275` 断言 `> 0`），属**行为降级**而非破坏——这是 A04 消除全历史聚合的代价（`R4-verification.md` §3、`R4b-verification.md` §3 C）。
2. **未协商回放的失败语义变化**：回放投递失败现在使 `session/load` 失败（`-32603`），不再 warn+继续；服务端会话态保持完整，留给下一次 load 原位修复。
3. **回退分支仍全量**（R7）：后端不实现 `load_history_segment` 时回退 `load_session_history` 全量读取（有 `tracing::warn!`，只在未交付任何内容前触发）；两个生产后端都实现了段读取，风险等级低但「有界」在这条分支不成立。
4. **继承区不进预算**（R5）：首段整份交付 `threads.inherited_context`（单行 JSON），内存上限不受 `REPLAY_SEGMENT_BYTE_BUDGET` 约束。**单条超预算行独占一段**（R6），代价是单段内存可达该行大小。
5. **测试基建 flake（R8 + 本轮新增观测）**：
   1. `streamed_canonical_replay_falls_back_to_the_full_read_when_the_segment_read_is_unsupported` 在默认并发下偶发失败（日志断言），单跑/串行通过；产品断言两次运行都通过。
   2. **`peri-resources` 冷开锁超时抖动**（独立复核者 6 跑 2 红，本节作者另遇 1 次）：`connection_open_test.rs` 的 `canonical_database_and_symlink_aliases_share_one_lock_inode` / `incompatible_schema_is_rejected_without_wal_or_database_changes` 在并行负载下报 `session database initialization is busy`（来自 `connection.rs` 的 `SCHEMA_OPEN_LOCK_TIMEOUT = 5s`）。归属判定：`peri-resources` **不依赖** `peri-acp`，这两个文件也不在任何未提交改动里 ⇒ **预先存在**、与本轮改动无因果关系；建议单独立项跟踪（未改任何超时或测试）。与 §5.16 的锁回归用例重做是两回事：后者只改 `connection_open_test.rs` 的探测写法与用例，**未改** `SCHEMA_OPEN_LOCK_TIMEOUT` 或产品锁路径。
6. **R9 的过期偏差（已澄清，不再列为现状）**：`S9-boundedness.md` §6 第 9 条曾记录 `session_history_segment_test.rs:4` 指向不存在的远端文件；本次核对该文件第 4 行**已指向存在的** `remote/session_history_page_test.rs`（全文仅此一处 `remote/` 引用）⇒ R9 在当前工作区**不成立**，它是记录时的引用错误，不是现状。
7. **断言强度**（R10）：`assert_segment_plan_bounded` 不检查 `USE TEMP B-TREE FOR ORDER BY`、也不检查行触达；「行数有界」目前由审计的字节码判读支撑，**未固化成断言**。
8. **SDK 测试（S13 后已更新）**：此前 `bun test` 全量 252 pass / 2 fail（默认环境）或 253 pass / 1 fail（解除 `mise` 阻塞后），失败为 WASM telemetry 导出 panic（`langfuse-client/src/batcher/recovery.rs:256`，基线 `7f0de56e8` 引入，**非本切片引入**）。S13 已修该 panic 与夹具 gzip 解析，现全量 **254 pass / 0 fail**；**修复保留**，但红→绿链不可独立复核，见 §5.17。
9. **TUI 实现者证据不可信（流程记录）**：S7c 的 TUI 修复 agent 在 200 轮迭代上限被中断且一度使 lib test 不可编译，S7c/S7e 的权威记录是**复核者报告**（`.recon/S7c-tui-fix.md` 不存在）。
10. **未验证清单（宁多勿少）**：
    1. A04/A05/A06/A07 的性能用例（`history_perf_tests` 5 条 `#[ignore]`）未在终局矩阵复跑（第四版：这 5 条已在 A05 复跑中全部执行、全绿，见 §5.18.3；**终局矩阵本身仍未复跑**）；云后端 21 条 `#[ignore]` 未跑；人工 TUI 用例、`e2e/`、真实外部 stdio 客户端、真实云端一致性均未跑；「HEAD 源码同源对照」未做（禁 git 写操作 + 磁盘限制）；峰值内存 RSS 未采样。
    2. `peri.sessionWorkspaceV1` 键的**门控来源未定位**（`final-adversarial-audit.md` §3.3 补充事实、§4.4 第 11 条）。
    3. 「拒绝不得触发一次 initial load」只有 `history.len()` 不变的**间接**证明，**缺通知全量断言**（同文 §3.2 缺口、§4.4 第 14 条）。
    4. 本轮 F1 修复的独立复核报告为 `S11-verification.md`；其**未覆盖**：真实 TUI/SDK 端到端（只到 ACP 层真实 API），以及「锚点落在历史中间（更早方向仍有可展示行）」的客户端消费面。
11. **实验副本 DDL 越界（如实披露，S12 复核发现）**：S12 侦察沿用前序 A2 的一次性实验脚本时，曾在**副本库**（`.recon/a2-raw/probe-ab.db`、`size-probe.db`、`timing.db` 等，非仓库路径、非任何 Peri 会话库）执行 `CREATE INDEX idx_active_b ON messages(thread_id) WHERE excluded = 0`（`index-probe.sql:16/21/24` 的 CREATE/DROP 序列；`S12-verification.md` §2.7 在 `sqlite_master` 中观察到残留）。用户约束是「不授权直接执行数据库 DDL」，**没有实验副本豁免** ⇒ 如实记为越界。生产 schema / 迁移 / 列 / 索引**零改动**（§6.3 第 2 条按生产 diff 口径成立）；此后全部实验**只读**（S13 独立实验为只读 + VM 指令计数）。

### 5.14 拒绝分支持久化与 A14 最小直接用例（S12→S13）

1. **拒绝/失败路径覆盖：由临时探针转为持久 wire 用例**。第一轮审计用的是未跟踪探针（随 F1 修复删除），不入库、不可回归。S13 在 `history_page_rejection_test.rs`（518 行，按职责拆分后）落了三条持久用例：`R6`（`:276`，逐段读取失败时的错误码与零回放通知）、`R9`（`:351`）、`R10`（`:441`），全部走真实 `ServerLoop` + MPSC transport + `dispatch_request`。加上第一轮的八类（R1/R2/R3/R4/R5/R7/R8/R11），拒绝/失败路径**类别覆盖 11/11**；如实说明：这是**类别**覆盖，不是每个参数变体穷尽。
2. **证伪证据**：注入「异常回放通知」后三条用例 **3/3 红**；生产文件按 `shasum` + `diff -q` 校验字节级还原（`S13-wire-split.md` §3）。断言含错误码、零回放通知、热态与存储事实不变。
3. **旧记录更正**：`.recon/S12-gap-tests.md` 的「9/11」经复核实为 **8/11**（口径差异：第一轮实测八类），S13 后为 **11/11**；该文件已补 S13 修订记录。
4. **断言强度披露（不列为 PASS 依据）**：存储侧只比较**条数**（`load_session_history(...).len()` 与热态四元摘要），**等长改写不被检出**——内容等价性没有断言（`S13-wire-split.md` §7.1 第 3 项、`S13-verification-behavior.md` §2）。
5. **A14 最小直接用例**：新增 `a_parent_full_after_fork_leaves_the_child_snapshot_and_pages_intact`（`peri-acp/src/dispatch/session_fork_test.rs:332`，走真实 `apply_compaction`）：fork → 记录子侧快照/首页事实 → 父侧压缩提交 → 断言子侧逐条等价；把父压缩目标换成 `fork_id` 注入后用例红（`S12-verification.md` §4.4）。**覆盖边界**：3 条父自有消息、首页一次读尽（`next_cursor = None`）、无「fork 前 Full」、无多页游标链、无冻结祖先；内容面只到首页（`S13-verification-behavior.md` §7）。A14 因此由「未验证」降为 **PARTIAL**。
6. **口径更正**：该用例实际是「fork 后父会话**单次** `apply_compaction`」，此前记录的「第二次 Full」不成立（已在 `S12-gap-tests.md` 修订记录中更正）。

### 5.15 文件规模治理（STD-SIZE-001，按职责拆分，语义零改动）

1. **对象**：本交付触及且超 1000 行的 **10 个**源码/测试文件，全部按职责拆分（不是按行数切断）：
   - `peri-acp`：`history_page_wire_test.rs` 1935 → 父 333 + 5 子模块（557/168/518/127/542）；`requests_rewind_cases_test.rs` 1722 → 父 243 + 5 子模块（535/136/356/191/288）；`requests_unnegotiated_replay_test.rs` 1223 → 父 516 + 3 子模块（296/145/282）。
   - `peri-acp-types`：`session_resources.rs` 1034 → 990 + `session_resources/history.rs` 66（公开路径经 `pub use` 不变）。
   - `peri-agent`：`transcript.rs` 1064 → 805 + `transcript/entry.rs` 107 + `transcript/persistence_port.rs` 191；`transcript_test.rs` 1101 → 父 33 + 3 个测试子模块（436/563/105）。
   - `peri-tui` / `peri-resources`：`system.rs` 1013 → 502 + `system/interaction.rs` 264 + `system/bg_task.rs` 294；`session_events_test.rs` 1021 → 239 + 388 + 407；`session_history_perf_test.rs` 1361 → 800 + 355 + 222；`full_remote_test.rs` 1029 → 740 + 299。
2. **语义零改动证据**：各拆分均以「区间搬移 + 逐块字节比对」执行，差异只允许声明项（`pub(super)→pub(crate)`、`#[path]` 挂载、`pub use` 重导出、模块文档）；断言与函数体零改动；挂载沿用仓库先例（`#[path]` / 文件树子模块）。
3. **挂载真实**：拆分产出的用例文件逐个过滤运行均**非空跑**（复核抽查 23 个文件各 1 passed）；五个 crate 拆分前后 lib 计数一致（`peri-acp` 893、`peri-tui` 1859、`peri-resources` 499、`peri-agent` 1089、`peri-acp-types` 576）。
4. **独立复核**（`S13-verification-size.md`）：183 个非 `.recon` 源码/测试文件全部 ≤1000 行（最大 990）；职责拆分、挂载、断言零改动均 PASS；五个 crate 独立全量 **4916 passed / 0 failed / 32 ignored**。
5. **复核 FAIL 的口径与已处置部分**：复核按「含 `.recon/` 证据的完整文件集合」口径判 FAIL（381 文件中 14 个超限，全在 `.recon/`）。其中 2 个是**未忽略的 `.rs` 原始副本**（`s13-acp-tests-raw/original/`）：本节作者核对 `sha256` 与 `git show HEAD:<path>` 字节一致后**已删除**（可从 HEAD 随时取回，不损失信息）；其余为日志/`.orig` 副本，属**证据归档**而非源码，不做机械拆碎——本节作者裁决：`STD-SIZE-001` 的适用对象是源码/测试，证据日志按归档保留（复核者明确不擅自豁免，故由本节作者明示此裁决）。**补充核对**：存留的 6 个 `.rs.orig` 副本（`s13-size-agent-raw/`、`s13-size-tui-resources-raw/`）`sha256` 与拆分前提交 `3ccf75864` 的对应文件逐字节一致 ⇒ 无独有信息，保留只为阅读便利，随时可由 git 取回。
6. 复核另发现 `git diff --check` 门禁失败（`requests_unnegotiated_replay_test.rs:517` 文件尾空行），本节作者已修复；全仓 `git diff --check` 现 **exit 0**。另：拆分在 `history_page_wire_test.rs` / `requests_rewind_failure_paths_test.rs` 各留下 1~2 个多余空行，被第四次提交前的 `lefthook` `cargo fmt --check` 拦下并已 `fmt --all` 归零（见 §6.4 第 6 条）。

### 5.16 锁回归重做与残留调用点缺口（A1，S13）

1. **S12 的问题**：当时的锁用例结论不可靠（零预算探针 3 跑 1 绿），「回退真实调用点会红」的证伪不成立（`S12-verification.md` §4.1–§4.3、§7.2）。
2. **S13 重做**：用**显式握手**（另一进程取得 `.schema-lock` 后写 `holder.ready`；父进程读到 ready 才探测；`holder.release` 在**被测调用返回之后**才写）建立确定顺序，不再依赖时长猜测。判别断言是「等待不得短于预算」（修复态实测 **252.794 ms**；退回零等待实测 **0.484 ms**）。新用例 `release_probe_waits_out_an_external_holder` 单跑 **6/6 绿**；`peri-resources --lib` 全量 **3/3 绿，499 passed / 0 failed / 26 ignored**（`S13-lock-test.md`）。
3. **修复与保护面**：四处真实调用点（`connection_open_test.rs:168/329/396/453`）不再各自携带预算字面量，统一走 `probe_initialization_lock_release`（`:21-22`，预算常量 `LOCK_RELEASE_PROBE_BUDGET = 250ms`）；新用例走**同一个函数**——把该入口退回零等待写法，用例 **3/3 红**。生产锁路径（`connection.rs`）**未改**。
4. **残留边界（独立复核实测，A1 判 FAIL）**：把四处调用点**绕过共享入口**、各自内联回 `Duration::ZERO` 时，新回归仍 **3/3 绿**。即：守卫覆盖的是「预算的唯一来源（helper）」，不是调用点自身的写法；调用点级绕过属未覆盖写法（`S13-verification-behavior.md` §4）。
5. **容忍窗口**：250 ms 是容忍窗口而非机制修复——本机实测外部持锁 100 ms → 绿、350 ms → 红（边界落在 (100ms, 350ms]）。使用方需知：>250 ms 量级的继承持锁仍会红。
6. **结论**：本条为 **PARTIAL**（确定性证据达标、唯一预算来源受保护），调用点级绕过缺口**如实声明为已知局限**，不冒充「已护住四处调用点」。（第四版：本条已由对抗裁决结案——判别断言改用独立下界、拒绝结构性闭合、调用点级缺口维持已声明局限，见 §5.18.2。）

### 5.17 WASM telemetry 修复：保留，但红→绿链不可独立复核（A3，S12→S13）

1. **对象**：`langfuse-client/src/batcher/recovery.rs:256` 在无线程目标（WASM）上 telemetry 导出 `spawn_blocking` 触发 panic（`OS can't spawn worker thread: Not supported (os error 138)`），使 SDK 真实 WASM 集成触发 abort。属**基线既有缺陷**（`7f0de56e8` 引入），非本切片引入。
2. **修复**：无线程目标就地编码（native 保留原 blocking 分支）；S12 曾只做到 emscripten 交叉编译检查，无运行时红→绿证据（`S12-verification.md` §4.8），S13 补做运行时闭环（`S13-wasm-closure.md`）。
3. **实施者报告的红→绿链（待核实陈述）**：三次重建（修复在位 `ac7f334b…` ×2 相同 vs 退回 HEAD `7b45dfa5…`，产物哈希随源码变化）；退回后目标用例红（上述 panic + `Aborted()`，`TARGET_EXIT=1`），字节级还原后同一用例绿（`TARGET_EXIT=0`）；另修掉夹具侧 gzip 解析问题（按 magic `1f 8b 08` 解压，产品 gzip 契约未动）。绿灯覆盖：目标用例 1 pass、该文件 2 pass、SDK `bun test` **254 pass / 0 fail**（S12 记录 252/2）；native `langfuse-client --lib` **292 passed / 0 failed / 1 ignored**。
4. **独立复核的结论（NOT-VERIFIED）**：复核者独立执行当前 WASM 集成文件 **2 pass / 0 fail**，但实施者引用的三次构建与旧实现红灯日志（`/tmp/a3-wasm-closure/`）**已全部删除**，无法独立确认旧实现红灯真实执行；本次不得将静态分析或 native 测试替代 WASM 红灯（`S13-verification-behavior.md` §6）。
5. **裁断**：**保留修复**（两方复核一致建议保留；回退会在真实 WASM 运行时 abort）。状态记为 **PARTIAL**：当前 runtime 通过为实测，历史红→绿链为不可独立复核的实施者陈述。复现建议：保留两版源码哈希、两版构建日志、两版运行原始输出及退出码。

### 5.18 两项待裁决的对抗裁决（第四版新增，2026-10-14）

四个只读对抗 agent（`plan`，两主题各一正一反，**不含写权限**；prompt 内钉死 worktree 路径、禁 git 写、禁 DDL、只读）分别产出立场文书，由本报告作者核对证据、补做注入实验后裁决。原始立场文书为过程产物（`.peri/plans/`，未入库），结论与证据落在这里。

#### 5.18.1 `peri.sessionWorkspaceV1` 门控：裁决「实现零改动 + 文档订正」

1. **形状**：5 处读取走**进程级** `SessionManager::effective_host_caps()`——`session_restore.rs:326/393/428`、`session_lifecycle.rs:570`、`storage_v2.rs:40`；未 initialize 时回落 `all_enabled`（`peri-acp/src/session/caps.rs:31-37`，本轮复核原文）。分页相反：`ConnectionFacts::history_pagination()` 按**当前连接**失败关闭（`host/connection.rs:69-77`，「调用约定」段落已写明不得改用进程级）。
2. **两方独立推翻的前提**：侦察阶段简报称「TUI 不 initialize ⇒ 改连接级会丢 TUI 的 workspace 字段」。正方与反方各自查到 **TUI 确实发 initialize**：`peri-tui/src/kit/entry.rs:417` 无条件 await `register_ui_commands`，该方法发 `initialize`（`acp_client/client/requests.rs:78-89`，基座 `all_enabled`、`peri.oauth=false`），host 侧落 `set_pending_caps` + `commit_initialize`。⇒ 该成本论据**不成立**，「改连接级会破坏 TUI」不能作为维持现状的理由。
3. **可达性（两方一致的硬事实）**：跨连接污染需要「同进程两个连接共享一个 `SessionManager`」。全仓无此调用点：每 host 一个 `ConnectionContext`（`host/mod.rs:381`），四种部署形态（TUI/print/stdio/wasm）各自一进程一连接。正方**自认未能证明可达**（「目前只有夹具可达」）；唯一不需要多连接就能触达的偏差是「外部 stdio 客户端跳过 initialize ⇒ 拿到全能力兜底」，而该形状是本交付**已声明边界**（A25 保留③、`caps.rs:29-31` 文档）。
4. **改与不改的收益差**：改读连接级 `ConnectionFacts::caps()`（保留兜底）对「未 initialize 的连接」**零行为差别**（两处同样回落 `all_enabled`，`host/connection.rs:54-58`），只把 6 个装配点改成传 facts；改成失败关闭则要动 ≥12 处夹具（`requests_workspace_cases_test.rs`/`requests_workspace_storage_v2_test.rs` 等），并把同族进程级门控（`mcp_oauth.rs:64/125/162/211` 等）拆成三种判据——一致性债换一个不可达形状的收益。
5. **裁决**：**实现零改动**。本轮只做两处文档订正：① `docs/design/peri-acp-protocol.md` 工作区扩展段写明门控事实源是进程级协商值、与分页按连接判定的差异及理由；② 修正 `peri-tui/src/acp_client/client/requests.rs` 的「TUI 无协议 initialize 握手」过期注释（该注释正是简报误判的来源）。
6. **再开启条件（写进蓝图 D17.2）**：出现同进程多连接的生产形态；或外部客户端跳过 initialize 被证实真实消费工作区扩展；或 TUI 的 initialize 常态失败；或需求方明确要求按连接判定——届时与 `oauth` 家族一起改，不做单点差异。

#### 5.18.2 锁守卫面：采纳独立断言下界，拒绝结构性闭合，保留调用点级局限（仍 PARTIAL）

1. **本轮新增注入实验（本机实测）**：把 `LOCK_RELEASE_PROBE_BUDGET` 从 250 ms 调到 **20 ms**，回归用例 `release_probe_waits_out_an_external_holder` **仍然绿**——判别断言 `waited + SLACK >= BUDGET` 的参照值与阈值同源，改小常量即让断言恒真，判别力被**静默取消**。这是正方主张中唯一可复现的一条，反方未覆盖。
2. **修复（测试内 8 行）**：判别断言改用独立下界 `PROBE_WAIT_FLOOR = 200ms`（不引用被测预算）。同一注入实验复跑 → **红**（`release probe gave up before its budget …: waited 22.289458ms < 200ms (budget 20ms)`）；把预算复原为 250 ms → **绿**（0.32 s）。原判别力不受影响（守卫入口退回零等待时 S13 实测 `0.484 ms`，`0.484 + 50 < 200` 必红，与修复前同向）。
3. **拒绝结构性闭合（正方方案 A/C）**：真实代价 = 移动 production 模块私有原语（`connection.rs:259` 无 `pub`，测试是它的子模块）+ 18 处机械改写；收益仅把绕过从「改一个词」抬成「改结构 + diff 可见」（正方自陈）。反方指出残余不可消除：新增入口、`Duration::from_nanos(1)`、`std::fs` 原生 API（同文件 `:476` 已在用）。且同文件内**合法的零等待**（`:442/:456/:632/:646` 等负向断言）与绕过写法逐字同形 ⇒ 文本守卫必须带行号白名单，白名单本身是新的豁免通道；本仓无源码扫描式守卫先例（`peri-acp/src/event/variant_coverage_test.rs` 是正向存在性检查，不可类推）。
4. **危害评估采纳反方**：绕过方向是「更敏感」而非「更沉默」——零等待会把 fork 继承窗口报成假红（S12 实测 20 轮 16 红），而真实泄漏在零等待下同样必红（探测拿不到锁即报 `busy`）。因此调用点级缺口的影响是 **flaky/噪声（Cry Wolf）**，不是漏检。
5. **未满足的判据如实保留**：独立复核的硬判据「真实四调用点回退可被检出」**仍未满足**（本轮未改四处调用点写法）。裁决按「测试卫生纪律、非生产风险」降级处理，代价与理由见上；`probe_initialization_lock_release`（`connection_open_test.rs:16-23`）入口注释已写明「调用点自己内联零等待探测不在本守卫覆盖内」。
6. **结论**：本条维持 **PARTIAL**（确定性握手 + 唯一预算来源受保护 + 判别断言与被测常量解耦；调用点级写法缺口为已声明局限）。修复后的新断言同时是策略断言：预算低于约 150 ms 会让该用例变红，需人来裁决，而不是让守卫悄悄失效。

#### 5.18.3 A05 索引：用户授权后实施并复跑（A05 由 FAIL 改判达成）

1. **授权**：用户显式授权 A05 所需的新增索引（本条是本次唯一获授权的 DDL；其余 DDL 仍未授权）。
2. **实施（canonical 单一来源）**：`peri-resources/src/sessions/canonical.rs::CREATE_INDEXES` 追加
   `CREATE INDEX IF NOT EXISTS idx_messages_active ON messages(thread_id, excluded)`；本机
   `sqlite_store/schema.rs` 的 **Current 分支**补一次幂等索引重放（形状判定只覆盖表与列，
   不补则上一版构建建出的库永远拿不到新索引）；新建、v11 迁移终点、远端初始化与远端升级批次
   都经同一份清单，未各自改动。新用例 `current_shape_open_backfills_a_missing_canonical_index`
   （删掉索引 → 重开 → 补回 + 版本不变，`peri-resources` lib **500 passed / 0 failed / 26 ignored**，
   本次实跑）。
3. **复跑（默认口径 10k/100k/1M，iters=20/pages=20）**：`.recon/a05-raw/perf-rerun.out`
   （`5 passed; 0 failed`，104.74 s）；恢复 cold p50 **3.299 / 3.379 / 2.435 ms**、hot p50
   2.322 / 2.092 / 2.105 ms，跨规模持平；对照授权前 11.109 / 98.182 / 1 081.927 ms。
   计划证据：`load_active_payloads_on`、`load_active_flags_on`、`read_active_entry_positions`
   三段都改为 `SEARCH messages USING INDEX idx_messages_active (thread_id=? AND excluded=?)`。
4. **代价与边界**：索引占用 ≈**25 B/行**（10k/100k/1M 三点一致：+249 856 / +2 498 560 /
   +25 280 512 B，1M 档约占库体 1.7%）；`idx_messages_thread_id` **保留**（只按线程取全部行的路径
   仍用，如分页阶段一与 `read_history_page_bounds`）；升级路径**首次补建**耗时未测量（已列入
   `perf-evidence.md` §4.2 未测量项）。
5. **判定**：A05「恢复工作量不随归档条数线性增长」**达成**（原 FAIL 撤销）。§2 的 A05 行与
   §0.1 计数已同步。

---

## 6. 交付面清单

### 6.1 改了哪些 crate / 包（核心三 commit `95c1e07d7` / `9b96f456a` / `3ccf75864`：144 files / +9 289 / −860；另有 S12~S13 轮的 16 个已跟踪修改文件 + 26 个新增源码/测试文件，由第四次提交一并落地）

| 层 | 范围 | 关键新增/改动 |
| --- | --- | --- |
| `peri-acp-types` | 契约与类型 | **新增** `src/history_page.rs`（游标/页/段类型 + 两个后端共用的唯一一份页与段规则）；`session_resources.rs`（`load_active_snapshot`/`load_message_flags`/`load_history_page`/`load_history_segment`/`load_entry_positions`/`rewind_history(clear_excluded)`/`RewindOutcome`）；`peri_caps.rs`（`peri.historyPagination`）；`store/history.rs`（`ActiveSessionSnapshot`、`ForkedHistory::active_payloads`） |
| `peri-agent` | 执行核心 | Full 提交成功后 `shrink_after_full_commit`（条目/索引/flags/容量一起释放）；Compact 进度改 `full_watermark: Option<MessageId>`；delivery 三态幂等按持久事实判定；手动 Compact 改有效集合对称身份校验；subagent 继承区 flags 走 `load_message_flags` |
| `peri-acp` | 出口协议 | **新增** `host/history_page.rs`（模式分流 + 参数规则 + 页响应投影）；`dispatch/session_load.rs`（`initial_history_cursor`、游标错误映射）；`dispatch/session_replay.rs`（条目身份/位置戳、未协商流式 canonical 回放、回放失败使 load 失败）；`session_replay.rs` 的 `REPLAY_SEGMENT_ROWS/BYTE_BUDGET`；rewind 三条路径改以存储为事实源 + `structure_version` 递增 |
| `peri-resources` | 存储适配 | SQLite 与 Remote 两后端：有效范围读取（`excluded = 0` 在 SQL 侧）、`SUM(LENGTH(content))` 聚合移除、分页两阶段读取、段读取（向前 keyset）、rewind 单事务/单批原子入口；**新增** 5 个测试文件（第一版的未跟踪审计探针 `tests/zz_audit_history_anchor_probe.rs` 已随 F1 修复删除，见 §0.3 计数说明） |
| `peri-tui` | 前端 | **新增** `kit/history_paging.rs`、`kit/history_page_consumer.rs`、`acp_client/client/history_page.rs`、`kit/acp_events/history_page.rs`、`kit/message_area/viewport_anchor.rs`；取消 Compact 强制 reload；区间合并/双向工具补全/锚点重定位 |
| `npm-packages/@peri-sdk` | 前端 SDK | **新增** `src/state/history-merge.ts`；`agent/session.ts`（能力声明、`loadEarlierHistory`、按 `loadGeneration` 暂存与一次安装）、`state/session-docs.ts`（Compact/rewind 分流）、`view/session-view.ts`；**新增** 2 个测试文件 |
| `langfuse-client`（附带修复） | 测试基线 | `src/batcher/recovery.rs` 无线程目标（WASM）就地编码——修基线 `7f0de56e8` 引入的 telemetry panic；`npm-packages/@peri-sdk/tests/wasm-acp-integration.test.ts` 夹具按 gzip magic 解压（口径与产品契约一致，见 §5.17） |
| `docs` | 文档 | `docs/design/peri-acp-protocol.md`（分页契约 + `loadGeneration` + 条目身份）、6 个 `docs/code-index/*`、`docs/verification/tui-integration-cases/06-transcript-ui.md` 与 `README.md`（P0-81） |

> S13 规模治理（§5.15）在 `peri-acp`（2 个测试文件 → 2 父 + 8 子模块）、`peri-acp-types`（1 文件 → 父 990 + `session_resources/history.rs`）、`peri-agent`（2 文件 → 2 父 + 5 文件）、`peri-tui`/`peri-resources`（4 文件 → 11 文件）按职责拆分；公开路径与断言零改动，产出的 26 个新文件即 §0.1b 第 3 条所列“新增源码/测试文件”。

### 6.2 对外行为变化

1. **新能力名**：`peri.historyPagination`（`initialize` 的 `clientCapabilities._meta`，默认 **false**）。不带该键的连接**不获得也不会看到**分页私有语义。
2. **`session/load` 新增 cursor 模式**（仅协商连接）：请求 `{ sessionId, cursor, limit? }`；响应 `{ sessionId, snapshotToken, nextCursor, items[] }`；`items[].entryId = "{threadId}:{messageId}"`、`items[].position`（存储序不透明整数）、`items[].oversize`、`items[].subItems[].position`。历史页不装配、不广播、不取执行锁。
3. **初始 load 的响应/通知新增字段**（协商连接）：响应可含 `nextCursor`/`snapshotToken`；`loadGeneration` 可由客户端传入并以 `_meta.periLoadGeneration` 回显（门控 `caps.replay`）；回放通知带 `_meta.periEntryId` / `_meta.periPosition`（门控分页能力）。
4. **未协商连接**：语义**不变**（标准完整回放），但投递方式改为**逐段流式**（canonical 完整、含归档原文）；**回放投递失败现在使 load 失败**（`-32603`，此前是 warn+继续）。
5. **新错误码**：`-32011`（跨会话 cursor）、`-32012`（cursor 失效/结构版本过期）、`-32013`（rewind 边界目标缺失）；`-32601`/`-32602` 复用既有段。
6. **rewind 语义收敛**：存储侧「删边界行 + 清 `excluded`」在同一事务/同一批内完成，并如实回报 `Applied` / `BoundaryMissing`（未生效即失败关闭，不发布、不推进版本）；成功时 `structure_version` 恰好 +1（内存计数，无 DDL）。
7. **行为降级**：`ThreadMeta.content_size` 恒为 0（无生产消费者）。
8. **TUI**：手动 Compact 不再强制完整 reload；滚到已载入窗口顶端加载更早页（含迟到页丢弃、锚点补偿）。
9. **SDK 公开面新增**：`canLoadEarlierHistory` / `loadEarlierHistory(limit?)`、`HistoryPageUnavailableError`（reason：`not_negotiated` / `exhausted` / `invalidated`）、`StaleHistoryPageError`、`AWAITING_RESULT_STATUS`、`COMPACTION_STATUS`、`EntryView.historyOversize`；初始回放改为按 `loadGeneration` 暂存、响应后一次安装（`stream()` 仍逐条投递首屏）。

### 6.3 明确不变的部分

1. **无新 JSON-RPC 方法**：`session/load` 仍是唯一主入口（带 cursor 只是同一方法的新模式）；diff 未新增 `session/*` 方法名（实测：`git diff | grep -o '"session/[a-z_/-]*"'` 只列出既有 10 个）。
2. **无 DDL / 无 schema 改动 / 无迁移 / 无双写 / 无兜底**：`git diff` 中 `ALTER TABLE`/`CREATE TABLE`/`CREATE INDEX`/`DROP TABLE` 计数 **0**；**生产代码与 schema 无 DDL/迁移/TTL/GC/双写实现**——新增行里仅两处文档命中：`双写`（`docs/code-index/peri-agent.md`，否定语境「不走 writer 双写」）、`user_version`（`docs/code-index/peri-resources.md`，说明「服务端拒绝写 `PRAGMA user_version`」），均为否定性表述，不计入（实测）。本轮的 F1 修复同样未触碰 schema。
3. **无新依赖**：`Cargo.toml` / `package.json` 零改动（实测）。
4. **消息类型、持久化 payload 编码、Compact 摘要语义、Micro 投影、预算算法**不变（复用现有消息内容与 ViewModel）。
5. **Yjs 文档结构（chat schema 2 / session schema 1）与前端架构**不变（无分文档、无轮换、无虚拟化改造）。
6. **既有错误码与既有 `session/*` 语义**不变（除上述新增码与 rewind 的如实回报）。

### 6.4 交付前的格式化归零（第二轮补做）

1. **现象**：`cargo fmt --all -- --check` 在交付集上报 **42 个文件**不符合 rustfmt（rustfmt 1.10.0-stable / cargo 1.99.0）。
2. **归属判定**：这 42 个文件**全部**落在本次改动的文件集合内（`comm` 比对：改动集合之外 **0** 命中）；对照主仓库（`7175eaaf7`，本分支基线 `5be107603` 的后代）跑同一条命令为 **0 diff** ⇒ 偏差由本次改动的编辑引入，不是基线问题，也与工具链/配置无关（仓库无 `rustfmt.toml`、无 `rust-toolchain.toml`）。此前所有轮次只跑过 `check` / `test`，没有跑过 fmt，故此偏差未被发现。
3. **处置**：在 worktree 内执行 `cargo fmt --all`。改动前后逐行比对 `git status`：**仅** `peri-tui/src/kit/acp_bridge_cancel_test.rs` 从「已修改」回到与 HEAD 完全一致（它此前的差异**只是排版**，被格式化还原 ⇒ 该文件最终不交付差异）；**没有**新增任何被改动的文件，`git diff --stat` 由 145 files / +9 219 / −853 变为 **144 files / +9 289 / −860**（同一批文件内的换行重排）。
4. **复验（全部本轮重跑）**：`fmt --all -- --check` = **0 diff**；`check --locked --all-targets`（5 crate）**exit 0**；`peri-acp --lib` **887/0**、`peri-tui --lib` **1817/0/6 ignored**、`peri-resources --lib` **497/0/26 ignored**。格式化不改变语义（`acp_bridge_cancel_test.rs` 还原为 HEAD 版后 TUI 全量仍 1817/0）。
5. **意义**：`lefthook.yml` 的 pre-commit 第一步是 `cargo fmt --check`（`fix: cargo fmt`）；补做之前该钩子必然红，现在通过。
6. **第三轮（S13 收尾，第四次提交前由 pre-commit 拦下）**：S13 拆分后的两个文件（`history_page_wire_test.rs`、`requests_rewind_failure_paths_test.rs`）在块搬移处各留下 1~2 个多余空行；`lefthook` pre-commit 的 `cargo fmt --check` 判红（各拆分报告声称的 crate 级 fmt 通过未覆盖「多文件搬移产生的空行」这一形态）。已 `fmt --all` 归零，随后同一钩子四步全绿（`check` / `clippy` / `fmt` / `layer-imports`），第四次提交落地为 **`c1744d472`**（146 files / +31 046 / −7 166）。
7. **第四轮（A05 索引与对抗裁决，第六次提交 `5e691a2ac` 前）**：`fmt --check` 天然通过（改动含 `fmt --all` 归零后的既有形态）；pre-commit 四步全绿（`check` 59.99 s / `clippy` 31.15 s / `fmt` 5.40 s / `layer-imports` 1.40 s），clippy 只有两条**既有** warning（`peri-acp/src/dispatch/session_replay.rs:55` 的 `large_enum_variant`、`peri-tui/src/kit/acp_events/history_page.rs:41` 的 `type_complexity`），非本轮引入、不影响门禁结论。
8. **第五轮（D8 闭合，第八次提交前）**：`lefthook run pre-commit` 第一次判红——**只有 `fmt` 一步**（实施 agent 写入的 1 处 `assert!` 排版不符 rustfmt；`check` 109.01 s / `clippy` 96.33 s / `layer-imports` 1.28 s 三步已绿）。`fmt --all` 归零后 `fmt --all -- --check` 无输出（exit 0），改动范围仍为同一批 31 个文件（无新增/回退文件），随后复跑清零面 `-p peri-resources --lib -- rewind` **17 passed / 0 failed** 确认格式化未改语义；`check` / `clippy` 两步在同一次钩子内已通过（clippy 仍只有上述两条既有 warning）。

---

## 7. 需求 §10「不在本任务内」的显式声明（逐条确认未被引入）

| §10 条目 | 本任务状态 | 依据 |
| --- | --- | --- |
| 旧格式兼容、迁移、双写和旧数据自动修复 | **未引入** | 复用既有表与列；`git diff` 无迁移代码/无双写路径；读取仍带 `excluded = 0` 复用既有 flags（`final-adversarial-audit.md` 必查 1 实测） |
| 从未 Compact 的超长历史的分块摘要或兜底恢复 | **未引入** | 无新增「未压缩历史」代码路径；初始 load 不触发摘要（`history_page.rs` 契约：`nextCursor = null` 不编造游标、不触发兜底摘要） |
| 改写摘要算法、模型预算、Micro Compact 算法 | **未引入** | `micro.rs` / `planner.rs` / 摘要生成未改语义；本任务只改**读取范围、运行时持有范围与 ACP 分页契约**（需求 §1 已确定边界） |
| 物理删除历史原文、归档存储分层、TTL、GC | **未引入** | 收缩只释放**热态**持有；归档原文仍在存储中可按 cursor 回看（`perf-evidence.md` §5.1 探针、`session_history_page_test.rs` 归档可读断言）。rewind 的尾部切除是**既有**语义，非本任务新增的删除通道；无 TTL/GC/分层代码 |
| 新建完整 Checkpoint / event-sourcing / 持久执行恢复系统 | **未引入** | 无新表/新列；`structure_version` 为**内存计数**（`host/mod.rs:156`，无存储列）；分页游标是范围 token + 锚点，不是 Checkpoint 数据模型 |
| Yjs 架构替换、文档轮换、完整前端内存治理 | **未引入** | SDK 复用 `SessionDocs` 与既有 ViewModel；无分文档/轮换/虚拟化改造；且**明确不宣称**分页保证前端累计内存恒定（§3.2） |
| 将主流程替换为必须使用 `session/resume` 与独立历史 RPC | **未引入** | `session/load` 仍是唯一主入口（`S6-verification.md` 不变量 1 实测：无新方法名、模式在生命周期锁之前分流） |

不含在本任务内的另外两条需求原文边界，一并确认：**不兼容旧存储格式**（无兼容层）；**不要求客户端改为 resume + 另一套历史接口**。

---

## 8. 测试矩阵、复现命令与证据索引

### 8.1 终局测试矩阵（`final-test-matrix.md`，本次未复跑）

| 层 | 命令 | 结果 |
| --- | --- | --- |
| `check --all-targets` | 5 个 crate（串行） | 全部 exit 0；本仓库代码 0 warning |
| `test --lib` | `peri-acp-types` / `peri-acp` / `peri-agent` / `peri-resources` / `peri-tui` | 576 / 884 / 1089 / 497（26 ignored）/ 1817（6 ignored） = **4 863 passed / 0 failed**（`peri-acp` 本轮 F1 修复后为 **886**：−1 审计探针 +2 回归用例；`peri-resources` 本轮复跑仍为 **497 / 0 failed / 26 ignored**） |
| 集成目标（`tests/`，附加层） | 19 个目标 | **117 passed / 0 failed**（第一版的 F1 存储层探针目标已删除） |
| SDK | `bun run typecheck && bun run build && bun test` | typecheck、build PASS；`bun test` **252 pass / 2 fail**（**2 条均为 `sqld` 环境失败**：`mise` 未信任 worktree 的 `mise.toml`，`error: sqld exited (1)`）；**解除环境阻塞后 253 pass / 1 fail**（剩下的 1 条为基线 WASM telemetry 导出 panic，见 §5.13 第 8 条） |

与已知基线对比：`peri-acp-types` +8、`peri-acp` +21、`peri-agent` 0、`peri-resources` +13~16、`peri-tui` +10，**无失败、无静默跳过信号**。

### 8.2 复现命令（照抄各报告；作者本次实际跑过 #1~#5）

```bash
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history

# 0) F1 修复后的回归面 —— 本轮复跑（第一版的探针命令已随探针删除，见 §0.3）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_page
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib

# 1) A19 / rewind 面 —— 本次复跑 69 passed
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- rewind

# 2) 性能与计划证据（三规模默认 10k/100k/1M，约 36 分钟夹具；本次只跑 200 行档）
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- history_perf_tests --ignored --nocapture
PERI_PERF_ROWS=200 PERI_PERF_ITERS=3 PERI_PERF_PAGES=3 \
  ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib \
  -- history_perf_restore_and_paging_evidence --ignored --nocapture

# 3) 段读取（D7）有界性审计用例
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib \
  -- audit_segment_lookahead_and_plan_shape --ignored --nocapture

# 4) 未协商回放与分页 wire 面
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- unnegotiated_replay --test-threads=1
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_page

# 5) SDK 分页与合并（本次复跑 25 pass / 0 fail）
cd npm-packages/@peri-sdk && bun run typecheck && bun run build && bun test
```

D8 闭合（第五版）的复现面（过滤词按实际测试名匹配）：

```bash
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history

./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib -- rewind_between_two_fulls
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib -- rewind_judges_recorded_and_unrecorded
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib -- fork_remap_rewrites_superseded_by
./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- compact_v2::full
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- rewind
```

### 8.3 证据索引

| 主题 | 文件 |
| --- | --- |
| 性能与计划（A04/A05/A06/A07） | `.recon/perf-evidence.md`（§1 口径、§3 计划、§4/§4.1 三规模、§4.2 索引在位复跑、§5 补充用例、§7 未测量、§8/§8.1 触达行数与其闭合）；复跑原始输出 `.recon/a05-raw/perf-rerun.out` |
| 段读取有界性与 R1~R11 | `.recon/S9-boundedness.md`（+ `.recon/S9-boundedness-raw.log`） |
| 标准客户端完整回放（D7）复核 | `.recon/S9-verification.md`、`.recon/S9a-storage-contract.md`、`.recon/S9b-acp-streaming-replay.md` |
| 回放标记门控（D12） | `.recon/S9c-caps-gate-fix.md`、`.recon/S9c-verification.md` |
| ACP 分页出口（S6） | `.recon/S6-verification.md`、`.recon/S6-design.md` |
| 契约/收缩/rewind 存储层 | `.recon/R4-verification.md`、`.recon/rewind-review-findings.md`（F7 已闭合）、`.recon/S3-review-findings.md` |
| **D8 闭合（第五版）** | 本报告 §5.1（裁决、实现四面、证据、残留）；用例：`peri-acp-types/src/store/history_test.rs`（批次边界 / 摘要被删 / 新旧混用 / fork 重映射）、`peri-agent/src/agent/compact_v2/full_test.rs`、`peri-resources/src/sessions/{sqlite_store/session_history_test.rs,remote/session_history_rewind_test.rs}` |
| R1–R3 回归复核 | `.recon/R4b-verification.md` |
| 前端 TUI | `.recon/S7-tui-verification.md`、`.recon/S7c-tui-verification.md`、`.recon/S7d-verification.md`、`.recon/S7e-verification.md`、`.recon/S7e-anchor-relocation.md` |
| 前端 SDK | `.recon/S7-sdk-verification.md`、`.recon/S7c-sdk-fix.md`、`.recon/S7c-sdk-verification.md` |
| 终局矩阵 | `.recon/final-test-matrix.md` + `.recon/final-matrix-logs/`（34 个原始日志） |
| 对抗审计（F1/F2、A01~A25 状态） | `.recon/final-adversarial-audit.md`、`.recon/zz-probe-subagent-failure-output.txt` |
| **F1 修复的独立复核（本轮）** | `.recon/S11-verification.md`（+ 其引用的 `.recon/S11-verify-*.txt` / `*.rs.txt` 探针留档） |
| S12 收尾轮（拒绝持久化、锁初查、基线 WASM 修复、A05 侦察、冒烟） | `.recon/S12-*.md`（8 份；含独立复核 `S12-verification.md`，判 NEEDS-REVISION） |
| S13 修订轮（wire 拆分与拒绝持久用例、锁回归重做、规模治理、WASM 闭环） | `.recon/S13-*.md`（9 份）+ 独立复核 `S13-verification-size.md`、`S13-verification-behavior.md` |
| 编排脚本与原始留档（S12/S13） | `.recon/workflows/s12-remaining-items.mjs`、`.recon/workflows/s13-revision-round.mjs`；`.recon/{s13-raw,s13-acp-tests-raw,s13-wire-split-raw,s13-size-agent-raw,s13-size-tui-resources-raw,a1-raw,a2-raw,b1-raw,b2-raw}/` |
| 报告自身的复核（第一版） | `.recon/A-acceptance-report-audit.md`（M1~M4 必须修订项、S1~S5 建议项；已按其实施，见 §0.1 修订记录） |

---

## 9. 报告复核提示（给评审者）

1. 本报告的最低两条标准：**没证据不写 PASS**（A05 在索引复跑证据到位后由 FAIL 改判 PASS；17 条 PARTIAL 均写明哪一半成立；未跑的直接写未验证；A14 由「未验证」降为 PARTIAL 的依据与覆盖边界见 §5.14）；**两类成本不混写**（§3）。
2. 若只看三行：**F1 已修复并独立复核（§5.9）**、**A05 已由用户授权索引闭合达成（§5.18.3）**、**D8 已闭合且无需 DDL（§5.1，取代关系落在既有 `projection` 载荷）**；另有两条红线：**累计缓存零测量（不得宣称恒定）**、**未新增任何列/表（schema 代数未推进）**。
3. 报告本身不新增任何毫秒承诺；所有毫秒数都带 `.recon/perf-evidence.md` §1 的 debug/冷热口径。
