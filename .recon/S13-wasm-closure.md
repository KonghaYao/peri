# S13 · A3：langfuse-client WASM 修复的运行时闭环验证

工作目录：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`（git worktree，会话期间与其它并发任务共用）
分支 / HEAD：`feat/transcript-shrink-history-pagination` / `3ccf75864`
依据：`.recon/S12-baseline-fix.md`（B3 修复实施）、`.recon/S12-verification.md` §4.8（遗留项：只做了 emscripten 交叉编译检查，无 WASM 运行时红→绿证据）
本次改动：生产源码 **0 行**；测试夹具 1 个文件；重建 gitignored 产物 3 次。
未执行 `git add` / `commit` / `checkout` / `stash` / `rebase`，未执行任何 DDL。

## 0. 结论

1. **WASM 运行时红→绿闭环达成（真实运行，不是编译检查）**。把 `encode_batch` 退回 HEAD 实现并重建后，目标用例以 `panicked at langfuse-client/src/batcher/recovery.rs:256:5: OS can't spawn worker thread: Not supported (os error 138)` + `Aborted()` 失败（`TARGET_EXIT=1`）；字节级还原修复并重建后，同一用例通过（`TARGET_EXIT=0`）。
2. **产物可复现且确实随源码变化**：修复在位时两次独立构建的 `dist/wasm/peri_wasm.wasm` sha256 相同（`ac7f334b…`，与 S12 记录一致）；退回后变为 `7b45dfa5…`。
3. **S12 §3.3 的第二处红点是夹具问题，已按仓库既有口径修掉**。夹具把 gzip 正文当 JSON 解析；探针实测 `content-encoding=gzip`、正文 magic `1f 8b 08`（gzip）。产品默认 gzip 是既有契约（`langfuse-client/src/config.rs:105`、`config_test.rs:34`），产品自带测试一律按 magic 解压（`langfuse-client/src/batcher_concurrency_test.rs:7`），故改 **夹具**，不改产品配置（未采用 S12 §4.4 的"wasm 显式 `compression: none`"选项）。
4. **绿灯覆盖**：目标用例 1 pass；该文件 2 pass；`@peri-sdk` 全量 `bun test` **254 pass / 0 fail**（S12 记录 252 pass / 2 fail）；native `langfuse-client --lib` **292 passed / 0 failed / 1 ignored**。
5. **判定：保留修复**（回退会在真实 WASM 运行时 abort 整个模块）。

---

## 1. 入口与环境

### 1.1 使用的既有入口（未自创）

1. 构建：`cd npm-packages/@peri-sdk && bun run build`（`package.json` 既有脚本 → `scripts/build.ts` → 仓库 `scripts/cargo-wasm.sh build --locked --release -p peri-wasm --target wasm32-unknown-emscripten` + `Bun.build` 打包 + 拷贝 `dist/wasm/`）。以下报告的"重建"都指这一条。
2. 测试：`bun test tests/wasm-acp-integration.test.ts -t "start, send two inputs"`、`bun test tests/wasm-acp-integration.test.ts`、`bun test`（包级全量）。均为仓库 `package.json` 既有脚本 `test`/`test:wasm` 的同一命令。
3. native 回归：`./scripts/cargo-rmcp-patched.sh test --locked -p langfuse-client --lib`。
4. 未修改 `package.json`、未新增脚本、未改生产代码。

### 1.2 环境与准备

```bash
cd $WT && mise trust mise.toml && mise trust npm-packages/@peri-sdk/mise.toml
# mise trusted /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history
# mise trusted /Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history/npm-packages/@peri-sdk
cd npm-packages/@peri-sdk && mise exec -- sqld --version   # sqld sqld 0.24.32 (40c272de 2025-02-14)
```

`mise trust` 之前夹具在 `waitForSqld` 就失败（`Config files in …/@peri-sdk/mise.toml are not trusted`），与本任务输入描述一致；trust 后 sqld 可用，无其它环境阻塞。

工具链：bun 1.4.2 / mise 2026.5.1 / emcc 6.0.10（`~/.emsdk`，`prepare-emscripten.sh` 版本断言与补丁 stamp 均通过）/ rustc 1.99.0。

### 1.3 字节级基线（动手前）

```text
9f395ebedbf2a1e2a8d474d8dba8a703da0203a05e12b75d052b5e6ef26234d6  ← 修复版（langfuse-client/src/batcher/recovery.rs，= S12 记录值）
4d2fad62689c1572b0c9495b889e9ea5e342fcee8e682e870ec3e3e848af0460  ← HEAD 原实现（git show HEAD:…）
90da963ad0a4d1fade06a5668734894bacb24e1965639bf2274cfed2ac9d78ec  ← 夹具修改前
```

两版备份存 `/tmp/a3-wasm-closure/`（报告写完后删除，关键输出已抄录在本文）。

---

## 2. 夹具问题 vs 产品问题（任务要求 3）

**判定：夹具问题。** 证据链：

1. 产品侧 gzip 是显式契约，不是偶发：`langfuse-client/src/config.rs:105` `compression: Compression::Gzip`（`TelemetryConfig::default()`），`config_test.rs:34` `assert_eq!(Compression::default(), Compression::Gzip)`。
2. 产品自带测试都按 magic 判压缩后再解：`batcher_concurrency_test.rs:7-18`（`0x1f 0x8b` → `GzDecoder`，否则原样）。
3. 夹具原实现 `telemetry.push(await request.json())` 假定未压缩（`tests/wasm-acp-integration.test.ts:60`），`grep` 该文件无任何 gzip/decompress 处理。
4. 运行时实测（本任务，§4.5 探针）：WASM 侧发来的正文 `content-encoding=gzip`、magic `1f 8b 08`，与 `SyntaxError: JSON Parse error: Unrecognized token ''`（首字节 `0x1f`）互为印证。
5. 夹具是验收方，验收的是"WASM 真的上报 OTLP"；把验收降到非默认 `compression: none` 会削弱验收口径，故不采用。

**修法（最小、贴仓库既有口径）**：夹具侧新增 `otlpBody()`，按 magic 解压后 `JSON.parse`，gzip 与未压缩都能解；未新增对压缩方式的断言（保持 native 测试"两种都接受"的口径）。

---

## 3. 三次构建（耗时与产物哈希）

| # | 源码状态 | 命令 | cargo 耗时 | 产物 `dist/wasm/peri_wasm.wasm` sha256 |
|---|---|---|---|---|
| 1 | 修复在位 | `bun run build` | 3m34s（日志窗口 11:53:22→11:56:57） | `ac7f334bd431c6f0da2cbd0a0bb7e18d88f798034543260e0c8c31ee0c5b1c01` |
| 2 | 退回 HEAD | `bun run build` | 5m41s（11:58:05→12:03:48） | `7b45dfa5a4016fc0ed08c0a3aaaf2d94a7551b576a70886a3eb1dca1b5c43970` |
| 3 | 字节级还原修复 | `bun run build` | 4m43s（12:04:12→12:08:57） | `ac7f334b…`（与 #1 完全相同） |

构建 1 与 3 的源码哈希相同、产物哈希相同，说明构建可复现；构建 2 的产物哈希不同，说明重建确实吃到了源码变更（排除"跑了旧产物"的质疑）。

日志：`/tmp/a3-wasm-closure/build{1,2,3}.log`；均 `BUILD_EXIT=0`，无 error（只有 `peri-acp` dead_code 等既有 warning）。

## 4. 红→绿证据链（任务要求 2）

### 4.1 R1 · 原夹具 + 修复在位（复现 S12 §3.3，红）

```bash
cd $WT/npm-packages/@peri-sdk && bun test tests/wasm-acp-integration.test.ts -t "start, send two inputs"
```

```text
bun test v1.4.2 (744846f84)
tests/wasm-acp-integration.test.ts:
SyntaxError: JSON Parse error: Unrecognized token ''
(fail) start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host [1265.46ms]
POST - http://127.0.0.1:49678/api/public/otel/v1/traces failed
 0 pass / 1 filtered out / 1 fail / 8 expect() calls
TARGET_EXIT=1
```

与 S12 §3.3 逐字一致：panic 已消失，红点是夹具解析 gzip 正文。日志 `run-A-orig-fixture.log`。

### 4.2 R2 · 夹具修复后（绿）

夹具改动见 §5。同一条命令：

```text
(pass) start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host [4080.47ms]
 1 pass / 1 filtered out / 0 fail / 20 expect() calls
TARGET_EXIT=0
```

同文件两条用例：`2 pass / 0 fail`（`run-C-green-fullfile.log`，`FILE_EXIT=0`）。日志 `run-B-green-fixture-fixed.log`。

### 4.3 R3 · 退回原实现 → 重建 → 红（关键证据）

```bash
cd $WT && cp /tmp/a3-wasm-closure/recovery.rs.head langfuse-client/src/batcher/recovery.rs
shasum -a 256 langfuse-client/src/batcher/recovery.rs   # 4d2fad62689c1572b0c9495b889e9ea5e342fcee8e682e870ec3e3e848af0460
cd npm-packages/@peri-sdk && bun run build              # 见 §3 构建 2，产物 7b45dfa5…
bun test tests/wasm-acp-integration.test.ts -t "start, send two inputs"
```

```text
bun test v1.4.2 (744846f84)
tests/wasm-acp-integration.test.ts:

thread 'main' (1) panicked at langfuse-client/src/batcher/recovery.rs:256:5:
OS can't spawn worker thread: Not supported (os error 138)
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
Aborted()
error: 0
0
(fail) start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host [1062.55ms]

 0 pass / 1 filtered out / 1 fail / 5 expect() calls
TARGET_EXIT=1
```

要点：同一用例、同一夹具（已修好，排除夹具噪声）、同一命令；只有 WASM 产物不同 → 红点由修复回退引起，位置 `recovery.rs:256:5` 与 S12 §1 修复前记录完全一致。日志 `run-D-red-reverted.log`。

### 4.4 R4 · 字节级还原修复 → 重建 → 绿

```bash
cd $WT && cp /tmp/a3-wasm-closure/recovery.rs.fixed langfuse-client/src/batcher/recovery.rs
shasum -a 256 langfuse-client/src/batcher/recovery.rs   # 9f395ebedbf2a1e2a8d474d8dba8a703da0203a05e12b75d052b5e6ef26234d6（与备份一致）
cmp langfuse-client/src/batcher/recovery.rs /tmp/a3-wasm-closure/recovery.rs.fixed  # CMP_IDENTICAL_OK（字节相同）
git diff --numstat -- langfuse-client/src/batcher/recovery.rs                        # 16  5  （与 S12 记录一致）
cd npm-packages/@peri-sdk && bun run build              # 见 §3 构建 3，产物 ac7f334b…
bun test tests/wasm-acp-integration.test.ts -t "start, send two inputs"
```

```text
(pass) start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host [1542.64ms]
 1 pass / 1 filtered out / 0 fail / 20 expect() calls
TARGET_EXIT=0
```

同文件两条用例：

```text
(pass) start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host [1542.64ms]
(pass) a second ManagedAgents owner cannot claim the same real Session and never starts its transport [450.26ms]
 2 pass / 0 fail / 23 expect() calls
FILE_EXIT=0
```

日志 `run-E-green-restored.log`。

### 4.5 补充证据

gzip 探针（临时文件在 `/tmp`，不进仓库；由真夹具改写路径 + 打印请求头生成，产物 = 构建 3）：

```bash
cd $WT/npm-packages/@peri-sdk && bun test /tmp/a3-wasm-closure/a3-encoding-probe.test.ts -t "start, send two inputs"
```

```text
A3PROBE content-encoding=gzip content-length=18718 wireLen=18718 magic=1f 8b 08
A3PROBE content-encoding=gzip content-length=7323  wireLen=7323  magic=1f 8b 08
(pass) … [1233.69ms]   1 pass / 0 fail      PROBE_EXIT=0
```

→ 夹具新增的 magic 分支在真实运行中被走到（不是被 identity 分支蒙混过关）。日志 `run-F-probe.log`。

包级全量：`cd $WT/npm-packages/@peri-sdk && bun test` → `254 pass / 0 fail / 2081 expect() calls / 34 files`，`SUITE_EXIT=0`（S12 记录为 252 pass / 2 fail）。日志 `run-G-sdk-fullsuite.log`。

native 回归（修复在位）：`./scripts/cargo-rmcp-patched.sh test --locked -p langfuse-client --lib` → `test result: ok. 292 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 6.97s`，`NATIVE_EXIT=0`。日志 `run-H-native-lib.log`。

---

## 5. 变更清单

### 5.1 生产源码：无变更

`langfuse-client/src/batcher/recovery.rs` 结束态 sha256 `9f395ebedbf2a1e2a8d474d8dba8a703da0203a05e12b75d052b5e6ef26234d6`，与动手前逐字节相同（`cmp` 通过），`git diff --numstat` = `16 5`。任务期间只做了"退回 → 还原"的临时换档，未留下改动。

### 5.2 测试夹具：`npm-packages/@peri-sdk/tests/wasm-acp-integration.test.ts`（+6/−1）

```diff
@@ -39,6 +39,12 @@
+/** 正文按 magic 判压缩：遥测默认 gzip，两种情况都解出 OTLP JSON（同 langfuse-client 的 batcher_concurrency_test.rs）。 */
+function otlpBody(bytes: Uint8Array): unknown {
+  const raw = bytes[0] === 0x1f && bytes[1] === 0x8b ? Bun.gunzipSync(bytes) : bytes;
+  return JSON.parse(new TextDecoder().decode(raw));
+}
@@ -57,7 +63,7 @@
-    telemetry.push(await request.json());
+    telemetry.push(otlpBody(new Uint8Array(await request.arrayBuffer())));
```

结束态 sha256 `d24ec3a69bb5dc8f8d9ad5acb0ec0494cf655ac07d05c2d219a95b93610effac`。范围仅限 `npm-packages/@peri-sdk`（任务允许），未触碰 SDK 生产源码与其它包。

---

## 6. 未验证项与局限

1. **本任务没跑 TUI / peri-acp / peri-resources 等其它 crate 的回归**（不在 A3 范围；本工作树这些文件的未提交改动属其它并发任务）。
2. **夹具在失败/中止路径上会泄漏资源**：R1（夹具异常）与 R3（WASM abort）各留下 1 个 sqld 子进程（PPID=1）+ 1 个 `$TMPDIR/peri-sdk-wasm-acp-*` 目录；成功路径（R2/R4/探针/全量）不泄漏。已定位到我自己的 2 个并清理（§7）。**具体机制未定位**（疑似失败时 `dispose()` 未执行完，或 `mise exec` 的孙进程未随 SIGTERM 收敛），未做进一步实验。
3. **第二条用例（claim 冲突）不是本修复的回归守卫**：R3 用了 `-t` 过滤，只跑了目标用例；abort 会杀掉整个测试进程，故"退回修复时第二条用例会怎样"没有实测。它的历史红点是同进程夹具异常连带记红，现已随夹具修复转绿。
4. 夹具版本与产物组合只在本机验证（bun 1.4.2 / sqld 0.24.32 / macOS arm64 / emcc 6.0.10）；CI 未跑。
5. **同步编码的性能面未测**：`max_wire_bytes`（4 MiB）上限内就地编码对 WASM 事件循环的最坏阻塞时长、超限与错误回收路径都没有实测（与 S12-verification §4.8 的同一遗留）。
6. 本工作树**与其它并发任务共用**：会话期间 `.recon/`、`peri-acp/src/host/history_page_*` 等文件被其它会话改动（不属于本任务，我未触碰）。我的两个文件在收尾时按哈希复核过（§5 的 sha256 即结束态）。
7. 报告写入前未再重跑构建 3 之后的第四轮验证；§4.4 的绿与 §5 的结束态哈希来自同一次构建产物。

---

## 7. 清理（我自己启动的东西）

1. `mise trust`（2 条）为环境准备，保留（任务授权）。
2. 自起进程：R1 泄漏的 sqld PID 21931（11:57:33 启动，`--db-path …/peri-sdk-wasm-acp-hoN5qz/sessions`）与 R3 泄漏的 sqld PID 46709（12:03:58，`…/peri-sdk-wasm-acp-gmFElF/sessions`）已 `SIGTERM` 终止并复核消失。会话前既存、以及其它会话的 sqld（07:17–07:21 等，共 5 个）**未触碰**。
3. 临时目录：删除我这两次失败留下的 `$TMPDIR/peri-sdk-wasm-acp-{hoN5qz,gmFElF}`；其它会话遗留的 `peri-sdk-wasm-acp-*` 目录未触碰。
4. `/tmp/a3-wasm-closure/`（备份、探针、全部日志）在报告写完后整体删除；本文已抄录所有关键输出片段。
5. 未驻留任何服务；`bun test` 的模型/Langfuse 模拟端点随测试进程结束。

VERDICT: WASM-VERIFIED
