# S12 · B3：基线失败修复（langfuse-client 的 WASM telemetry panic）

工作目录：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
分支：`feat/transcript-shrink-history-pagination`（HEAD = `3ccf75864`）
依据：`.recon/S12-smoke-and-baseline.md`（下称"侦察报告"）§3.1 直接位置、§4.1 方案 A
本任务改动：**源码 1 个文件**（`langfuse-client/src/batcher/recovery.rs`）+ 重建 gitignored 产物（`npm-packages/@peri-sdk/dist/wasm/*`）。未提交、未 add。

---

## 0. 结论速览

1. **按侦察报告 §4.1 方案 A 的最小修复已实施并生效**：panic 消失，wasm 链路现在真的把 OTLP 正文 POST 到 Langfuse 端点（探针实测 1 次请求、7225 字节、`content-encoding: gzip`）。
2. **但目标用例没有转绿**：`bun test tests/wasm-acp-integration.test.ts -t "start, send two inputs"` 仍 `1 fail`，失败原因**换成了另一处**——夹具把 gzip 正文当 JSON 解析（`SyntaxError: JSON Parse error`），不是 abort、不是 panic。
3. 全量 `bun test` 由修复前 `253 pass / 1 fail`（数字取自侦察报告 §2.1 的基线实测，本会话只复现了单用例）变为 **`252 pass / 2 fail`**：同文件第二条用例（据侦察报告 §2.3 修复前是通过的）现在也红，同样红在 gzip 夹具。
4. 按任务规则 1（发现风险超出报告预期即停下只写报告），**修复到此为止**：第二处缺陷的修复面落在 `npm-packages/@peri-sdk/tests/wasm-acp-integration.test.ts`，既不在本任务授权范围，也与侦察报告 §3.4"夹具正当"的判定冲突，需要人来裁决改夹具还是改 SDK 配置。
5. 归属补充：**两处失败同源于 PR #189（`7f0de56e8`，2026-10-13，telemetry-spool 合并）**，早于本分支基点，均非本切片引入。

---

## 1. 修复前：本会话实测复现（原始输出）

```bash
cd $WT/npm-packages/@peri-sdk
MISE_TRUSTED_CONFIG_PATHS=$WT bun test tests/wasm-acp-integration.test.ts -t "start, send two inputs"
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
(fail) start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host [645.31ms]

 0 pass
 1 filtered out
 1 fail
 3 expect() calls
Ran 1 test across 1 file. [724.00ms]
EXIT=1
```

与侦察报告 §2.2 逐字一致，位置仍是 `recovery.rs:256`。

---

## 2. 修复内容（侦察报告 §4.1 方案 A，逐字落地）

```bash
cd $WT && git diff --stat -- langfuse-client/src/batcher/recovery.rs
#  langfuse-client/src/batcher/recovery.rs | 21 ++++++++++++++++-----
#  1 file changed, 16 insertions(+), 5 deletions(-)
```

`git diff` 要点：

```diff
-/// 批次编码（blocking job）；失败把事件与来源交回调用方按未提交处置。
+/// 批次编码（native 走 blocking job，无线程目标就地编码）；失败把事件与来源交回调用方按未提交处置。
@@
-    tokio::task::spawn_blocking(move || {
+    let job = move || -> Result<ExportPayload, LangfuseError> {
         let writer =
             BudgetedWriter::new(&telemetry, budget, spool, context, max_wire_bytes, cleanup)?;
         let payload = export::encode(&events, writer, &export)?;
         // 原文/线包累计字节按本进程产生或接手的 payload 计一次，重试不重复累计。
         counters.account_payload(payload.raw_len, payload.wire_len);
         Ok(payload)
-    })
-    .await
-    .map_err(|_| LangfuseError::IngestionApi("OTLP batch encoding job did not complete".into()))?
+    };
+    // 无线程目标（WASM/emscripten 未启用 pthread）派生 worker 线程必失败，而 `task::spawn_blocking`
+    // 没有 tokio 给 `fs` 那样的内联 shim，只会 abort 整个模块：就地编码。批次受 `max_wire_bytes`
+    // 约束，是有界同步工作；native 保持 blocking 池，行为与错误映射不变。
+    #[cfg(any(target_arch = "wasm32", target_os = "emscripten"))]
+    {
+        job()
+    }
+    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "emscripten")))]
+    {
+        tokio::task::spawn_blocking(job).await.map_err(|_| {
+            LangfuseError::IngestionApi("OTLP batch encoding job did not complete".into())
+        })?
+    }
```

约束自查：

1. **公开导出契约未动**：`encode_batch` 仍是 `pub(super)`、签名与参数逐字不变；未增删任何 `pub` 项。
2. **未引入新依赖**：`Cargo.toml` 未改（`git status -- langfuse-client/` 只有 `M src/batcher/recovery.rs`，无新增未跟踪文件）。
3. **native 行为等价**：`spawn_blocking` 调用与错误映射逐字保留，仅把闭包提取为 `job` 变量。
4. **改动范围**：`git status --short` 中属于本任务的只有 `M langfuse-client/src/batcher/recovery.rs`；其余 `M peri-acp/…`、`M peri-resources/…`、`M peri-tui/…` 是本工作树其他并发任务的未提交改动，本任务未触碰。
5. **格式**：`cargo fmt -p langfuse-client -- --check` → `RC=0`（首次检查报 `recovery.rs` 的 `map_err` 需按 rustfmt 展开，已按格式化器输出改写）。

---

## 3. 修复后验证（真实输出）

### 3.1 重建 wasm 产物（必需：不改产物则 run 的还是旧 wasm）

```bash
cd $WT
./scripts/cargo-wasm.sh build --locked --release -p peri-wasm --target wasm32-unknown-emscripten
```

```text
warning: `peri-wasm` (bin "peri-wasm") generated 1 warning
    Finished `release` profile [optimized] target(s) in 2m 57s
=== artifact sha256 ===
ac7f334bd431c6f0da2cbd0a0bb7e18d88f798034543260e0c8c31ee0c5b1c01  npm-packages/@peri-sdk/dist/wasm/peri_wasm.wasm
```

说明：`bun test` 直接读 `npm-packages/@peri-sdk/dist/wasm/`（`tests/wasm-acp-integration.test.ts:78`），该目录是 `scripts/build.ts` 从 `target/wasm32-unknown-emscripten/release/` 拷来的产物（`dist` 已被 `.gitignore:204` 忽略）；本次按 `build.ts` 的同一路径手工拷贝 `peri-wasm.js` + `peri_wasm.wasm`（该 glue 无 `./snippets/*` 导入，`build.ts:65-70` 的 snippet 分支为空，已 grep 确认）。
另：为满足 pre-commit 的 `cargo fmt --check` 而做了一次纯格式化改写，**重建前后的产物 sha256 完全相同**（`ac7f334b…`），即格式化未影响 codegen。

### 3.2 探针：导出链路真的通了（新增证据，落在 /tmp，不进仓库）

`/tmp/wasm-telemetry-probe.test.ts` 与夹具同构，只把 Langfuse 处理器换成"记录原始字节 + 头部"，命令：

```bash
cd $WT/npm-packages/@peri-sdk && MISE_TRUSTED_CONFIG_PATHS=$WT bun test /tmp/wasm-telemetry-probe.test.ts
```

```text
PROBE_HEADERS {"path":"/api/public/otel/v1/traces","method":"POST","contentEncoding":"gzip","contentType":"application/json","contentLength":"7225","authorization":"Basic cGstdGVzdDpzay10ZXN0","ingestionVersion":"4"}
PROBE_LEN 7225
PROBE_MAGIC_HEX 1f 8b 08 00 00 00 00 00 00 ff d5 5c fb 6f 1c c7
PROBE_HEAD_TEXT "\u001f�\b\u0000\u0000\u0000\u0000\u0000\u0000��\\�o\u001c�}�W\u0006\u0004\n�){'�$�TR@�e��^&�\u001aAd�s�s�\r�v�;�$ς\u0000;h�8"
PROBE_REQUESTS 1
(pass) probe raw langfuse request bytes [564.35ms]

 1 pass
 0 fail
```

`1f 8b 08` 是标准 gzip 头（本仓库 native 侧同一断言见 `client_export_test.rs:387`）。**即：修复后 wasm 侧确实完成了编码并把正文发出，未再 abort。**

### 3.3 目标用例（修复后仍红，原因已换）

```bash
cd $WT/npm-packages/@peri-sdk && MISE_TRUSTED_CONFIG_PATHS=$WT bun test tests/wasm-acp-integration.test.ts -t "start, send two inputs"
```

```text
bun test v1.4.2 (744846f84)

tests/wasm-acp-integration.test.ts:
SyntaxError: JSON Parse error: Unrecognized token ''
(fail) start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host [471.75ms]
POST - http://127.0.0.1:50042/api/public/otel/v1/traces failed

 0 pass
 1 filtered out
 1 fail
 6 expect() calls
TARGET_EXIT=1
```

同文件两条全跑（修复前为 `1 pass / 1 fail`）：

```text
(fail) start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host [481.17ms]
(fail) a second ManagedAgents owner cannot claim the same real Session and never starts its transport [138.52ms]
 0 pass
 2 fail
FILE_EXIT=1
```

全量套件（修复前 `253 pass / 1 fail`，取自侦察报告 §2.1）：

```text
2 tests failed:
(fail) start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host [475.52ms]
(fail) a second ManagedAgents owner cannot claim the same real Session and never starts its transport [140.32ms]

 252 pass
 2 fail
 2071 expect() calls
Ran 254 tests across 34 files. [1435.00ms]
EXIT=1
```

### 3.4 native 回归（证明没改坏 native 路径）

```bash
cd $WT && ./scripts/cargo-rmcp-patched.sh check  --locked -p langfuse-client --all-targets   # Finished in 43.14s，无 error
cd $WT && ./scripts/cargo-rmcp-patched.sh test   --locked -p langfuse-client --lib
cd $WT && ./scripts/cargo-rmcp-patched.sh clippy --locked -p langfuse-client -- -W clippy::all
```

```text
test result: ok. 292 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 6.73s
# clippy：Finished `dev` profile … in 24.21s（`langfuse-client` 无告警；唯一告警来自被 patch 的 hyper-util 依赖，与本次改动无关）
```

---

## 4. 关键发现：第二处基线缺陷（报告未预见，超出授权面）

### 4.1 现象与证据链

1. 新失败信息是 `SyntaxError: JSON Parse error: Unrecognized token ''`——引号里那个"看不见的字符"就是探针抓到的 gzip 魔数首字节 `0x1f`。
2. 夹具在进程内起 `Bun.serve`，处理器第 60 行直接 `telemetry.push(await request.json())`（`npm-packages/@peri-sdk/tests/wasm-acp-integration.test.ts:56-62`），**没有任何解压**；同文件 `grep -i "gzip|content-encoding|decompress"` → **无匹配**。
3. 产品的 gzip 是**生产默认**，不是偶发行为：
   - `langfuse-client/src/config.rs:105`：`TelemetryConfig::default()` 里 `compression: Compression::Gzip`；
   - `langfuse-client/src/config_test.rs:34`：`assert_eq!(Compression::default(), Compression::Gzip)`；
   - `langfuse-client/src/client_test.rs:12` 注释："内容编码按用例显式指定（gzip 是生产默认）"；
   - 产品自带测试一律**按 magic 判压缩后再解**（`batcher_concurrency_test.rs:7`、`batcher_shutdown_test.rs:10`）。
4. 第二条用例（claim 冲突）本身**没有任何 telemetry 断言**（`…test.ts:179-213`），它现在也红，是因为夹具是**同进程内**服务、处理器抛出的 `SyntaxError` 被 bun test 记到当时在跑的用例头上。探针把处理器换成"读原始字节"后**同一跳通过**，可反证红点是夹具解析而非产品/会话逻辑。

### 4.2 归属

```bash
git log --oneline -3 -- npm-packages/@peri-sdk/tests/wasm-acp-integration.test.ts
#  a08d47710 feat(sdk): rebuild the TypeScript SDK surface and sync docs, spec and CI   （2026-10-10）
git log --format='%h %ad %s' --date=short --reverse -S 'compression: Compression::Gzip' -- langfuse-client/src/config.rs
#  7f0de56e8 2026-10-13 Merge pull request #189 from KonghaYao/feature/peri-telemetry-spool
```

→ 夹具（2026-10-10，纯 JSON 假设）**早于** gzip 默认（2026-10-13，PR #189）；panic 站点同一提交。**两处都是基线缺陷，都不是本切片引入。**

### 4.3 为什么停下而不是顺手修

1. 侦察报告 §4.1 认定的最小修复面只有 `recovery.rs`，且 §3.4 明确把该夹具判为"显式、正当"，未授权改它。
2. 让它转绿必须动 `npm-packages/@peri-sdk/tests/wasm-acp-integration.test.ts`（验收夹具）或让 SDK 显式传 `compression: none`——两者都是**产品/验收口径决策**（是否接受"WASM 侧默认 gzip"），应由人裁决。
3. 侦察报告 §7 第 1 条已把"修掉后暴露下一处失败"列为未验证项；实际发生，按任务规则 1 停手上报。

### 4.4 后续可选修法（**未实施**，供裁决）

1. 夹具按 magic 解压（与 `batcher_concurrency_test.rs` 同法，最贴近仓库既有口径）：改动最小，只动 `tests/wasm-acp-integration.test.ts`。
2. 夹具保持不解压，改为断言"收到 gzip 正文 + `content-encoding: gzip`"，把 `expect(JSON.stringify(telemetry)).toContain("resourceSpans")` 换成解压后断言。
3. 让 SDK 在 wasm 配置里显式 `compression: none`：能立刻转绿，但把验收从"生产默认"偷偷降级成"非默认配置"，不建议。

---

## 5. 未验证项与局限

1. **目标用例未转绿（本任务验收未达成）**：本文给出的"修复生效"证据只到"panic 消失 + 正文成功上线（gzip）"；转绿依赖于 §4.4 的追加改动，超出授权。
2. **未改动、未验证第二处缺陷的修法**：§4.4 三条都只是建议，一行代码都没写。
3. **wasm 侧就地编码的时延未测量**：方案 A 的已知代价是"在事件循环上做有界同步编码"（受 `max_wire_bytes` 默认 4 MiB 约束），本次只证明功能通过，没有耗时数据。
4. **§3.5 其他 wasm 可达 `spawn_blocking` 站点未逐个插桩复核**：仅由"套件能跑到导出并发出请求"间接说明当前无第二个 abort 点。
5. **第二条用例失败的机制是推断**（同进程处理器异常记到当前用例），依据是"探针换掉处理器即通过""该用例无 telemetry 断言"，未做插桩。
6. **样本量小**：全量套件 1 次、目标用例 3 次、探针 2 次；均为确定性结果（无 flake 迹象），但未做大样本统计。
7. **本工作树有并发任务**：`git status` 里 `peri-acp/`、`peri-resources/`、`peri-tui/` 的未提交改动不是本任务的，也不影响 SDK 套件；但这些改动意味着"全量 bun test 的 252 pass"是本工作树混合状态下的数字。

---

## 6. 环境副作用与清理

1. **仓库内改动**：`M langfuse-client/src/batcher/recovery.rs`（有意）；`npm-packages/@peri-sdk/dist/wasm/{peri-wasm.js,peri_wasm.wasm}`（构建产物，`dist` 已被 gitignore）。**未 git add / commit / checkout / stash / rebase**，未执行任何 DDL。
2. **仓库外文件**：`/tmp/wasm-telemetry-probe.test.ts`（探针）、`/tmp/s12-*.sh`、`/tmp/{bun-target-after,bun-full-after,s12-fix-evidence,s12-rebuild,s12-post-verify}.log`。
3. **sqld 孤儿**：本会话共产生 6 个（失败路径泄漏，与侦察报告 §8.1 同因），**已全部 kill 并删除对应 `peri-sdk-wasm-acp-*` 临时目录**；机器上仍余 ≥3.9 小时前的 5 个历史孤儿，非本会话产物，未动。
4. 附带观察：修复 panic **并未**消除失败路径的 sqld/tmpdir 泄漏（3 次失败运行各 +1），说明泄漏出在夹具失败路径本身，不是 wasm abort 专属。
5. 给并发任务的提示：`dist/wasm/` 已换成修复后的产物，因此**本工作树任何人再跑 SDK 套件，看到的会是 `2 fail`（gzip 夹具）而不是原来的 `1 fail`（panic）**——红牌数变多，但不是修复引入的新缺陷，而是原来被 abort 掩盖的第二处基线缺陷显形。

---

## 7. 证据索引（可复现命令）

```bash
WT=/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history

# 修复前（换回旧产物才能复现 panic；当前工作树已是修复态）
cd $WT/npm-packages/@peri-sdk && MISE_TRUSTED_CONFIG_PATHS=$WT bun test tests/wasm-acp-integration.test.ts -t "start, send two inputs"

# 重建产物（改 crate 后必做）
cd $WT && ./scripts/cargo-wasm.sh build --locked --release -p peri-wasm --target wasm32-unknown-emscripten
cp target/wasm32-unknown-emscripten/release/peri-wasm.js  npm-packages/@peri-sdk/dist/wasm/peri-wasm.js
cp target/wasm32-unknown-emscripten/release/peri_wasm.wasm npm-packages/@peri-sdk/dist/wasm/peri_wasm.wasm

# 修复后：探针（证明正文已上线）+ 目标用例（证明仍红且红在夹具）
cd $WT/npm-packages/@peri-sdk && MISE_TRUSTED_CONFIG_PATHS=$WT bun test /tmp/wasm-telemetry-probe.test.ts
cd $WT/npm-packages/@peri-sdk && MISE_TRUSTED_CONFIG_PATHS=$WT bun test tests/wasm-acp-integration.test.ts
cd $WT/npm-packages/@peri-sdk && MISE_TRUSTED_CONFIG_PATHS=$WT bun test

# native 回归
cd $WT && ./scripts/cargo-rmcp-patched.sh check  --locked -p langfuse-client --all-targets
cd $WT && ./scripts/cargo-rmcp-patched.sh test   --locked -p langfuse-client --lib
cd $WT && ./scripts/cargo-rmcp-patched.sh clippy --locked -p langfuse-client -- -W clippy::all
cd $WT && ./scripts/cargo-rmcp-patched.sh fmt    -p langfuse-client -- --check

# 归因
git -C $WT log --oneline -3 -- npm-packages/@peri-sdk/tests/wasm-acp-integration.test.ts        # a08d47710
git -C $WT log --format='%h %ad %s' --date=short --reverse -S 'compression: Compression::Gzip' \
    -- langfuse-client/src/config.rs                                                            # 7f0de56e8
grep -n -i "gzip\|content-encoding\|decompress" $WT/npm-packages/@peri-sdk/tests/wasm-acp-integration.test.ts   # 无匹配
```

VERDICT: PANIC-FIXED-TARGET-STILL-RED-ON-SECOND-BASELINE-DEFECT-STOPPED-AND-REPORTED
