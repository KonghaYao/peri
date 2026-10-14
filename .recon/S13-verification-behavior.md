# S13 行为修订独立复核（任务 D）

## 1. 结论

**BEHAVIOR-NEEDS-REVISION。** Native 全量通过，但真实调用点回退仍不能被锁回归检出；WASM 红→绿历史原始日志不可复核；A14 等验收口径及 A05 裁决未闭环。

1. **A1 FAIL**：四处真实释放探测调用回退到 `Duration::ZERO`，新回归连续 **3/3 绿**。修改共享 helper 会红，只能证明 helper 的预算受保护，不能替代真实调用点保护。
2. **A2 PASS（限定断言覆盖）**：按职责拆分已挂载；R6/R9/R10 为持久 wire 用例；拒绝路径注入异常回放通知后 **3/3 红**，还原后通过。存储只比较条数，不证明内容不变。
3. **A3 NOT-VERIFIED（完整红→绿链）**：本次独立执行当前 WASM 集成文件 **2 pass / 0 fail**；但实施报告引用的三次构建、旧实现红灯原始日志已全部删除，报告中的摘录不能作为独立执行证据。**建议保留修复**，不是建议回退。
4. 独立 native 全量：`peri-acp` **893 passed / 0 failed**；`peri-resources` **499 passed / 0 failed / 26 ignored**。合计 **1392 passed / 0 failed / 26 ignored**。
5. 本次临时改动已字节级还原。开工前已有的源码、未跟踪证据均保留；未新增 DDL。另独立复现既有 whitespace 门禁失败。

## 2. S12 §7.2 逐项核对

| 修订项 | 判据级结论 | 代码／运行证据及未落地部分 |
|---|---|---|
| 1 锁显式握手、受控首次 WouldBlock | PASS | `connection_open_test.rs:552-569,599-660`：child 持锁写 ready，父调用返回后才写 release；首次零预算探测必须 TimedOut。原态通过。 |
| 1 撤销真实修复调用点会红 | **FAIL** | 本次回退 `:168,329,396,453` 四处调用，回归 3/3 绿；见 §4。共享 helper 回退确实红，但四处调用仍可绕过它。 |
| 1 预算与根因边界 | PARTIAL | 新报告明确 250ms 是容忍窗口，未改产品锁；本次没有重做 100/350ms 边界实验，不能把前序数字写成本次实测。源码仍有“远长于本预算”概括，不能视为所有继承持锁的保证。 |
| 2 wire 按职责拆分、各文件 ≤1000、canonical 真命中 | PASS | 6 文件分别 333/557/168/518/127/542 行；`history_page` 32 条通过，其中 wire 21 条、5 子模块均被运行。 |
| 3 拒绝分支持久覆盖 | PASS（类别覆盖） | R6 `:276`、R9 `:351`、R10 `:441`；真 ServerLoop。旧 R1/R2/R3/R4/R5/R7/R8/R11 八类，加三类后为 **11/11 类**，不是每个参数变体都穷尽。 |
| 3 将原 9/11 更正为 8/11 | **未完整落地** | `.recon/S12-gap-tests.md:18` 仍写 9/11；S13 wire 报告未给出明确的“旧 8/11 → 当前 11/11”修正链。本报告按代码校正，不能算实施轮已改旧记录。 |
| 3 条数不变不等于内容不变 | PASS（披露），PARTIAL（保护） | S13 wire §7.1 第3项已披露等长改写漏网；实际新用例 `load_session_history(...).len()`、热态四元摘要仍非内容等价。 |
| 3 A14 一次父 compaction／单页／fork 边界 | **未完整落地** | `.recon/S12-gap-tests.md:42` 仍写“第二次 Full”；S13 行为报告没有纠正。实际仅 fork 后一次 `apply_compaction`，3 条自有消息、单页、`next_cursor=None`；见 §7。 |
| 4 WASM 独立重建原实现红→修复绿、gzip 夹具 | PARTIAL / **NOT-VERIFIED** | 夹具解压已落地且本次当前产物运行绿；历史构建与红灯原始日志消失，未独立重建红→绿。见 §6。 |
| 5 A05 用户裁决 DDL 或明确豁免 | **未落地／仍待裁决** | 未找到本轮授权索引或验收豁免；恢复生产 SQL 与 HEAD 一致，无新增索引／rowid 后缀裁剪。本次不重跑性能，不把全量绿解释为 A05 通过。 |
| 6 保留 TUI 一行修复并明确范围 | PARTIAL | `peri-tui/src/kit/acp_events/history_page.rs` 的 `{ text, .. }` 仍在；未回退。S13 未补独立的统一范围修订说明。本报告明确建议保留这项既有编译修复。 |
| 6 说明 S12 A2 实验副本已执行 DDL | **未完整落地** | S12 独立复核已记录，S13 行为实施报告未补正总范围口径；原 `index-probe.sql` 仍有 CREATE/DROP INDEX。不能把“无生产 schema 改动”写为整个历史“从未执行 DDL”。 |

上述“未落地”区分实现缺口与记录缺口：不能因本报告纠正文字就反推实施轮已完成修订。

## 3. 基线、命令与独立性

工作树：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`。
独立 `git rev-parse HEAD`：`3ccf75864c00475c07ffb735de92e0840c10fbca`。
开工已有 **15 个 tracked 修改文件 + 140 个未跟踪文件**，后者含 **26 个源码文件、114 个 .recon 文件**，不是干净工作树。

以下全部 shell 命令使用共同前缀，实际 Bash 调用亦如此：

```bash
cd /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history &&
```

Rust 一律由 `./scripts/cargo-rmcp-patched.sh` 执行，仅指定 `-p`，未用裸 cargo 或 `--workspace`；未执行用户禁止的 Git 写操作。WASM 仅执行既有 Bun 测试，不改 mise 信任配置、不启动长期服务。

独立原始证据目录（下称 E）：
`/var/folders/d5/gpfmkm2s4sqgwz5wwnj44p500000gn/T/s13-behavior-8qmmp9qq`。

1. `baseline.json`：开工时 **3179 个 tracked/untracked 普通文件** SHA-256；不遍历 ignored 构建产物、跳过 symlink。
2. `status-before.txt`、`diff-before.patch`：完整 porcelain 状态、`git diff HEAD`。
3. 被注入文件原字节备份；每条测试有独立 `.log` 与 `.exit`。
4. 命令用 Python `subprocess.run` 收集真实退出码、stdout/stderr，非 pipeline 最后一段退出码。并行全量 wrapper 外层返回 0 不作为测试通过依据，分别读取两个 Cargo 子命令的退出码。
5. E 暂存于系统临时目录，未删除；无永久保存保证。关键输出与 SHA-256 在本报告保留。

## 4. A1 锁回归证伪

### 4.1 原态

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- release_probe_waits_out_an_external_holder --nocapture
```

```text
EXIT 0
test sessions::sqlite_store::connection::open_tests::release_probe_waits_out_an_external_holder ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.31s
```

### 4.2 真实四处调用回退：FAIL 反例

只改 `connection_open_test.rs:168,329,396,453`，共享 helper 和回归本身保留：

```rust
// 前三处
let held = lock_schema_open(&path, Duration::ZERO).await.unwrap();
// 第四处
let held = lock_schema_open(&existing_alias, Duration::ZERO)
    .await
    .unwrap();
```

同 §4.1 命令连续三次；`lock-callsite-{1,2,3}.log/.exit`：

```text
RUN 1 EXIT 0
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.29s
RUN 2 EXIT 0
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.27s
RUN 3 EXIT 0
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.28s
```

**明确判 FAIL**：这是原故障写法重新出现在真实调用点、守卫仍绿的可执行反例，不是推测。原因：回归 `:636,650` 直接调用共享 helper，没有经过被退回的四个调用点。实现任务曾允许“共享 helper 路线”，但本任务明确要求真实调用点回退检出；两种证伪不能混为一谈。

### 4.3 对照：共享 helper 回退会红

恢复四处原字节后，只把 helper `:22` 改为 `lock_schema_open(path, Duration::ZERO).await`；同命令：

```text
EXIT 101
panicked at peri-resources/src/sessions/sqlite_store/connection_open_test.rs:639:5:
release probe gave up before its budget while the holder still held the lock: waited 154µs < 250ms
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 524 filtered out; finished in 0.02s
```

说明新握手能让 helper 的提前返回被观测到，不能将整项描述为“毫无价值”。等待判别仍包含计时和调度，握手确定的是持锁先后，并非数学上保证任何极端调度下零预算都不可能假绿。

### 4.4 SHA-256 还原

| 状态 | connection_open_test.rs SHA-256 |
|---|---|
| 原态 | `f8d545b20269402c270137e398a756d6d9995809fb0386afe301b9a6e87f43a7` |
| 四调用回退 | `601c306de70f07d3533c076d6c57baff8261dcd073b44837dd7b2aebd54845b4` |
| helper 回退 | `b3e740484a4d3405669740e3976c0c357ef181f436a8c0ae3e62d5e74662f405` |
| 每轮 finally 还原后 | `f8d545b20269402c270137e398a756d6d9995809fb0386afe301b9a6e87f43a7` |

两轮均输出 `BYTE_IDENTICAL True`。最后 resources 全量包含该用例，真实通过。

## 5. A2 wire 拆分与拒绝分支

### 5.1 原态与挂载

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_page_wire_tests::rejection_tests --nocapture
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_page --nocapture
```

```text
EXIT 0
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 887 filtered out; finished in 0.55s
EXIT 0
test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured; 861 filtered out; finished in 1.57s
MODULE_PASSED {'other': 11, 'chain_tests': 2, 'caps_tests': 6, 'initial_cursor_tests': 6, 'rejection_tests': 6, 'rewind_tests': 1}
```

父文件 `:22-39` 显式挂载五子模块；其他 11 条来自既有 history_page 纯函数测试及 session_replay。`wc -l` 独立输出：

```text
333 history_page_wire_test.rs
557 history_page_initial_cursor_test.rs
168 history_page_chain_test.rs
518 history_page_rejection_test.rs
127 history_page_rewind_test.rs
542 history_page_caps_test.rs
662 connection_open_test.rs
```

### 5.2 异常注入：三个新增用例全部红

仅临时改 `server_loop.rs` 两个拒绝出口：
1. `classify` 的 `Err(error)`，覆盖缺 sessionId 的 R6。
2. `handle_page` 返回 `Err` 后、发送错误响应前，覆盖 R9/R10。

在原错误响应前执行真实 transport `send_notification("session/update", ...)`，正文为标准 `agent_message_chunk` 文本。这是“拒绝却产生 initial-load 型回放副作用”的等价异常，**没有调用整套 initial load**；不把通知注入声称为完整 load 执行实验。错误码和原断言均未修改。

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_page_wire_tests::rejection_tests::a_page_request_ --nocapture
```

```text
EXIT 101
...a_page_request_for_a_closing_session_is_rejected_without_a_replay ... FAILED
panicked at peri-acp/src/host/history_page_rejection_test.rs:499:5:
拒绝不得发出任何回放通知（否则客户端会把拒绝当成一次 load）
...a_page_request_without_a_session_id_is_rejected_without_an_initial_load ... FAILED
panicked at peri-acp/src/host/history_page_rejection_test.rs:307:5:
拒绝不得发出任何回放通知（否则客户端会把拒绝当成一次 load）
...a_page_request_for_a_session_outside_the_live_set_is_rejected_without_creating_it ... FAILED
panicked at peri-acp/src/host/history_page_rejection_test.rs:406:5:
拒绝不得发出任何回放通知（否则客户端会把拒绝当成一次 load）
test result: FAILED. 1 passed; 3 failed; 0 ignored; 0 measured; 889 filtered out; finished in 0.47s
```

第四条是 A21 正向对照，仍绿。三条失败均是行为断言，不是编译失败或连接初始化失败。

`server_loop.rs` 原／还原 SHA-256：`fabe0783f1cbb87d39e6ab1e3286ec529931956666031816a05600e58d4d813f`；注入态：`e801a4178cb810043ed04117496a1800195064e30d94ba6d9bbb452bab62e7bd`。输出 `BYTE_IDENTICAL True`。`history_page.rs` 未注入、与 HEAD 相同。

**保护局限**：R6/R10 的热态四元为两项长度、结构版本及 environment 是否为空；R9 校验会话不存在与集合长度；存储均只比较 `.len()`。等长改写内容不在保护范围。通知排空仅保证同通道响应之前的通知，不覆盖未来实现延迟发送。

## 6. A3 WASM：哪些是实测，哪些不能独立确认

### 6.1 原始证据可用性

实施报告 `.recon/S13-wasm-closure.md:67-77,111-164` 声称：

1. `bun run build` 三次均 `BUILD_EXIT=0`，原实现产物不同于修复态。
2. 原实现目标运行 `TARGET_EXIT=1`，输出 `OS can't spawn worker thread: Not supported (os error 138)` / `Aborted()`。
3. 修复还原后 `TARGET_EXIT=0`；SDK 全量 254 pass。

以上是**待核实的实施者陈述**，不是本复核执行结果。报告 `:229` 明确已删除 `/tmp/a3-wasm-closure/` 的全部备份、探针、构建和运行日志；本次工具检查：

```text
folder_operations(operation="exists", folder_path="/tmp/a3-wasm-closure")
Folder does not exist at: /private/tmp/a3-wasm-closure
```

`.recon` 检索仅找到该报告及更早 final-matrix WASM 日志，没有发现报告所列 build1/2/3、run-D-red-reverted 原始文件。故无法比对原命令、退出码与原始输出是否来自所称三次构建，**不能独立确认旧实现红灯真实执行**。这不是证明实施者未执行或造假。

构建入口静态核实：`scripts/cargo-wasm.sh:34` 最终 exec `cargo-rmcp-patched.sh`。命令链看起来有效，不等于实际运行证据。本复核没有重建产物或临时回退 `recovery.rs`；不得将静态 cfg 审阅或 native 测试替代 WASM 红灯。

### 6.2 本次真实运行当前 WASM

```bash
cd npm-packages/@peri-sdk && bun test tests/wasm-acp-integration.test.ts
```

```text
EXIT 0
bun test v1.4.2 (744846f84)
(pass) start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host [840.89ms]
(pass) a second ManagedAgents owner cannot claim the same real Session and never starts its transport [376.32ms]
 2 pass
 0 fail
 23 expect() calls
Ran 2 tests across 1 file. [1253.00ms]
```

执行前独立 SHA-256：

```text
9f395ebedbf2a1e2a8d474d8dba8a703da0203a05e12b75d052b5e6ef26234d6  langfuse-client/src/batcher/recovery.rs
d24ec3a69bb5dc8f8d9ad5acb0ec0494cf655ac07d05c2d219a95b93610effac  npm-packages/@peri-sdk/tests/wasm-acp-integration.test.ts
ac7f334bd431c6f0da2cbd0a0bb7e18d88f798034543260e0c8c31ee0c5b1c01  npm-packages/@peri-sdk/dist/wasm/peri_wasm.wasm
```

三者与报告相符，只能证明当前文件一致及当前产物运行通过，**不能证明“产物必由当前源码独立重建”或历史红灯**。夹具 `:42-46,66` 确有 gzip magic 解压逻辑；本次未增加编码探针，不能把实施者 gzip 请求头／magic 数字写成本次实测。

### 6.3 是否存在“静态推断写成实测”

未找到可独立坐实的伪造实测点；报告列的是运行命令、退出码和运行形态摘录，而非仅列 `check` 后明说完成 runtime。问题是原始证据不可追溯。

必须降低以下表述的证明强度：
1. `S13-wasm-closure.md:75` “哈希不同说明重建吃到了源码变更”不足以**单独**排除其它输入变化；本次只能核对修复态产物哈希，旧产物已经不可得。
2. `:15` “回退会在真实 WASM 运行时 abort”在该报告是历史运行主张；本次不能仅凭 `spawn_blocking` 静态分析复述成独立实测。
3. `:182` SDK **254 pass** 未在本次重跑，不得与本次 **2 pass** 合并为独立全量结果。

判定 **WASM-NOT-VERIFIED（红→绿闭环）／当前 runtime PASS**。独立建议 **保留修复**：当前真实集成无 abort 且通过，改动局限于无线程目标就地编码、native 保留原 blocking 分支；无反证表明回退更安全。建议补保留源码哈希、两版构建日志、两版运行原始输出及各自退出码的可复核闭环，而不是为了日志缺口撤销工作中的修复。同步编码最坏阻塞、错误回收、CI 与全 SDK 回归不在本次结论内。

## 7. A14、A05 与验收口径

1. **A14 最小行为 PASS**：本次 ACP 全量原始日志包含：

   ```text
   test dispatch::session_fork_test::a_parent_full_after_fork_leaves_the_child_snapshot_and_pages_intact ... ok
   ```

   源码 `session_fork_test.rs:339-559` 先追加三条父自有消息，再 fork，读取子快照与首页后，仅一次调用父 `apply_compaction`。前后比较子 payload 身份／正文／flags，页身份／position／正文／next_cursor。`before_page.next_cursor.is_none()` 排除了多页形态。**不是“第二次 Full”、不是 Agent 自动触发 compaction 的全链路，也未证明所有冻结祖先／多页游标链**。原报告的“缺口已补”只能在此最小范围理解。本次未重复 A14 变异实验。
2. **A05 不通过验收，等待用户裁决**：`peri-resources/src/sessions/sqlite_store/session_data/history.rs` 与 HEAD 字节相同，`:41-42` 仍为 `WHERE thread_id = ?1 AND excluded = 0 ORDER BY rowid ASC`；schema／恢复路径没有本轮新实现。没有索引批准或豁免证据。维持 S12 的未闭环结论，**不将 S12 VM 指令计数冒写成本次实测**。
3. **历史 DDL 事实不能消失**：S12 独立复核已观察实验副本索引；本次只读核对 `.recon/a2-raw/index-probe.sql:16,21,24` 确有 CREATE/DROP INDEX，未执行该脚本、未打开实验库重做 DDL。本次“无新增 DDL”不是为 S12 历史违规洗白。

## 8. 独立全量运行

两条命令并发启动，各自输出及退出码分开保存；首次前台调用达到 120s 被工具转为后台，随后收到完成通知并读取结果，未启动重复任务。

```bash
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib
./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib
```

```text
COMMAND ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib
EXIT 0
test result: ok. 893 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 74.55s
COMMAND ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib
EXIT 0
test result: ok. 499 passed; 0 failed; 26 ignored; 0 measured; 0 filtered out; finished in 14.75s
```

这是还原注入后的当前工作树结果，各一轮。ACP 相对 S12 890 增加 R6/R9/R10 三条；resources 数量与 S12 一致。26 个 ignored 未运行，不能用默认全量通过支持 A05 性能验收。构建排队／编译时间不包含于上述 test finished 数字。

## 9. 范围、DDL、未跟踪残留及还原审计

### 9.1 当前改动归类

开工基线的 41 个非 `.recon` dirty/untracked 文件：

| 类别 | 文件／范围 | 判定 |
|---|---|---|
| S12 遗留 | `recovery.rs`、`session_fork_test.rs`、TUI `history_page.rs` | 既有改动，不归因于本次复核；分别为 WASM 最小修复、A14 补测、一行编译修复。 |
| A1 | `sqlite_store/connection_open_test.rs` | 范围内测试/helper；产品 `connection.rs` 与 HEAD 相同。 |
| A2 | `history_page_wire_test.rs` + 5 新子模块 | 范围内测试及挂载；生产 `history_page.rs/server_loop.rs` 最终与 HEAD 相同。 |
| A3 | SDK `wasm-acp-integration.test.ts` | 仅夹具 gzip 解码；recovery 修复没有再扩张。 |
| B1～B4 规模治理既有项 | `requests_rewind*`、`requests_unnegotiated_replay*`、Agent transcript 及拆分模块、ACP-types session_resources 及子模块、TUI system/session events 及子模块、resources remote/perf 及子模块 | 不属于 A1～A3，但开工前已存在，且本地 workflow 明列独立规模治理任务；不能误判成本次 D 越界。未在 D 重做 B1～B4 逐行语义守恒验收。 |

独立脚本枚举所有 15 tracked 修改与 26 untracked 源码，按实际内容计行；全部 ≤1000（最大 990）。这是实际代码集合，不覆盖 `.recon` 大日志/原始副本规模；不据此推翻 `S13-verification-size.md` 的另一验收口径。

### 9.2 DDL 审计

将全部非 `.recon` dirty/untracked 文件内 `CREATE/DROP/ALTER TABLE/INDEX/VIEW/TRIGGER` 行与对应 tracked HEAD 原文做 multiset 比较（忽略缩进、允许拆分搬移），输出：

```text
SOURCE_NEW_DDL_LINES {}
SOURCE_REMOVED_DDL_LINES {}
SOURCE_EQUALS_HEAD peri-acp/src/host/history_page.rs True
SOURCE_EQUALS_HEAD peri-acp/src/host/server_loop.rs True
SOURCE_EQUALS_HEAD peri-resources/src/sessions/sqlite_store/connection.rs True
SOURCE_EQUALS_HEAD peri-resources/src/sessions/sqlite_store/session_data/history.rs True
```

没有发现新增源码 DDL、schema 变更或恢复 SQL 下界裁剪。文字扫描不是所有动态 SQL 的形式证明；本复核没有自行执行任何 DDL/索引实验。按用户要求运行的既有测试会初始化临时数据库 schema，不能声称这些测试完全不含 schema 创建，但本轮未新增或修改其 DDL、未操作生产库。

### 9.3 未跟踪残留

开工已有 140 个未跟踪文件，包括 26 个待纳入版本控制的新模块；`.recon` 下已有 S12/S13 报告、raw 日志、脚本、`.orig`/`.rs` 副本、a2 实验数据库 sidecar。它们不是本次新产生，也未删改。报告所需原始日志与备份放 E，不污染工作树；E 为刻意保留的审计材料，不是假称“临时目录全部清空”。

本次 Bun 成功退出。未做开工前后的全系统进程／临时目录差集，**不声称机器上所有历史 sqld 或 `/tmp` 残留均已清理**，不处置其它任务进程。源码注入全部恢复的证据与进程全局清洁不是同一命题。

### 9.4 全量字节校验及门禁

写报告前独立重算所有 3179 个基线普通文件哈希，比较完整 `git diff HEAD` 及 porcelain 状态：

```text
BASELINE_CONTENT_CHANGES []
TRACKED_DIFF_BYTE_IDENTICAL True
STATUS_BYTE_IDENTICAL True
DIFF_CHECK_EXIT 2
peri-acp/src/host/requests_unnegotiated_replay_test.rs:517: new blank line at EOF.
```

Whitespace 失败位于开工已有的 B1 文件，复核未越界修复。未新增架构或模块入口，DOC-UPDATE-001 对本次 D 只需核对既有路由并写指定报告；未擅自更新其它文档。最终报告写入后的状态校验见 §11。

## 10. 未验证项与后续判据

1. **必须修订**：使真实四调用回退能被守卫检出；仅说明“调用点绕过 helper 不覆盖”不足以满足本次硬判据。
2. **WASM 证据补齐**：重新保存两版源码/产物身份、重建日志、runtime 原始输出和退出码；本次没有重建，没有独立旧实现红灯。
3. **口径修订**：旧 8/11 → 新 11/11；A14 限定一次存储 compaction、fork 子自有区和单页；存储条数不代表内容。报告不反向扩张测试保护能力。
4. **A05 等待用户**：未批准不加 DDL，不使用未经证明安全的 rowid 剪裁；性能目标不得静默豁免。
5. 每 crate 全量各一轮，不构成低概率竞态消除证明；无 TUI 本次重编译、无 Langfuse native 本次全量、无 SDK 本次全量、无 ignored/perf/CI 重测。TUI 一行修复保留是范围结论，不冒写为本次 TUI 运行验证。
6. 不能凭现有文件哈希确认实施者历史每条命令；对于已删除的原始证据只判不可独立复核，不臆断造假。

### 原始日志 SHA-256（E 内）

| 日志 | SHA-256 |
|---|---|
| lock-callsite-1.log | `15fd45d76bc225c163738555961b6aa716f6a665f48f8e89b75ed0fc20d56615` |
| lock-callsite-2.log | `e8673fab93b00f2adca022099ba9a2ae6b9d58f6fecd90ee6172d854a4e9385e` |
| lock-callsite-3.log | `3d8e813c15520a6c1c2428cc2ac9dc2dac3eb7ad036e9c2ef1cd621b9caff559` |
| lock-helper-red.log | `f2f85a1d137c0cfa25d2b1b694a350a60d268a8f0c227ac26ce1ded5cea941cc` |
| wire-red.log | `89c43ac47d607b4684d83f7029ef9484f9349e520a50192ce641136e8cec3383` |
| history-page-green.log | `3baf733f1c179630e6f07cfbb5797ce4a8207742ddd1f3468f90cfe2d3dbbbea` |
| peri-acp-full.log | `d20946166cc6cb8d8ba34bea6f06d6f10756a77b261ad179a86fda31bd8c146f` |
| peri-resources-full.log | `bfdafe3a5e726f67146224097cbf2471de27fb16d40f99f592d9401043859520` |
| wasm-current.log | `7df3d339298d9bf662e8b3ac82d4cebbe217a9301929058364eff6c9c8c46143` |

## 11. 报告写入后的最终审计

命令：Python 读取 E/baseline.json，对 3179 个基线普通文件重算 `hashlib.sha256`；`git diff HEAD` 与原 patch 做字节比较；`git status --porcelain=v1 --untracked-files=all` 仅剔除本报告一行后与原状态做字节比较；另执行 `git rev-parse HEAD`、`git branch --show-current`。

```text
FINAL_BASELINE_FILES 3179
FINAL_BASELINE_CONTENT_CHANGES []
FINAL_DIFF_BYTE_IDENTICAL True
FINAL_STATUS_MINUS_REPORT_BYTE_IDENTICAL True
FINAL_STATUS_ADDED_LINES ['?? .recon/S13-verification-behavior.md']
FINAL_STATUS_REMOVED_LINES []
FINAL_HEAD 3ccf75864c00475c07ffb735de92e0840c10fbca
FINAL_BRANCH feat/transcript-shrink-history-pagination
FINAL_SHA256 f8d545b20269402c270137e398a756d6d9995809fb0386afe301b9a6e87f43a7 peri-resources/src/sessions/sqlite_store/connection_open_test.rs
FINAL_SHA256 fabe0783f1cbb87d39e6ab1e3286ec529931956666031816a05600e58d4d813f peri-acp/src/host/server_loop.rs
```

因此本次 D **工作树唯一最终新增为指定报告**；所有源码证伪注入已还原，无新源码／测试／DDL 留下。最终需要修订的判据仍是 A1 真实调用点保护、A3 可独立复核红→绿证据、验收口径与 A05 裁决；不能以 native 全绿覆盖这些失败／未验证项。

VERDICT: BEHAVIOR-NEEDS-REVISION
