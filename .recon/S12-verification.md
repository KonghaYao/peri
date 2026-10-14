# S12 独立复核（任务 C）

## 1. 结论

**需修订，不批准当前 S12 为完整可交付。** 全量 native lib 测试通过，不等于修复证伪及产品验收全部通过。

1. **锁修复的测试证据不成立**：常量退回零预算后 3 次中仍有 1 次绿；把实际修复调用点退回后，新用例 3/3 绿。它证明非零等待有用，但没有可靠守住本轮修复调用点。250ms 预算仍会被 350ms 的合法外部持锁窗口击穿。
2. **A05 仍 FAIL**：没有 A05 实施文件或报告；独立 SQL 实验显示恢复工作量线性增长，rewind 之后固定 rowid 下界会丢行。不能把“无 DDL 暂不修”解释为验收通过。
3. **工程门禁 FAIL**：改动的 `history_page_wire_test.rs` 为 1935 行，超过 `STD-SIZE-001` 的 1000 行上限；HEAD 已有 1601 行，但标准明确要求本次修改的文件满足上限。
4. **报告须修正**：拒绝路径实际覆盖 8/11 类，不是 9/11；A14 的用例不是“第二次 Full”，而是 fork 后首次显式提交一次父 compaction；锁新增 Rust test 函数是 2 个（其中 1 个 child helper），不是运行数只增加 1。
5. **WASM 仍 PARTIAL**：native 回归和 emscripten `check` 通过；撤回实现后 native 292 条仍全绿。没有新增持久 WASM 回归用例，本复核没有建立 WASM 运行时红→绿证据。实施报告自己记载 SDK 目标仍红，不能用本轮 native 通过覆盖这项缺口。

### 判据级结论

| 判据 | 判定 | 本复核依据 |
|---|---|---|
| 受影响 native lib 可编译、可运行 | PASS | 4 crate，3540 passed / 0 failed / 33 ignored |
| 冷开锁改动保持有限等待、未改产品锁 | PASS | diff；默认全量 1 轮及 18 线程全量 3 轮均绿 |
| 冷开锁新增测试可靠检测撤销修复 | FAIL | 常量零预算 2 红 1 绿；实际调用点撤销 3 绿 |
| 冷开锁机制被消除 | PARTIAL | 只修等待容忍度；350ms holder 仍红，不是产品锁机制修复 |
| A14 本轮最小 fork→父 compaction→子快照/首页不变 | PASS | 新用例绿；子侧 compaction 污染注入红 |
| A14 全部冻结祖先、多页游标链形态 | PARTIAL | 本用例没有覆盖；不是该最小补测的实测结果 |
| 拒绝不得触发 initial load：新增覆盖的路径 | PASS | 真 ServerLoop；29 条 history_page 绿；注入 initial load 后通知断言红 |
| 拒绝路径覆盖与报告一致 | FAIL | R1/R2/R3/R4/R5/R7/R8/R11 = 8 类，R6/R9/R10 未持久覆盖 |
| R6/R9/R10 当前行为 | PASS（临时探针） | 3 场景在 1 条临时 wire 用例通过；探针已撤回 |
| rewind 后旧 cursor 拒绝 | PASS | 新用例通过；将热态版本回退为 0 后成功响应导致用例红 |
| A05 恢复工作量不随归档数线性增长 | FAIL | VM 指令计数 50036 / 500036 / 1000036，返回始终 3 条 |
| TUI 模式匹配编译修复 | PASS | 撤回一行出现 E0027；还原后 history_page 21 条绿 |
| WASM telemetry panic 修复运行时闭环 | PARTIAL | emscripten check 绿；native 回退不红；未独立执行 WASM 集成 |
| scope / 无 DDL 合规 | PARTIAL | 生产源码无 DDL；前序 A2 实验副本确有新增索引，见 §2 |
| 文件规模门禁 | FAIL | 修改文件 1935 > 1000 行 |
| 临时改动完整还原 | PASS | 5 文件 SHA-256、完整 git diff、报告写入前 git status 均字节一致 |

## 2. 实际改动与范围审计

### 2.1 基线

工作目录：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`。
HEAD：`3ccf75864c00475c07ffb735de92e0840c10fbca`。

以下所有 shell 命令均先执行用户指定前缀：

```bash
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history &&
```

正文中的命令表以此前缀为共同前提。未使用裸 cargo、`--workspace`、git add/commit/checkout/stash/rebase。没有自行执行数据库 DDL，没有改生产数据库。测试自身的临时 SQLite 夹具初始化不属于新增 schema 实施。

复核开始时实际 **不干净**，不能沿用输入中的“工作树干净”。命令：`pwd && git status --short && git rev-parse HEAD && git diff --stat && git diff --name-only`。

```text
 M langfuse-client/src/batcher/recovery.rs
 M peri-acp/src/dispatch/session_fork_test.rs
 M peri-acp/src/host/history_page_wire_test.rs
 M peri-resources/src/sessions/sqlite_store/connection_open_test.rs
 M peri-tui/src/kit/acp_events/history_page.rs
5 files changed, 721 insertions(+), 20 deletions(-)
```

### 2.2 每个源码文件

命令：`git diff --numstat`；行数由 Python `read_bytes().splitlines()` 与 `git show HEAD:<path>` 分别计数。

| 文件 | + / -；HEAD→当前行数 | 实际目的及范围判断 |
|---|---|---|
| `langfuse-client/src/batcher/recovery.rs` | +16/-5；446→457 | 将编码闭包在 wasm32/emscripten 就地执行，native 保留 spawn_blocking；无新依赖/公开接口变更。符合 B3 最小修复面，但无新增回归测试。 |
| `peri-acp/src/dispatch/session_fork_test.rs` | +234/-0；325→559 | 64KiB 测试页预算；新增父 compaction 不改 fork 子快照/首页用例。符合 B2。 |
| `peri-acp/src/host/history_page_wire_test.rs` | +338/-4；1601→1935 | 两个 helper；加强未协商拒绝；新增坏参数表驱动及 rewind 旧 cursor wire 用例。符合 B2 功能面，但违反修改文件规模规则。报告“+342 行”实际上是增删行合计 342，不是新增 342。 |
| `peri-resources/src/sessions/sqlite_store/connection_open_test.rs` | +132/-10；507→629 | 4 个获取调用改 250ms；两个 child 路径使用重试 helper；新增 holder child test 与外部 holder 测试。未改 `connection.rs`。比侦察点名三处更广，但同文件同类修复且 B1 报告已说明，不判无关扩张。 |
| `peri-tui/src/kit/acp_events/history_page.rs` | +1/-1；281→281 | `LocalUserBubble { text }` → `{ text, .. }`，修复新增 attachments 字段后的编译错误。前序报告已声明它先于 B1/B2/B3 存在、不是这些任务的产物；纳入当前 diff 复核，但不能归功于 B3。 |

### 2.3 未跟踪文件与报告偏差

复核前 `git status --porcelain=v1 --untracked-files=all` 的完整原文已保存在证据目录 `status-before.txt`，本复核没有改变它列出的文件。

1. 报告共 7 个：`S12-cold-open-lock.md`、`S12-a05-recovery-scan.md`、`S12-acceptance-gaps.md`、`S12-smoke-and-baseline.md`、`S12-cold-open-lock-fix.md`、`S12-gap-tests.md`、`S12-baseline-fix.md`。`S12-a05-fix.md` **不存在**。
2. `.recon/a1-raw/`：`exp.sh`、`loop.sh`、`summary-module-only.txt`、`summary-threads18.txt`，为锁侦察脚本/汇总。
3. `.recon/a2-raw/`：`ab-probe.sh/.out`、`index-probe.sql`、`scan-probe.sql/.out`、`realdb-probe.out`、`rewind-demo.out`、`timing-inproc-base.sql`、`timing-inproc.out`、`write-probe.sql/.out`，以及 probe、probe-ab、rewind-demo、size-probe、timing、write 六组 db-shm/db-wal，为存储性能实验。主 `.db` 与 `.log` 被忽略，不会出现在 git status。
4. `.recon/b1-raw/`：`loop-cargo.sh`、pre/post-fix-summary、final-summary、final-extra-summary，为锁修复验证产物。
5. `.recon/workflows/s12-remaining-items.mjs`：任务编排定义，非产品实现。本复核仅阅读，不执行其中 agent/workflow。
6. B3 所述忽略产物实际存在：`dist/wasm/peri_wasm.wasm` SHA-256 为 `ac7f334bd431c6f0da2cbd0a0bb7e18d88f798034543260e0c8c31ee0c5b1c01`。哈希吻合只证明文件一致，不证明本复核运行过它。
7. **前序范围违规证据**：A2 在实验副本上运行过 DDL，不能称“本轮没有任何数据库 DDL”。`index-probe.sql:16/21/24` 有 CREATE/DROP INDEX；本复核只读 `sqlite_master`，在 `probe-ab.db`、`size-probe.db`、`timing.db` 都观察到：

```text
('idx_active_b', 'CREATE INDEX idx_active_b ON messages(thread_id) WHERE excluded = 0')
```

这不是生产 schema 改动，但用户约束是“禁止任何数据库 DDL”，没有实验副本豁免。不能仅凭 tracked diff 没有 schema 文件就判全部合规。本复核没有执行这些 SQL 脚本，也没有删改其产物。

## 3. 独立运行的测试证据

原始日志目录（本机临时目录，非长期归档保证）：
`/var/folders/d5/gpfmkm2s4sqgwz5wwnj44p500000gn/T/s12-verification-mhj32tb0`。
下称 `E`。本报告保留关键原始输出，不依赖实施者的测试数字。

### 3.1 全量 lib（当前修复态，各 1 次）

统一命令：`./scripts/cargo-rmcp-patched.sh test --locked -p <crate> --lib`。

```text
peri-acp EXIT=0
 test result: ok. 890 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 93.37s
peri-resources EXIT=0
 test result: ok. 499 passed; 0 failed; 26 ignored; 0 measured; 0 filtered out; finished in 13.68s
langfuse-client EXIT=0
 test result: ok. 292 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 6.60s
peri-tui EXIT=0
 test result: ok. 1859 passed; 0 failed; 6 ignored; 0 measured; 0 filtered out; finished in 9.94s
```

日志：`E/{peri-acp,peri-resources,langfuse-client,peri-tui}-lib.log`。合计 3540 passed / 0 failed / 33 ignored；不把 ignored 计为通过。

### 3.2 还原后的新增/受影响范围

命令均为 `./scripts/cargo-rmcp-patched.sh test --locked -p <crate> --lib -- <filter>`；langfuse 再跑全 lib。

| crate / filter | 原始 test result 数字 | 日志 |
|---|---|---|
| peri-resources / lock_release_probe_budget_tolerates_an_external_holder | 1 passed; 0 failed; 524 filtered out | restored-lock.log |
| peri-acp / a_parent_full_after_fork | 1 passed; 0 failed; 889 filtered out | restored-a14.log |
| peri-acp / history_page | 29 passed; 0 failed; 861 filtered out | restored-acp-history.log |
| peri-tui / history_page | 21 passed; 0 failed; 1844 filtered out | restored-tui-history.log |
| langfuse-client / 全 lib | 292 passed; 0 failed; 1 ignored | restored-langfuse.log |

29 条 ACP history_page 范围包含两条本轮新增 wire 用例及加强的未协商拒绝用例；A14 单独运行。全部 exit=0。

### 3.3 真并发负载（修复态）

命令：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- --test-threads=18`，连续 3 轮。

```text
run1: test result: ok. 499 passed; 0 failed; 26 ignored; ... finished in 16.94s
run2: test result: ok. 499 passed; 0 failed; 26 ignored; ... finished in 21.76s
run3: test result: ok. 499 passed; 0 failed; 26 ignored; ... finished in 16.33s
```

日志：`E/resources-concurrent-{1,2,3}.log`，全部 exit=0。加默认并发全量为 4/4 轮通过；不外推 flake 概率为零。

### 3.4 WASM 编译检查

```bash
RUSTFLAGS="$(./scripts/cargo-wasm.sh --print-rustflags)" ./scripts/cargo-rmcp-patched.sh check --locked -p langfuse-client --target wasm32-unknown-emscripten
```

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 26s
EXIT=0
```

`cargo-wasm.sh` 此处只打印 flags，实际检查由用户指定脚本执行。日志 `E/langfuse-wasm-check.log`。**这是 cross-target 编译检查，不是 WASM 运行测试。**

## 4. 逐项证伪与反例（全部临时改动已撤销）

### 4.1 B1：零预算回退并非必红

临时只改 `LOCK_RELEASE_PROBE_BUDGET`：`Duration::from_millis(250)` → `Duration::ZERO`，保留新增测试。命令连续 3 次：

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- lock_release_probe_budget_tolerates_an_external_holder --nocapture
```

```text
run1 EXIT=101
thread '...lock_release_probe_budget_tolerates_an_external_holder' ... panicked at ...connection_open_test.rs:620:13:
release probe budget did not outlast the holder: session database initialization is busy
test result: FAILED. 0 passed; 1 failed; ... 524 filtered out; finished in 0.07s
run2 EXIT=101
test result: FAILED. 0 passed; 1 failed; ... 524 filtered out; finished in 0.02s
run3 EXIT=0
test result: ok. 1 passed; 0 failed; ... 524 filtered out; finished in 0.02s
```

日志 `E/lock-zero-{1,2,3}.log`。这是本复核直接观察到的反例，不是“可能有竞态”的静态猜测。

原因由代码顺序支持：`:616` 先异步写 release 文件，`:617` 才尝试抢锁；child 可以在两者之间醒来、drop 锁，所以零预算也可能成功。之前握手只保证 child 已持锁，**没有保证释放后的首次探测必然遇到锁占用**。

### 4.2 B1：撤销实际修复调用点，新测试 3/3 绿

进一步证伪，不改预算常量、不改新增测试主体：

1. 四处原测试的 `lock_schema_open(..., LOCK_RELEASE_PROBE_BUDGET)` 恢复 `Duration::ZERO`。
2. 两处 child 获取恢复 `OpenOptions::open` + `file.try_lock().unwrap()`。
3. 同 §4.1 命令连续 3 次。

```text
run1 EXIT=0: test result: ok. 1 passed; 0 failed; ... finished in 0.06s
run2 EXIT=0: test result: ok. 1 passed; 0 failed; ... finished in 0.09s
run3 EXIT=0: test result: ok. 1 passed; 0 failed; ... finished in 0.02s
```

日志 `E/lock-sites-reverted-{1,2,3}.log`。新测试直接使用常量测试另一路径，不依赖原四个被修调用点。因此“退回实际修复，新测试必须红”**不满足**。这不否认生产行为正确，而是否认当前回归保护充分。

### 4.3 B1 边界：外部持锁 350ms

保留完整修复，只在 `initialization_lock_holder_child_process` 看到 release 文件后、`drop(file)` 前临时插入 `std::thread::sleep(Duration::from_millis(350));`。运行 §4.1 同一命令：

```text
EXIT=101
panicked at ...connection_open_test.rs:621:13:
release probe budget did not outlast the holder: session database initialization is busy
test result: FAILED. 0 passed; 1 failed; ... finished in 0.31s
```

日志 `E/lock-holder-350ms.log`。这是可控外部 holder，不是声称已经复现了真实 fork 继承 350ms。结论限于：250ms 是容忍窗口，不是消除机制；负载导致进程释放/调度延迟超窗仍红。B1 对“真实泄漏远长于 250ms”的断言也不能推出所有非泄漏均短于 250ms。

以上三个实验每次 finally 恢复后 SHA-256 都为 `2ee3d86528735d27a91be312ab3086cd7de10a1e2e1c6ef9b858627231ef06c3`。

### 4.4 B2 A14：子快照污染反例

B2 **没有产品实现修复可撤销**；撤掉测试只会不再运行，不构成红→绿证据。本复核在允许修改的测试文件中注入子状态污染：仅在新用例内，将父 `parent_ids` 的加载目标及对应 `apply_compaction` 的目标一起换成 `fork_id`，不改生产实现。

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- a_parent_full_after_fork --nocapture
```

```text
EXIT=101
panicked at peri-acp/src/dispatch/session_fork_test.rs:451:5:
assertion `left == right` failed: 父收缩不得改变子快照的条目身份
test result: FAILED. 0 passed; 1 failed; ... 889 filtered out; finished in 0.24s
```

日志 `E/a14-child-compacted.log`；还原后 1 passed。证明断言能检测子侧被 compaction 污染；**不等同于实现侧撤销隔离逻辑**。测试只有 3 条消息、一次读尽的首页、没有非空 next_cursor、没有祖先冻结区，也没有 fork 前一次 Full。报告及注释称“第二次 Full”超过夹具事实，应修正文案或补足夹具。

### 4.5 B2 拒绝：先执行 initial load 的反例

只在新增参数表驱动测试中，每条坏请求前临时调用同连接真实 `session/load`（只有 sessionId），再执行原坏请求和原断言。这是**额外副作用注入**，不是修改生产拒绝实现；避开触碰不在本轮 diff 的 `server_loop.rs`。

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- a_rejected_page_request_never_runs_an_initial_load --nocapture
```

```text
EXIT=101
panicked at ...history_page_wire_test.rs:441:9:
只有 limit、没有 cursor 字段（R1）：拒绝不得发出任何回放通知（否则客户端会把拒绝当成一次 load）
test result: FAILED. 0 passed; 1 failed; ... 889 filtered out; finished in 0.27s
```

日志 `E/rejection-initial-load-injected.log`。证明至少通知断言不是空转；不宣称四类断言均独立证伪。存储断言只比较 `load_session_history().len()`，**不能检测等长内容改写**；`session_facts` 也是长度/版本/环境四元摘要，不是完整热态快照。

### 4.6 B2 过期 cursor：结构版本回退

在新用例真实 rewind 后、旧 cursor 请求之前临时设 `state.structure_version = 0`，使结构失效事实丢失，其他流程不变：

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- an_expired_cursor_is_rejected_without_an_initial_load --nocapture
```

```text
EXIT=101
panicked at ...history_page_wire_test.rs:574:28:
结构版本前进后旧游标必须被拒绝，实际返回了成功响应
test result: FAILED. 0 passed; 1 failed; ... 889 filtered out; finished in 0.29s
```

日志 `E/expired-version-reset.log`。证明用例能检测结构版本未推进这类状态回归；未改生产校验分支。还原后归入 29 条通过。

### 4.7 B2 缺少的三个 wire 分支：独立临时补测

在同一已改测试文件临时追加 `s12_review_missing_rejection_branches`，复用 PageFixture、真实 ServerLoop 和 transport：

1. 合法 cursor 但缺 sessionId：-32602；零通知；原会话四元事实不变。
2. 同会话置 closing：-32010；零通知。
3. 从热态集合移除会话后用原 cursor：-32602；零通知；不得创建会话。

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- s12_review_missing_rejection_branches --nocapture
```

```text
REVIEW R6 code=-32602 data=None
REVIEW R10 code=-32010 data=None
REVIEW R9 code=-32602 data=None
test result: ok. 1 passed; 0 failed; 0 ignored; ... 890 filtered out; finished in 0.33s
EXIT=0
```

日志 `E/rejection-boundaries.log`。当前这些边界通过，但没有保留在交付测试中；它们本来就没有 `data.reason`，不能机械套用新 helper 的“data 只含 reason”规则。

### 4.8 B3：回退修复，native 不变红

将 `encode_batch` 恢复为原先无条件 `tokio::task::spawn_blocking(move || {...}).await`，运行：

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p langfuse-client --lib
```

```text
EXIT=0
test result: ok. 292 passed; 0 failed; 1 ignored; ... finished in 8.53s
```

日志 `E/langfuse-reverted-lib.log`。还原后再次 292 passed，SHA 一致。

**没有完成 WASM 红→绿证伪**：本轮没有新增 WASM 回归用例；native 分支本来就等价，不能靠 native 证明无线程目标不会 panic。用户明确限定构建/测试入口为 cargo-rmcp-patched，本复核未另行运行 `bun test` 或修改 SDK 夹具。实施报告记载“修后 SDK 252 pass/2 fail”是**前序报告结果**，不是本复核实跑数字。

边界检查：`client_export.rs:120-170` 确認 gzip/none 都经 raw/wire 字节守卫；`gzip_and_none_share_otlp_semantics`、`raw_and_wire_limits_are_independent` 已在 native lib 实跑范围。SDK 夹具 `wasm-acp-integration.test.ts:60` 仍 `request.json()`，而默认 compression=Gzip（`client_export.rs:46`），故 gzip 夹具不匹配有独立静态支持；没有把该静态证据写成独立 runtime 复现。同步编码的 WASM 事件循环延迟、超限及错误回收仍未实测。

### 4.9 TUI：退回模式匹配修复

只改回 `AcpEventData::LocalUserBubble { text }`：

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- history_page --nocapture
```

```text
EXIT=101
error[E0027]: pattern does not mention field `attachments`
167 |                 | AcpEventData::LocalUserBubble { text } => {
    |                   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ missing field `attachments`
```

日志 `E/tui-head-pattern.log`；这是编译失败，**0 条测试实际运行**，不能写作“1 test failed”。还原后 history_page 21 passed。

边界检查：`acp_notifier/session_update.rs:334-338` 的当前 LocalUserBubble 解码明确 `attachments: Vec::new()`；`{ text, .. }` 不新增展示行为，只恢复编译。未来带附件的分页协议语义未验证，不能据此承诺附件回放；当前 21 条分页测试没有新增附件验收。

## 5. A05 独立实验（不执行 DDL）

### 5.1 只读查询计划与 VM 工作量

复用现有合成实验库 `.recon/a2-raw/probe.db`，Python `sqlite3.connect('file:<绝对路径>?mode=ro&immutable=1', uri=True)`；先检查 WAL 为空，再只读 schema/行数。Python SQLite 版本 3.51.0，与 Rust sqlx 的捆绑版本不同。

实际执行命令结构：

```bash
python3 - <<'PY'
import sqlite3
from pathlib import Path
c = sqlite3.connect('file:' + str(Path('.recon/a2-raw/probe.db').resolve()) + '?mode=ro&immutable=1', uri=True)
queries = [
 'SELECT message_id, content FROM messages WHERE thread_id = ?1 AND excluded = 0 ORDER BY rowid',
 'SELECT message_id, truncated, excluded, projection FROM messages WHERE thread_id = ?1 AND excluded = 0 AND (truncated = 1 OR projection IS NOT NULL)',
 'SELECT message_id, rowid FROM messages WHERE thread_id = ?1 AND excluded = 0 ORDER BY rowid ASC',
]
for q in queries:
    ops = [0]
    def step(): ops[0] += 1; return 0
    c.set_progress_handler(step, 1)
    rows = c.execute(q, ('probe-thread',)).fetchall()
    c.set_progress_handler(None, 0)
    print(len(rows), ops[0], c.execute('EXPLAIN QUERY PLAN ' + q, ('probe-thread',)).fetchall())
c.close()
PY
```

原始输出（完整实验还输出了 EXPLAIN 中的相关指令，日志 `E/a05-readonly-probe.log`）：

```text
sqlite_version=3.51.0
[('probe-thread', 200003, 3)]
RESULT_ROWS=3 SQLITE_VM_OPS=1000043
RESULT_ROWS=1 SQLITE_VM_OPS=1000051
RESULT_ROWS=3 SQLITE_VM_OPS=1000043
SEARCH messages USING INDEX idx_messages_thread_id (thread_id=?)
DeferredSeek ...
Column 0 5 ...
Next ...
```

三条 SQL 来自当前源码 `context.rs:137`、`compaction.rs:182`、`session_data/history.rs:41`；均无本轮 diff。**VM_OPS 是 SQLite VM 指令步数，不是扫描行数**，不冒充 A2 的 scanstats rows。

### 5.2 内存副本缩放及 rewind 下界反例

用 `source.backup(sqlite3.connect(':memory:'))` 将只读合成库复制到内存；只做事务性 DELETE/UPDATE DML，随后 rollback，不改磁盘库及 schema。

缩放：每轮 `BEGIN`；`DELETE FROM messages WHERE excluded=1 AND rowid>?`（10000 / 100000 / 200000）；运行 ACTIVE_ENTRY_POSITIONS_SQL，用 progress_handler 每条 VM 指令计数；`ROLLBACK`。

rewind 等价 DML：记原 active 的 `MIN(rowid)=200001`；`BEGIN`；`UPDATE messages SET excluded=0 WHERE thread_id=? AND rowid IN (1,2)`；比较完整 active 与追加 `rowid>=200001` 的 active；`ROLLBACK`。该操作与 `clear_rewind_excluded` 的恢复排除位语义一致，**不是端到端 rewind API 测试**。

```text
IN_MEMORY_REWIND cutoff=200001 active_rows=5 suffix_rows=3 lost_rowids=[1, 2]
ARCHIVED=10000 RESULT_ROWS=3 SQLITE_VM_OPS=50036
ARCHIVED=100000 RESULT_ROWS=3 SQLITE_VM_OPS=500036
ARCHIVED=200000 RESULT_ROWS=3 SQLITE_VM_OPS=1000036
```

日志 `E/a05-rewind-and-scaling.log`。结论：当前恢复查询工作量线性，固定后缀下界不安全。本实验不能证明数学意义上“所有无 DDL 算法均不存在”，只能否定现实现及这一候选收窄方案。

初次尝试 `Connection.deserialize` 载入 WAL 模式文件到内存，遇到 `sqlite3.OperationalError: unable to open database file`，exit=1；随后改用 SQLite backup API 成功。这个失败未计入 Rust pass/fail，也未掩盖为成功。

## 6. 字节级还原与最终状态

### 6.1 方法

1. 复核开始时，用 `git diff --name-only HEAD` 枚举 5 文件，将原始 bytes 备份到 `E/baseline/<原路径>`；保存 SHA-256、`git diff HEAD`、`git status --porcelain=v1 --untracked-files=all`。
2. 临时注入由精确 Edit/Write 完成；测试执行放在 Python `try/finally`，finally 以备份 bytes 还原，再打印 SHA-256。不是用 git checkout/reset 还原，没有覆盖其他文件。
3. 全部注入完成、报告写入之前独立比较完整 diff/status，结果如下。

```text
SHA256_MATCH True langfuse-client/src/batcher/recovery.rs 9f395ebedbf2a1e2a8d474d8dba8a703da0203a05e12b75d052b5e6ef26234d6
SHA256_MATCH True peri-acp/src/dispatch/session_fork_test.rs 827071ed635e4aef5d08e7406d42b8af07c509090036ab48ae1425d2256e380e
SHA256_MATCH True peri-acp/src/host/history_page_wire_test.rs 08cfbfec05e97396cb93290d58929789147334b97ed30061cfd7b93d03b9f702
SHA256_MATCH True peri-resources/src/sessions/sqlite_store/connection_open_test.rs 2ee3d86528735d27a91be312ab3086cd7de10a1e2e1c6ef9b858627231ef06c3
SHA256_MATCH True peri-tui/src/kit/acp_events/history_page.rs eef3bbde455e810a7fb3033241b6c24e72788c23db52aa95543131d60fafee6b
DIFF_BYTE_IDENTICAL True
STATUS_BYTE_IDENTICAL True
A05_FIX_REPORT_EXISTS False
HEAD 3ccf75864c00475c07ffb735de92e0840c10fbca
```

`git diff --check` 同时 exit=0，无输出。5 文件哈希是**复核前=复核后**的完整 SHA-256，不是基于行数推断。

### 6.2 报告产生的唯一预期状态差异

用户同时要求新增报告和 status 回原状态，二者字面上不能完全同时满足。处理方式：先证明写报告前完整 status 完全相同；写报告后只允许新增 `?? .recon/S12-verification.md`，不修改 ignore/index 来隐藏它。最终再次验证记录附于文末之前。

## 7. 未验证项、局限与修订要求

### 7.1 未验证项

1. WASM runtime、SDK `bun test`、gzip 夹具修复及 fresh WASM 产物重建均未独立执行。仅复核现有产物 hash、native 行为、emscripten 编译；不宣称 B3 目标转绿。
2. B2 没有产品实现改动可回退，本复核只在已修改测试文件里注入等价异常状态/副作用。没有复制前序报告对未改实现文件的注入做法，也没有完成每一个断言的独立 mutation coverage。
3. A14 未覆盖冻结祖先、子历史多页链、真实 Agent Full 生命周期；实际覆盖的是 Store facade 的 compaction 提交及 fork 子数据隔离。
4. 250ms 锁预算不保证任意机器/负载稳定；没有重跑 30 轮，没有定位 fork 继承者 PID，没有验证 Linux/Windows。350ms 为可控反例，不是实际 fork 窗口测量。
5. A05 没有复跑 1M 性能全套；使用 20 万行合成库和 Python SQLite 3.51.0。没有测峰值内存、真实云端后端、生产存储延迟或 schema 迁移代价。
6. 没有重跑全项目 E2E/交互式 TUI、文档测试、clippy、fmt 或全库规模扫描。改动规模门禁用本轮文件实际行数核实；未声称整个仓库符合规范。
7. `sessionWorkspaceV1` 重复 initialize 门控偏差本轮没有修复。本复核阅读实现/报告但没有再次做运行探针，因此它仍属已记录、待另行裁决的侧翼，不新增确定性产品失败结论。
8. 临时日志在系统 temp 目录，可能被系统清理；核心命令、变更方式、数字和错误片段已保存在本报告。没有上传任何报告或日志。

### 7.2 交付前最小修订清单

1. **锁回归测试**：用显式握手/受控首次 WouldBlock 建立确定顺序；验证撤销真实修复调用点会红，而不是只测试一个共享常量；说明预算边界，删除“必红/彻底消除”式过度表述。不要求本复核直接更改产品锁架构。
2. **测试布局**：按职责拆分 ACP wire 测试，保证每个本轮修改文件 ≤1000 行，同时保持测试挂载及 canonical 命令真实命中。
3. **验收记录**：把 9/11 修为 8/11；明确条数不变不等于内容不变；明确 A14 的一次父 compaction/单页/fork 边界。是否把临时 R6/R9/R10 探针转成持久测试由实施阶段处理。
4. **WASM 闭环**：在另行授权的 SDK 测试入口下独立重建并完成原实现失败、修复态成功的 runtime 回归；处理 gzip 测试夹具阻塞，不将“panic 消失”当整体 SDK 通过。
5. **A05**：需要用户裁决索引/DDL 或显式验收豁免；批准前不新增索引、不偷偷使用 rowid 后缀裁剪。本报告不会自行放宽 A05。
6. **范围记录**：确认保留 TUI 一行编译修复；说明 A2 对实验副本执行 DDL 的既成事实，不再把“未改生产 schema”等同“未执行任何 DDL”。

总体选择 **NEEDS-REVISION**，而非 DELIVERABLE：已有可用实现且 native 测试全面通过，但存在已复现的测试保护缺陷、明确工程门禁失败和未闭环验收。也不选 NOT-DELIVERABLE：尚未观察到本轮源码导致的 native 主路径全面损坏，问题有明确修订/裁决路线。

## 8. 最终检查实录

写入报告后执行 `git diff --check`（exit=0），并用 Python 比较 SHA-256、`git diff HEAD` 及完整 porcelain 状态（只剔除报告这一行）：

```text
FINAL_STATUS_MINUS_REPORT_BYTE_IDENTICAL True
FINAL_DIFF_BYTE_IDENTICAL True
FINAL_SHA256_ALL_MATCH True
FINAL_STATUS_NEW_LINES ['?? .recon/S12-verification.md']
FINAL_STATUS_REMOVED_LINES []
REPORT_EXISTS True
```

最终 `git status --short` 仍为原 5 个修改源码、原 7 个未跟踪报告、原 a1/a2/b1/workflows 目录，**唯一新增是本报告**。未留下任何证伪注入、临时测试或新增生产改动。

VERDICT: NEEDS-REVISION
