# S13 · peri-agent transcript 生产 / 测试模块规模拆分（任务 B3）

工作树（git worktree）：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
分支 `feat/transcript-shrink-history-pagination`，HEAD `3ccf75864`。
（每条命令均以该工作树为 cwd；Rust 构建/测试一律用 `./scripts/cargo-rmcp-patched.sh`。）

---

## 1. 结论

1. **拆分完成**：`transcript.rs`（1064→**805**）与 `transcript_test.rs`（1101→**33**）按职责拆为 5 个新文件，**7 个文件全部 ≤1000 行**（STD-SIZE-001）。
2. **职责边界**：生产按「条目/暂存模型」「持久化前台接线」切分；测试按「持久化行为」「内存模型」「reminder 投递矩阵」切分，父模块只留挂载点与共享夹具。
3. **语义零改动**：15 个搬移块逐块字节比对，14 块**完全一致**，唯一差异块是 4 处私有方法标注 `pub(super)`（见 3.2，生效可见范围与拆分前相同）。
4. **公开路径保持**：`pub use entry::{StagedData, TranscriptEntry};` 维持 `session::transcript::{StagedData, TranscriptEntry}` 可用；`MessageTranscript` 的固有方法路径不受 impl 所在模块影响。
5. **挂载真实成立**：三个子模块过滤运行分别 **12 / 30 / 2 passed**（`0 filtered out` 不成立，均非空跑），父过滤 **47 passed / 1042 filtered out**，与拆分前逐字相同。
6. **全量绿且数量一致**：`-p peri-agent --lib` 拆分前 `1089 passed; 0 failed; 0 ignored; 0 filtered out` → 拆分后**同数字**，`EXIT=0`。
7. **门禁干净**：`cargo fmt --check -p peri-agent` → 0 diff；`clippy -p peri-agent -- -W clippy::all` → 0 warning / `EXIT=0`；编译 0 warning。
8. **未触碰范围外文件**：`git status --short -- peri-agent/` 仅 7 个文件（2 改 + 5 新增）。存量超限文件、文档索引未动（见 §8）。

---

## 2. 改动清单与拆分映射

### 2.1 生产模块

| 文件 | 行数 | 职责 | 来自原 `transcript.rs` |
| --- | --- | --- | --- |
| `session/transcript.rs`（父） | 805 | 模块文档、共享类型（`DEFAULT_PENDING_*`、`CompactionCommitState`、`PersistOp`、`MessageTranscript` 定义）、内存模型 impl（构造/查询/追加/staging/标记/压缩生命周期/rebuild/rewind）、子模块挂载 | 1-30、31-63、160-194、197-325、383-942、1056-1064 |
| `session/transcript/entry.rs` | 107 | 条目与暂存模型：`TranscriptEntry`（id / `as_message` / `visible_to` / `into_payload` / `project_message`）、`StagedData` | 64-158 |
| `session/transcript/persistence_port.rs` | 191 | `MessageTranscript` 的持久化前台接线：绑定后端与预算、sticky 失败查询、flush / tx 句柄、额度预留与投递、`shutdown_persistence`、`Drop` 排空 | 327-382、944-1047、1050-1054 |

（`session/transcript/persistence.rs`＝后台 writer，本任务未改，sha256 不变。）

### 2.2 测试模块

| 文件 | 行数 | 职责 | 来自原 `transcript_test.rs` |
| --- | --- | --- | --- |
| `session/transcript_test.rs`（父） | 33 | 挂载点 + 共享夹具 `make_human/make_ai/make_tool_result` | 1、41-54 |
| `session/transcript_persistence_test.rs` | 436 | 异步持久化行为（真实 SQLite 夹具 `TestSession` + 内存替身 `MockSessionResources`；sticky 失败、预算有界、compact 生命周期落盘、shutdown/Drop 排空） | 56-479 |
| `session/transcript_memory_test.rs` | 563 | 内存模型（构造/寻址、Staging 两阶段写入、标记与投影、祖先只读边界、rewind、rebuild、`set_flags_batch`） | 3-31、481-1000 |
| `session/transcript_reminder_test.rs` | 105 | canonical reminder 受众投递矩阵（H8） | 1002-1101 |

`session/transcript_projection_test.rs`（`mod projection_tests`，3 个用例）本任务未改。

### 2.3 挂载方式

1. 生产子模块沿用本文件既有先例：`mod entry;` / `mod persistence;` / `mod persistence_port;` + `pub use entry::{StagedData, TranscriptEntry};`（`persistence.rs` 原本就是 `mod persistence;` 的子模块）。
2. 测试子模块沿用仓库先例 `#[path = "..."] mod x_tests;`（同 S13 wire 拆分）：父文件是 `#[path = "transcript_test.rs"] mod tests;` 的目标，子文件再由父文件用 `#[path]` 挂载，子模块经 `use super::*;` 复用父模块夹具与 import。
3. 测试模块路径随之变化（**有意的可见后果**）：`session::transcript::tests::test_x` → `session::transcript::tests::{persistence_tests|memory_tests|reminder_tests}::test_x`。

---

## 3. 语义零改动证据

### 3.1 逐块字节比对（脚本 `.recon/s13-size-agent-raw/verify_moves.py`）

```
$ python3 .recon/s13-size-agent-raw/verify_moves.py
块                                                原文区间           目标起始行      差异行
--------------------------------------------------------------------------------------------
transcript.rs 1-9 模块文档                           1-9            1          0        OK
transcript.rs 31-63 常量+CompactionCommitState     31-63          32         0        OK
transcript.rs 160-194 PersistOp                  160-194        65         0        OK
transcript.rs 197-325 结构+构造                      197-325        106        0        OK
transcript.rs 383-942 查询→rewind                  383-942        235        0        OK
transcript.rs 1056-1064 tests 挂载                 1056-1064      797        0        OK
transcript.rs 64-158 TranscriptEntry+StagedData  64-158         13         0        OK
transcript.rs 327-382 持久化绑定与预算                   327-382        24         0        OK
transcript.rs 944-1047 内部辅助（flush/预留/关闭）         944-1047       81         8        OK(声明差异)
transcript.rs 1050-1054 Drop impl                1050-1054      187        0        OK
transcript_test.rs 41-54 共享夹具                    41-54          11         0        OK
transcript_test.rs 56-479 持久化用例                  56-479         13         0        OK
transcript_test.rs 3-31 祖先 flag 用例               3-31           14         0        OK
transcript_test.rs 481-1000 内存模型用例               481-1000       44         0        OK
transcript_test.rs 1002-1101 reminder 用例         1002-1101      6          0        OK
--------------------------------------------------------------------------------------------
全部块字节一致
```

脚本对每个块用「块内首个唯一非空行」在新文件中定位，再按原区间长度逐行 `difflib` 比对；`OK(声明差异)` 块的 8 行差异就是 §3.2 的 4 处 `pub(super)`（断言要求差异**恰好等于**这 8 行，多一行即失败）。

### 3.2 唯一非搬移改动：4 处 `pub(super)`

搬移后这 4 个私有方法仍被**父模块**（`append`/`commit_staged`/`set_truncated`/`rewind_to` 等）调用，必须对 `transcript` 可见：

```
-    fn reserve_persistence(&self, reservation: Reservation) -> Option<Reservation> {
+    pub(super) fn reserve_persistence(&self, reservation: Reservation) -> Option<Reservation> {
-    fn persist_appended_entry(&self, entry: TranscriptEntry) {
+    pub(super) fn persist_appended_entry(&self, entry: TranscriptEntry) {
-    fn persist_flags(&self, id: MessageId, flags: MessageFlags) {
+    pub(super) fn persist_flags(&self, id: MessageId, flags: MessageFlags) {
-    fn send_persist(&self, op: PersistOp) {
+    pub(super) fn send_persist(&self, op: PersistOp) {
```

生效可见范围等价（静态推理，未做编译期反向探针）：
拆分前 `fn`（私有于模块 `transcript`，可见于 `transcript` 及其全部后代）
＝拆分后 `pub(super)`（在 `transcript::persistence_port` 中，可见于 `transcript` 及其全部后代）。
另有 `bind_persistence` 保持私有（两个调用点 `with_persistence` / `with_persistence_budget` 同块搬移）。全量 lib 编译通过 ⇒ 无任何调用点因可见性变化失联。

### 3.3 父模块头部差异（import 按归属重新分配）

```
$ diff <(sed -n '1,30p' transcript.rs.orig) <(sed -n '1,31p' transcript.rs)
9a10,12
> //!
> //! 模块结构：条目与暂存模型见 `entry`，持久化接线（前台）见 `persistence_port`，
> //! 后台 writer 见 `persistence`；`MessageTranscript` 的内存模型、标记与压缩生命周期留在本模块。
20c23
< use crate::messages::{BaseMessage, MessageContent, MessageId};
---
> use crate::messages::{BaseMessage, MessageId};
26,28c29
< use peri_acp_types::system_reminder::{
<     encode_system_reminder, reminder_egress_allowed, ReminderAudience, TrustedSystemReminder,
< };
---
> use peri_acp_types::system_reminder::{ReminderAudience, TrustedSystemReminder};
```

说明：`MessageContent` / `encode_system_reminder` / `reminder_egress_allowed` 随 `TranscriptEntry` 迁到 `entry.rs`（全文件仅 137/142 行与 `visible_to` 使用）；`ReminderAudience` 仍被父模块 `visible_model_messages` / `visible_snapshot` 使用，故保留。迁移后两侧 import 均为「谁用谁 import」，编译 0 warning。

### 3.4 覆盖性（原行去向）

原 `transcript.rs` 1064 行 = 14 个搬移/保留块（全部经 §3.1 比对）+ 被重新挂载的分隔行：
`159`（StagedData 与 PersistOp 之间空行）、`195`（旧 `mod persistence;`，改为 3 个 `mod` + `pub use`）、`196/326/943/1049/1055`（块间空行）、`1048`（`impl MessageTranscript` 收尾大括号，改由父/子模块各自闭合）。
原 `transcript_test.rs` 1101 行 = 5 个块（其中 1、41-54 进父文件）+ 块间空行（含 `55`、`480`、`1001`）。空行与大括号的重排已由 `cargo fmt --check` 归零。原文件未丢失任何非空行：所有块差异行数均为 0（除 §3.2 的 4 行）。

---

## 4. 公开路径保持证据

1. `session/mod.rs:54` 的 `pub use transcript::{MessageTranscript, StagedData, TranscriptEntry};` 未改，且 `transcript.rs` 内 `pub use entry::{StagedData, TranscriptEntry};` 提供同一路径。
2. 外部使用点未改且编译通过：`agent/compact_v2/planner.rs:9`（`use crate::session::transcript::{MessageTranscript, TranscriptEntry};`）、`agent/stages/receive.rs:115`（`crate::session::TranscriptEntry::Reminder`）、`session/subagent/child_runner_test.rs:119`。
3. `PersistenceBudget` / `Reservation` 仍然只在 `transcript` 内部可见（`mod persistence;` 私有 + `use persistence::{...}` 私有导入未改），`grep -rn PersistenceBudget` 在 transcript 之外无命中。

---

## 5. 挂载真实成立（非空跑）证据

原始输出存 `.recon/s13-size-agent-raw/mount-proof.txt`（拆分后最终状态，逐条命令）：

| 过滤命令（`-p peri-agent --lib -- <filter>`） | running | 结果 |
| --- | --- | --- |
| `session::transcript::tests::persistence_tests::` | 12 | `12 passed; 0 failed; ...; 1077 filtered out` |
| `session::transcript::tests::memory_tests::` | 30 | `30 passed; 0 failed; ...; 1059 filtered out` |
| `session::transcript::tests::reminder_tests::` | 2 | `2 passed; 0 failed; ...; 1087 filtered out` |
| `session::transcript::tests::persistence_tests::test_drop_flushes_pending_appends_to_store` | 1 | `test ...test_drop_flushes_pending_appends_to_store ... ok`；`1 passed; ...; 1088 filtered out` |
| `session::transcript::`（全部） | 47 | `47 passed; 0 failed; 0 ignored; 0 measured; 1042 filtered out` |

12+30+2 = 44（三个子模块）+ 3（未改动的 `projection_tests`）= 47 = 拆分前 `session::transcript::` 的 47，逐项一致。

---

## 6. 全量 lib 测试（前后数字一致）

```
$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib     # 拆分前（.recon/s13-size-agent-raw/before-lib-test.txt）
test result: ok. 1089 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.91s
EXIT=0

$ ./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib     # 拆分后（after-lib-test.txt）
test result: ok. 1089 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.19s
EXIT=0
```

两次均 `0 failed / 0 ignored / 0 measured / 0 filtered out`，编译与测试输出 `warning` 计数 = 0。

---

## 7. 文件规模（STD-SIZE-001）

```
$ wc -l <7 个文件>                                   # .recon/s13-size-agent-raw/after-wc.txt
     805 peri-agent/src/session/transcript.rs
     107 peri-agent/src/session/transcript/entry.rs
     191 peri-agent/src/session/transcript/persistence_port.rs
      33 peri-agent/src/session/transcript_test.rs
     436 peri-agent/src/session/transcript_persistence_test.rs
     563 peri-agent/src/session/transcript_memory_test.rs
     105 peri-agent/src/session/transcript_reminder_test.rs
```

全仓扫描 `bash scripts/check-file-size.sh`：

| 时点 | 结果 |
| --- | --- |
| 拆分前 | 11 个超限（源码 4 / 测试 7），含 `transcript.rs` 1064、`transcript_test.rs` 1101 |
| 拆分后 | 6 个超限（源码 2 / 测试 4），**两个 transcript 文件已不在名单** |

拆分后仍在名单的 6 个（存量、非本任务范围，未改动）：`peri-tui/src/kit/panels/mcp.rs` 1637、`peri-tui/src/kit/acp_events/system.rs` 1013、`peri-resources/.../session_history_perf_test.rs` 1361、`.../schema_test.rs` 1038、`.../full_remote_test.rs` 1029、`peri-tui/src/kit/acp_events_test/session_events_test.rs` 1021。

---

## 8. 未验证项与局限

1. **未做变异/注入证伪**：本次是纯搬移，用「逐块字节比对 + 编译 + 全量测试数字一致」替代。若需要「拆分本身被证伪」的形态，可补一轮注入（例如把某个子模块的 `mod` 挂载注释掉，验证对应用例消失——本次未做，但 §5 的 12/30/2 已经证明挂载非空）。
2. **未跑其他 crate 的 lib 测试**：任务范围限 `peri-agent`；且工作树内 `peri-acp`、`peri-acp-types`、`peri-resources`、`peri-tui` 正由并行工作流改动（存在其未提交改动与新增文件），此时跑不能把结果归因到本次拆分。公开面未变已由「同 crate 全量编译 + §4 路径核查」覆盖。
3. **`pub(super)` 等价性是静态推理**：未写编译期反向探针（在 `transcript` 之外调用应失败）来证伪；方向性证据是全量编译通过（无调用点失联）。
4. **clippy 不覆盖 test target**：`cargo clippy` 默认目标与仓库 pre-commit 一致（lib/bin），测试代码只经 `cargo test` 编译（0 warning）。
5. **文档索引未同步（需后续任务处理，本次按「禁止改范围外文件」未动）**：
   - `docs/code-index/peri-agent.md:140` 仍列 `session/transcript.rs + transcript_test.rs + transcript_projection_test.rs`；
   - `docs/design/micro-compact.md:350` 仍把 `transcript_test.rs` 记为唯一测试文件。
   建议由文档同步项补入新增的 3 个测试文件与 2 个生产子模块（含新职责表述）。
6. **可复现脚本**：`.recon/s13-size-agent-raw/split.py`（幂等，输入为同目录 `.orig` 备份，**不可对已拆分树复跑**：父文件已变）、`verify_moves.py`、`mount-proof.sh`；证据目录 `.recon/s13-size-agent-raw/`（`before-lib-test.txt`、`after-lib-test.txt`、`after-transcript-filtered.txt`、`mount-proof.txt`、`verify-moves.txt`、`head-diff.txt`、`after-wc.txt`、`after-file-size-scan.txt`、`fmt-check.txt`、`clippy-peri-agent.txt`、`before/after-sha256.txt`）。

---

## 9. 改动文件 sha256（前 16 位）

| 文件 | 行数 | sha256 |
| --- | --- | --- |
| `session/transcript.rs` | 805 | `5c443a385fdb7ec2…` |
| `session/transcript/entry.rs` | 107 | `3bad9a135abc5404…` |
| `session/transcript/persistence_port.rs` | 191 | `f3d21ae4bb3fabb1…` |
| `session/transcript_test.rs` | 33 | `9b84627d4eb211a2…` |
| `session/transcript_persistence_test.rs` | 436 | `efcf41234be4bd79…` |
| `session/transcript_memory_test.rs` | 563 | `7115496ca4a38648…` |
| `session/transcript_reminder_test.rs` | 105 | `dc6ba924a1aba61a…` |

（完整值见 `.recon/s13-size-agent-raw/after-sha256.txt`。）

VERDICT: SIZE-SPLIT-DONE
