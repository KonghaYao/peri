# 终局测试矩阵报告（S10 终局验证）

- 日期：2026-10-14（CST）
- worktree：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
- 分支：`feat/transcript-shrink-history-pagination`（改动全部未提交；`git status --short` 171 行、`git diff --stat` 145 files / +9203 / −853）
- 需求：`/Users/konghayao/Desktop/peri-transcript-history-requirements.md`（§9 验收表 A01~A25、§5.3 ACP 标准边界、§10 不在本任务内）
- 蓝图：`spec/issues/2026-10-13-transcript-shrink-history-pagination.md`
- 原始日志：`.recon/final-matrix-logs/`（本次全部命令的完整 stdout/stderr，34 个文件）
- 验证者声明：**未修改任何生产代码，未新增或调整任何测试文件**（§6 有核对证据）。所有 cargo 命令串行执行（一次一个进程）。

## 0. 结论摘要

| # | 命令（均为必需项） | 结果 |
| - | --- | --- |
| 1 | `check --locked -p peri-acp-types --all-targets` | PASS（exit 0；仅 1 条来自 patch 的第三方 `hyper-util` 的既有 warning） |
| 2 | `check --locked -p peri-acp --all-targets` | PASS（exit 0，无 warning） |
| 3 | `check --locked -p peri-agent --all-targets` | PASS（exit 0，无 warning） |
| 4 | `check --locked -p peri-resources --all-targets` | PASS（exit 0，无 warning） |
| 5 | `check --locked -p peri-tui --all-targets` | PASS（exit 0，无 warning） |
| 6 | `test --locked -p peri-acp-types --lib` | PASS：576 passed / 0 failed / 0 ignored |
| 7 | `test --locked -p peri-acp --lib` | PASS：884 passed / 0 failed / 0 ignored |
| 8 | `test --locked -p peri-agent --lib` | PASS：1089 passed / 0 failed / 0 ignored |
| 9 | `test --locked -p peri-resources --lib` | PASS：497 passed / 0 failed / 26 ignored |
| 10 | `test --locked -p peri-tui --lib` | PASS：1817 passed / 0 failed / 6 ignored |
| 11 | SDK `bun run typecheck && bun run build && bun test` | typecheck PASS、build PASS、**test 252 pass / 2 fail** |

- **必需矩阵 11 条：10 条全绿；Rust 5 crate 合计 4863 passed / 0 failed，无 flake。**
- 第 11 条的 2 个失败**不是本切片引入**，但结论与既有报告（S7b/S7c 判为「纯环境、切片前即存在」）**不完全相同**：解除环境阻塞后仍有 1 条真实失败（WASM 内 telemetry 导出 panic），归因见 §2。
- 附加层（非必需，本次补跑）：19 个集成测试目标（`tests/`）全部 exit 0，共 117 passed / 0 failed（§1.4）。

## 1. 逐条命令与结果

### 1.1 cargo check --all-targets（串行）

| 命令 | 退出码 | 输出摘要 | 日志 |
| --- | --- | --- | --- |
| `./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp-types --all-targets` | 0 | `Finished dev profile ... in 31.84s` | `final-matrix-logs/check-peri-acp-types.log` |
| 同上 `-p peri-acp` | 0 | `Checking peri-acp` → `Finished ... in 2m 02s` | `check-peri-acp.log` |
| 同上 `-p peri-agent` | 0 | `Finished ... in 1m 21s` | `check-peri-agent.log` |
| 同上 `-p peri-resources` | 0 | `Finished ... in 57.29s` | `check-peri-resources.log` |
| 同上 `-p peri-tui` | 0 | `Finished ... in 2.41s`（目标目录为热缓存） | `check-peri-tui.log` |

5 份日志中，本仓库代码的 warning 为 0；`check-peri-acp-types.log` 里有 1 条既有 warning，来自 patch 的第三方 crate（`target/peri-hyper-util-patches/.../hyper-util-0.1.21/src/client/legacy/client.rs:1551` `unused variable: timer`），与本切片无关。

### 1.2 cargo test --lib（串行）

| 命令 | 退出码 | `test result:` 行 |
| --- | --- | --- |
| `test --locked -p peri-acp-types --lib` | 0 | `ok. 576 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.32s` |
| `test --locked -p peri-acp --lib` | 0 | `ok. 884 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 289.86s` |
| `test --locked -p peri-agent --lib` | 0 | `ok. 1089 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 23.82s` |
| `test --locked -p peri-resources --lib` | 0 | `ok. 497 passed; 0 failed; 26 ignored; 0 measured; 0 filtered out; finished in 60.65s` |
| `test --locked -p peri-tui --lib` | 0 | `ok. 1817 passed; 0 failed; 6 ignored; 0 measured; 0 filtered out; finished in 46.86s` |

**无失败用例**，因此没有需要单跑确认 flake 的用例。`peri-resources` 的既有 flake（`connection_open_test` 锁探针、workspace git 探针）本次未复现。

被 ignore 的用例（默认不跑，符合预期）：

- `peri-resources` 26 条 = 21 条 cloud 实验（需 `.env`）+ 5 条 `history_perf_tests`（性能证据夹具）。**按任务要求未运行**（夹具 36 分钟）。
- `peri-tui` 6 条 = 4 条定向性能探针（`append_probe`×2、`legacy_group_probe`、`perf_probe`）+ 剪贴板性能对比 + release synthetic profile。

### 1.3 SDK（`npm-packages/@peri-sdk`）

| 命令 | 退出码 | 结果 |
| --- | --- | --- |
| `bun run typecheck` | 0 | `tsc --noEmit && tsc --noEmit -p tsconfig.demo.json` 通过 |
| `bun run build` | 0 | `Finished release profile ... in 5.66s` + `Built @peri-code/sdk with the workspace peri-wasm artifact`（含 1 条 `peri-wasm` linker warning，既有） |
| `bun test`（首次，默认环境） | 1 | **252 pass / 2 fail**：`start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host`、`a second ManagedAgents owner cannot claim the same real Session and never starts its transport` |
| `bun test`（补跑，sqld 可用） | 1 | **253 pass / 1 fail**：只剩 `start, send two inputs, ...`（WASM abort） |

日志：`final-matrix-logs/sdk-typecheck.log`、`sdk-build.log`、`sdk-test.log`、`sdk-test-with-sqld.log`、`sdk-wasm-acp-integration-with-sqld.log`。

### 1.4 附加层：集成测试目标（`tests/`，非必需，全部 exit 0）

命令形式：`./scripts/cargo-rmcp-patched.sh test --locked -p <crate> --test <target>`；日志 `final-matrix-logs/it-<crate>-<target>.log`。

| crate / target | 结果 |
| --- | --- |
| peri-acp-types / `system_reminder_trust_contract` | ok. 3 passed |
| peri-resources / `session_resources_contract` | ok. 7 passed |
| peri-acp / `compact_command_contract_test` | ok. 2 passed |
| peri-acp / `concurrent_bg_agent_test` | ok. 2 passed |
| peri-acp / `integration_test` | ok. 5 passed |
| peri-acp / `prompt_cache_boundary` | ok. 1 passed |
| peri-agent / `compact_authority_projection_test` | ok. 2 passed |
| peri-agent / `compact_pressure_adversarial_test` | ok. 10 passed |
| peri-agent / `compact_failure_adversarial_test` | ok. 21 passed |
| peri-agent / `compact_followup_behavior_test` | ok. 7 passed |
| peri-agent / `compact_session_adversarial_test` | ok. 8 passed |
| peri-agent / `compact_session_manual_adversarial_test` | ok. 7 passed |
| peri-agent / `middleware_tests` | ok. 4 passed |
| peri-agent / `stream_interruption_test` | ok. 3 passed |
| peri-agent / `stream_interruption_logs_test` | ok. 1 passed |
| peri-tui / `print_exit` | ok. 10 passed |
| peri-tui / `print_bare` | ok. 2 passed |
| peri-tui / `print_background_exit` | ok. 3 passed |
| peri-tui / `meta_session_cli` | ok. 19 passed |

合计 117 passed / 0 failed。这 19 个目标里含本切片改动过的 `peri-agent/tests/compact_authority_projection_test.rs`、`compact_pressure_adversarial_test.rs`、`peri-resources/tests/session_resources_contract.rs`，是必需矩阵（`--lib`）覆盖不到的一层。

## 2. 失败与 flake 分析

### 2.1 失败用例与单跑结果（全部为实测）

失败用例（SDK 包，两个用例同文件）：

1. `start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host`（`tests/wasm-acp-integration.test.ts:122` 的 fixture）
2. `a second ManagedAgents owner cannot claim the same real Session and never starts its transport`（同文件 `:180`）

| 运行 | 命令 | 结果 |
| --- | --- | --- |
| R1 首轮全量 | `bun test`（默认环境） | 252 pass / **2 fail**，两条例均为 `error: sqld exited (1)` at `waitForSqld (...:23:44)` |
| R2 单跑 | `bun test tests/wasm-acp-integration.test.ts`（默认环境） | **0 pass / 2 fail**，同一错误 → 确定性，非 flake |
| R3 单跑（解除环境阻塞） | `MISE_TRUSTED_CONFIG_PATHS=<worktree> bun test tests/wasm-acp-integration.test.ts` | **1 pass / 1 fail**：用例 2 通过；用例 1 以 WASM abort 失败 |
| R4 全量（解除环境阻塞） | 同 R3 的全量 `bun test` | **253 pass / 1 fail**（仅用例 1） |
| R5/R6 单跑用例 1 ×2 | `... bun test tests/wasm-acp-integration.test.ts -t "start, send two inputs"` + `RUST_BACKTRACE=1` | **2/2 同一 panic** → 确定性 |
| R7 对照（主仓库） | 在 `/Users/konghayao/code/ai/peri-v4p3`（主 checkout，dist 为 10-10 构建）跑同一测试文件 | **2 pass / 0 fail**（23 expect） |

### 2.2 第 1 层失败（R1/R2）：环境，非测试缺陷

- 直接原因：`mise ERROR Config files in .../.worktrees/peri-transcript-history/npm-packages/@peri-sdk/mise.toml are not trusted.` → `mise exec -- sqld ...` 无法启动真实 Turso，子进程立即退出（code 1）。
- 对照证据（实测）：主仓库 `mise exec -- sqld --version` → `sqld sqld 0.24.32`；`mise trust --show` → `~/code/ai/peri-v4p3: trusted`，worktree 的 `.../peri-transcript-history` 与 `.../peri-sdk`: `untrusted`。
- 判定：环境问题；该测试文件未被本切片修改。**但 S7b/S7c 先前「纯环境、跑不到 SDK 逻辑」的结论不完整** —— 本次解除环境阻塞后仍有一条真实失败（见 2.3）。

### 2.3 第 2 层失败（R3~R6）：WASM telemetry 导出 panic，非本切片引入

- 现象（原始输出）：

  ```text
  thread 'main' (1) panicked at langfuse-client/src/batcher/recovery.rs:256:5:
  OS can't spawn worker thread: Not supported (os error 138)
  note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
  Aborted()
  ```

- 代码位置：`encode_batch()` 的 `tokio::task::spawn_blocking(...)`（`langfuse-client/src/batcher/recovery.rs:256`），即 Langfuse batcher 的批次编码；wasm/emscripten 下无法创建 worker 线程 → abort，整个 wasm 模块终止。

归因证据链（全部实测或只读核对）：

1. **本切片未触碰相关代码**：`git status --short` / `git diff` 中无 `langfuse-client/**`、`peri-wasm/**`、`npm-packages/@peri-sdk/scripts/build.ts`、任何 `Cargo.toml`/`Cargo.lock`。
2. **panic 所在文件最后改动 = 本任务声明的基线 commit**：`git log -- langfuse-client/src/batcher/recovery.rs` → `7f0de56e8 Merge pull request #189 from KonghaYao/feature/peri-telemetry-spool`（基线）。该文件在切片前后未变。
3. **产物指纹对照**：主仓库 dist（10-10 构建，`2 pass`）的 wasm 中 spool 代码标记计数为 0（`OTLP batch encoding job`=0、`langfuse-client/src/spool`=0）；worktree 产物为 1/1，并含切片标记（`historyPagination`=1、`history_page`=4）。
4. **线程支持是构建级事实**：两个 `dist/wasm/peri-wasm.js` 的 md5 完全相同（`d4855ed14082b67e1aeacecd8de3edb6`），且 glue 中 `Atomics` 计数为 0 → 该 emscripten 产物**没有线程能力**，函数内 `spawn_blocking` 必然失败；这与切片代码无关。
5. **失败测试文件本身未被切片修改**：`git status` 无该文件；主仓库与 worktree 的同名文件 `diff -q` 为 IDENTICAL。
6. **主仓库对照全绿（R7）**：同机、同 Bun、同 sqld，说明失败不是运行环境「本机不支持」，而是当前源码产物相较 10-10 基线多出的 telemetry spool 路径。

**判定：该失败属基线（telemetry-spool 合并 `7f0de56e8`）引入的 WASM 导出缺陷，不是本切片引入。** 但它是一条**真实、确定性、非 flake**的红牌：任何在 wasm 内导出 telemetry 的路径都会 abort，因此 SDK 端到端用例在当前工作区（以及含该基线的任何后续分支）无法全绿。修复方向（不在本任务授权范围，仅供参考）：对 wasm/emscripten 目标把 `encode_batch`/读泵的 `spawn_blocking` 换成 inline 编码，或为产物启用 pthreads。

**残余不确定（如实记录）**：通过对照件（主仓库 dist）与失败件之间的差异，除本切片外还包含 telemetry-spool 合并与 3 个 subagent 提交（`d12173804`、`91161b81d`、`5be107603`），因此严格说这是「相对 10-10 产物的差异」，不是「HEAD 源码 vs HEAD 源码」的对照；未做「HEAD 源码的 wasm 冷构建 + 同源对照」（需独立源码树与数 GB 空间；本机磁盘已满，见 §5.3）。上述结论建立在证据 1~6 上；其中证据 4 表明线程能力是构建级常量，切片无法影响。

### 2.4 Flake 结论

- Rust 5 个 crate：0 failed，无 flake；已知的 `peri-resources` 并发 flake 未复现（本次全程串行）。
- SDK：两个失败均为确定性复现（R2 2/2、R5/R6 2/2），不存在 flake 判读。

### 2.5 环境观察（不影响上述结论）

1. 存在一个 **03:04 启动、已挂起 4 小时以上**的残留进程：`bun test tests/history-pagination.test.ts -t "TMP diag A"`（PID 34465，父进程已退出、PPID=1，日志 `/tmp/diagA.log` 只剩表头）。它未占用 CPU，也未影响本次任何结果，但它是前序会话的残留，建议清理。
2. `/tmp/peri-cf-dev-store` 下有 10-11 启动的常驻 `sqld --http-listen-addr 127.0.0.1:8090`（PID 88982），同样是历史残留。
3. 磁盘写满事件：`peri-tui --test print_exit` 首次编译因 `No space left on device (os error 28)`（写 `target/debug/incremental/.../dep-graph.part.bin`）失败；转为前台重试后通过（10 passed）。属环境资源限制，不是代码失败。

## 3. 与基线差异

### 3.1 通过数对比（实测 vs 已知基线）

| crate | 已知基线 | 本次实测 passed | 差 | ignored |
| --- | --- | --- | --- | --- |
| peri-acp-types | 568 | **576** | +8 | 0 |
| peri-acp | 863 | **884** | +21 | 0 |
| peri-agent | 1089 | **1089** | 0 | 0 |
| peri-resources | 481~484 | **497** | +13~16 | 26 |
| peri-tui | （未给；S7c 报告 1807） | **1817** | +10 | 6 |

所有可比项 **≥ 基线且 0 failed**。

### 3.2 静态核对（HEAD 源码 vs 当前源码，只读 `git archive HEAD` 到 /tmp 统计，未改工作区）

| crate | HEAD `#[test]`/`#[tokio::test]` 属性数 | 当前 | 净增 | 其中新增测试文件贡献 |
| --- | --- | --- | --- | --- |
| peri-acp-types | 547 | 576 | +29 | 19（`history_page_test.rs`） |
| peri-acp | 827 | 884 | +57 | 38（3 个新文件） |
| peri-agent | 1039 | 1044 | +5 | 0 |
| peri-resources | 475 | 518 | +43 | 32（5 个新文件） |
| peri-tui | 1866 | 1907 | +41 | 37（5 个新文件） |

- 用户给的口径与 HEAD 静态计数不一致（例：peri-acp-types 568 vs HEAD 547），推断其数字来自本轮中间态；由于实测全部 ≥ 基线且 0 failed，**不存在「测试被删除或静默跳过」的回归信号**。
- 运行计数与源码属性计数在 peri-acp-types（576/576）与 peri-acp（884/884）完全一致，可作为「无静默跳过」的旁证；peri-agent / peri-tui 的运行数含宏生成用例，未逐一对齐。

### 3.3 本切片新增的测试文件（交付物清单，非本次验证者改动）

- `peri-acp-types/src/history_page_test.rs`（19）
- `peri-acp/src/host/history_page_test.rs`（10）、`history_page_wire_test.rs`（13，含 1 ignore）、`requests_unnegotiated_replay_test.rs`（15）
- `peri-resources/src/sessions/sqlite_store/session_history_page_test.rs`（9，含 1 ignore）、`session_history_segment_test.rs`（8）、`session_history_perf_test.rs`（6，含 1 ignore，5 条 `#[ignore]` 性能夹具）、`peri-resources/src/sessions/remote/session_history_page_test.rs`（6）、`session_history_rewind_test.rs`（3）
- `peri-tui/src/kit/acp_events_test/history_page_test.rs`（10）、`kit/history_page_consumer_test.rs`（9）、`kit/history_paging_test.rs`（7）、`kit/message_area/viewport_anchor_test.rs`（7）、`kit/message_area/viewport_anchor_e2e_test.rs`（4）
- `npm-packages/@peri-sdk/tests/history-merge.test.ts`（12）、`tests/history-pagination.test.ts`（13）

（括号内为静态 `#[test]`/`it()` 计数；另有大量既有测试文件被本切片修改。）

## 4. 未验证项

1. **`history_perf_tests` 5 条性能用例**（`#[ignore]`）：按任务要求未运行；A04/A05/A06/A07 的数字仍只有 `.recon/perf-evidence.md` 的历史证据，本次未复测。
2. **云后端 21 条 cloud 用例**（`#[ignore]`，需 `.env` 授权测试库）：未运行；A13 的 Remote 侧只由 remote 单测（`session_history_page_test`、`session_history_rewind_test` 等）覆盖。
3. **TUI 渲染帧 / 整组件树 / 真实端到端**：本次只跑自动化层（`--lib` + `tests/`），`docs/verification/tui-integration-cases/` 的人工用例未执行。
4. **`e2e/` crate**：不在本矩阵命令内，未运行。
5. **基线同源对照**：未在 HEAD 源码上跑同一批命令（禁止 git 写操作）；未构建 HEAD 源码的 wasm 产物（需独立源码树与数 GB 空间，本机磁盘不足）。
6. **第 11 条的 1 条 WASM 失败未修复**：属基线缺陷，超出本切片授权范围。

## 5. 证据与可还原性

### 5.1 日志清单

`.recon/final-matrix-logs/`（33 个文件）：`check-peri-{acp-types,acp,agent,resources,tui}.log`、`test-lib-peri-{acp-types,acp,agent,resources,tui}.log`、`sdk-{typecheck,build,test,test-with-sqld,wasm-acp-integration-with-sqld}.log`、`it-<crate>-<target>.log`×19。

### 5.2 工作区未被验证者改动（自证）

- `git diff --stat`：验证前后**完全一致**（`145 files changed, 9203 insertions(+), 853 deletions(-)`）。
- `git status --short` 171 行；`.recon/` 之外的未跟踪文件 25 个，与开工时相同。
- 本次新增的文件只有：`.recon/final-test-matrix.md`（本报告）、`.recon/final-matrix-logs/**`（证据日志）、`/tmp/peri-head-baseline/`（静态计数用的 HEAD 导出，工作区外）。
- 未执行任何 git 写操作、DDL、依赖变更。

### 5.3 环境限制（影响可复现性的事实）

- 磁盘：开工时 `/System/Volumes/Data` 仅剩 520 MiB（100%），收尾 190 MiB；worktree `target/debug` 31 GB、`target/debug/incremental` 19 GB。任何冷构建都可能触发 `os error 28`。
- 结论中与「本机无 sqld / 缺 mise trust」相关的部分，可通过 `mise trust` 或 `MISE_TRUSTED_CONFIG_PATHS=<worktree>` 复现；后者为本次使用的非持久化手段（未修改任何 mise 配置）。

## 6. 本次运行新增 / 调整的测试文件

**无。** 未新增、未修改、未删除任何测试文件（也未修改生产代码）；所有改动均为 `.recon/` 下的报告与日志。§2 中为归因所做的对照（主仓库运行同一 SDK 测试、`MISE_TRUSTED_CONFIG_PATHS` 临时环境变量、`/tmp` 下的 HEAD 导出）均在 worktree 之外或以只读方式完成。
