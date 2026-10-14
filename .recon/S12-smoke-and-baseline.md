# S12 · A4：基线失败定位 + 可离线冒烟清单

工作目录：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`
分支：`feat/transcript-shrink-history-pagination`（HEAD = `3ccf75864`）
本任务只做侦察，未修改任何仓库文件（只新增本报告）。

---

## 0. 结论速览

1. **基线失败可确定性复现**：SDK 包 `bun test` → `253 pass / 1 fail`，退出码 1，整轮约 3 秒。失败点固定为 `langfuse-client/src/batcher/recovery.rs:256`。
2. **根因**：emscripten 产物**没有线程能力**（构建未启用 pthread，glue 内 `Atomics` 计数 = 0），而 `langfuse-client` 在 **wasm 可达路径**上无条件调用 `tokio::task::spawn_blocking`；tokio 的 blocking pool 派生 worker 线程失败（`ENOTSUP` / os error 138）后 panic（`pool.rs:388`），配合 `-Cpanic=abort` 直接把 wasm 模块 abort。
3. **性质判定**：**产品缺陷**（`langfuse-client` 自身缺陷），不是测试夹具缺陷，也不是"不可修复的 WASM 运行时限制"。WASM 无线程是**触发条件**，缺陷是"该调用点未对无线程目标做降级"——同一 crate 的 `client_body.rs` 已经对 wasm 做过同类降级（`file_body` 显式返回 `Config` 错误），说明这是漏改而非能力边界。
4. **修复规模**：单文件、约 15 行、不动 native 行为 → 属"一行级/低风险"档。
5. **冒烟可用性**：最小 ACP stdio 冒烟与 Rust 离线集成测试**当前即可运行且已实测通过**；`e2e/` 的 vitest 套件**当前不可运行**（`e2e/node_modules` 未安装 + `e2e/tui-tester` 子模块未初始化）。

---

## 1. 前置环境确认（先确认再复现）

| 项 | 状态 | 证据 |
| --- | --- | --- |
| sqld 二进制 | **可用**（`sqld 0.24.32`） | `~/.local/share/mise/installs/http-sqld/0.24.32/sqld` |
| mise 信任 worktree | **否**（主仓库 trusted，worktree untrusted） | `mise trust --show` → `~/code/ai/peri-v4p3: trusted` / `.../peri-transcript-history: untrusted` |
| 未信任时的表现 | `mise exec -- sqld` 直接报错退出 1 | `mise ERROR Config files in .../@peri-sdk/mise.toml are not trusted.` |
| 解除办法（本次采用） | 用环境变量临时授权，**不写任何仓库/全局配置** | `MISE_TRUSTED_CONFIG_PATHS=/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history` |
| 解除后验证 | `MISE_TRUSTED_CONFIG_PATHS=... mise exec -- sqld --version` → `sqld sqld 0.24.32 (40c272de 2025-02-14)`，EXIT=0 | 实测 |
| wasm 产物 | 已构建（`dist/wasm/peri_wasm.wasm`，2026-10-14 02:48，sha256 `8d4b08ed...a2b05`） | 内含 `OTLP batch encoding job`=1、`telemetry spool`=3、`historyPagination`=1 → 产物含当前 spool 代码 |

**说明：本次复现全程带 `MISE_TRUSTED_CONFIG_PATHS`**，即 sqld **确实起得来**。下面的失败**不是** sqld 环境失败，而是真实的 wasm abort。

> 与简报的偏差：简报称"工作树干净"，实测 **不干净**——`peri-tui/src/kit/acp_events/history_page.rs` 有 1 行未提交改动。详见 §9.2。本任务未触碰它。

---

## 2. 基线失败复现（实际命令 + 真实输出）

### 2.1 命令 1：全量

```bash
cd npm-packages/@peri-sdk
MISE_TRUSTED_CONFIG_PATHS=/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history bun test
```

真实输出（尾部）：

```text
1 tests failed:
(fail) start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host [933.59ms]

 253 pass
 1 fail
 2066 expect() calls
Ran 254 tests across 34 files. [2.84s]
```

`rc=1`，wall clock **3 秒**。

### 2.2 命令 2：单跑失败用例

```bash
cd npm-packages/@peri-sdk
MISE_TRUSTED_CONFIG_PATHS=... bun test tests/wasm-acp-integration.test.ts -t "start, send two inputs"
```

真实输出（原样，未删改）：

```text
bun test v1.4.2 (744846f84)

tests/wasm-acp-integration.test.ts:

thread 'main' (1) panicked at langfuse-client/src/batcher/recovery.rs:256:5:
OS can't spawn worker thread: Not supported (os error 138)
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
Aborted()
error: 0
0
(fail) start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host [381.56ms]

 0 pass
 1 filtered out
 1 fail
 3 expect() calls
Ran 1 test across 1 file. [409.00ms]
EXIT=1
```

### 2.3 同文件第二个用例

`bun test tests/wasm-acp-integration.test.ts`（两条同跑）→ `1 pass / 1 fail`：
用例 2（`a second ManagedAgents owner cannot claim the same real Session...`）**通过**，只有用例 1 失败。
即：**失败与"WASM 实例启动/会话认领"无关**，只与"真的把 telemetry 导出去"这条路径有关。

---

## 3. 根因定位（代码证据链）

### 3.1 直接位置

`langfuse-client/src/batcher/recovery.rs:245-266`：

```rust
pub(super) async fn encode_batch(...) -> Result<ExportPayload, LangfuseError> {
    let max_wire_bytes = export.max_wire_bytes;
    tokio::task::spawn_blocking(move || {          // ← 256:5，panic 报在这里
        let writer = BudgetedWriter::new(...)?;
        let payload = export::encode(&events, writer, &export)?;
        counters.account_payload(payload.raw_len, payload.wire_len);
        Ok(payload)
    })
    .await
    .map_err(|_| LangfuseError::IngestionApi("OTLP batch encoding job did not complete".into()))?
}
```

唯一调用者：`langfuse-client/src/batcher/worker.rs:457`（worker 把 live 批次交给 `encode_batch`）。

### 3.2 为什么会 panic 而不是返回错误

1. panic 文本 `OS can't spawn worker thread: {e}` 在依赖内**只有一处**：
   `~/.cargo/git/checkouts/tokio-71a69293b56be585/7227f2d/tokio/src/runtime/blocking/pool.rs:388`：
   ```rust
   Err(SpawnError::NoThreads(e)) => panic!("OS can't spawn worker thread: {e}")
   ```
   其中 `SpawnError::NoThreads(io::Error)` 来自 `spawn_thread` → `std::thread::Builder::spawn` 失败（pool.rs:501 / 510-532）。
2. panic 位置显示为调用方 `recovery.rs:256`，是因为 `#[track_caller]` 链完整：
   `tokio::task::spawn_blocking`（`task/blocking.rs:219`）→ `Handle::spawn_blocking`（`runtime/handle.rs:233`）→ `Spawner::spawn_blocking`（`pool.rs:360`）→ 在 388 行 panic，位置归因到用户调用点。
3. 为什么"整个模块死掉"：`scripts/cargo-wasm.sh` 的 `RUSTFLAGS` 含 **`-Cpanic=abort`**，panic 升级为 `abort()`，emscripten glue 打印 `Aborted()`，wasm 实例不可恢复。
   （`error: 0` / `0` 两行是 glue 的退出桥接输出，不是用例断言。）

### 3.3 为什么在 wasm 上必然失败（构建级事实，与任何业务代码无关）

| 检查 | 结果 |
| --- | --- |
| `scripts/cargo-wasm.sh` 是否有 `-pthread` / `-sUSE_PTHREADS` / `-sSHARED_MEMORY` | **无**（grep 结果为空） |
| `dist/wasm/peri-wasm.js` 中 `Atomics` 出现次数 | **0** |
| `dist/wasm/peri-wasm.js` 中 `pthread`（忽略大小写）出现次数 | **0** |
| 链接参数（节选） | `-sMODULARIZE -sEXPORT_ES6 -sNODERAWSOCKETS -sALLOW_MEMORY_GROWTH ...`，无任何线程开关 |

→ 该 emscripten 产物不具备 pthread 能力，`pthread_create` 返回 `ENOTSUP`（os error 138），**任何** `tokio::task::spawn_blocking` 调用都必然 panic。这不是"偶发"或"资源不足"，是**恒定的**。

### 3.4 归因：产品缺陷 / 夹具缺陷 / 运行时限制？

**判定：产品缺陷（langfuse-client 内部），不是夹具缺陷；WASM 无线程只是触发条件。** 依据：

1. **不是夹具缺陷**：`tests/wasm-acp-integration.test.ts` 没有被本分支改过，且夹具显式、正当地断言 telemetry 必须导出（`expect(f.telemetry.length).toBeGreaterThan(0)`），这正是产品应有的行为；MISE 环境解除后另一条用例通过，说明夹具本身工作正常。
2. **不是单纯运行时限制**：同一 crate 已经建立了"无线程/无文件系统目标走有界内存路径"的显式模式：
   - `langfuse-client/Cargo.toml:24`：`fs2`/`reqwest(stream)`/`futures` 只在 `all(not(wasm32), not(emscripten))` 启用；
   - `langfuse-client/src/client_body.rs:35-42`：非 native 目标的 `file_body` **显式返回** `LangfuseError::Config("file-backed telemetry bodies require the native spool")`；
   - tokio 自己也对 emscripten 做了同类处理（`tokio/src/runtime/blocking/pool.rs:253/398` 注释 `// fs uses the inline shim`，`runtime/mod.rs:561` `// Emscripten uses the inline shim in crate::blocking`）。
   → 也就是说：**tokio 官方承认 emscripten 上不能用 blocking 池并给 `fs` 做了内联 shim，但 `task::spawn_blocking` 这个公开 API 并没有 shim**；`encode_batch` 漏改，属实现缺陷。
3. **不是本分支引入**：
   - `git diff --name-only 19215d3a1 HEAD`（基点→HEAD）**不含** `langfuse-client/**`、`peri-wasm/**`、`scripts/cargo-wasm.sh`；
   - `git diff --stat 19215d3a1 HEAD -- npm-packages/@peri-sdk/tests/wasm-acp-integration.test.ts` 为空（未改）；
   - `git log -1 -- langfuse-client/src/batcher/recovery.rs` = `7f0de56e8 Merge PR #189 from KonghaYao/feature/peri-telemetry-spool`，且 `git merge-base --is-ancestor 7f0de56e8 19215d3a1` 成立 → **该文件由基线（telemetry-spool 合并）引入，早于本分支基点**。

### 3.5 其他 wasm 可达的 `spawn_blocking` 站点普查

全 crate 共 8 处（不含测试）；逐个核对 wasm 可达性：

| 位置 | 用途 | wasm 是否可达 | 依据 |
| --- | --- | --- | --- |
| `batcher/recovery.rs:256` | OTLP 批次编码 | **是（本次失败点）** | worker 无条件调用 |
| `batcher/recovery.rs:401` | 文件 claim checkpoint | 否 | 仅 `PayloadBody::File`，wasm 无 spool |
| `batcher/stats.rs:318` | 端口用量探测 | 否 | `let Some(store) = store else { return; }`，wasm 下 store=None |
| `batcher/worker/recover.rs:32` | 启动恢复扫描 | 否 | `if !spool_enabled { return; }` + `spool/registry` 均 None |
| `client_send.rs:185 / 413` | 文件 checkpoint / 文件读泵 | 否（但**仍被编译**） | 仅文件正文路径；wasm 下 `file_body` 已显式报错 |
| `spool/cleanup.rs:164` | 暂存删除 | 否 | `spool.rs:92` 整模块 `cfg(not(wasm32/emscripten))` |

→ **当前只有 1 处**会导致 wasm abort；其余是"潜在地雷"（一旦 wasm 侧出现文件正文即会踩到），不是本次失败原因。

---

## 4. 最小修复方案与风险评估

### 4.1 方案 A（推荐）：给 `encode_batch` 做目标分支，无线程目标内联编码

保持 native 走 blocking 池不变，wasm/emscripten 就地执行；同 crate 已有同类写法（`client_body.rs`），改动集中在一个函数内，约 15 行：

```rust
pub(super) async fn encode_batch(...) -> Result<ExportPayload, LangfuseError> {
    let max_wire_bytes = export.max_wire_bytes;
    let job = move || -> Result<ExportPayload, LangfuseError> {
        let writer = BudgetedWriter::new(&telemetry, budget, spool, context, max_wire_bytes, cleanup)?;
        let payload = export::encode(&events, writer, &export)?;
        counters.account_payload(payload.raw_len, payload.wire_len);
        Ok(payload)
    };
    // WASM/emscripten 无 worker 线程：就地编码；native 保持 blocking 池。
    #[cfg(any(target_arch = "wasm32", target_os = "emscripten"))]
    { job() }
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "emscripten")))]
    {
        tokio::task::spawn_blocking(job)
            .await
            .map_err(|_| LangfuseError::IngestionApi("OTLP batch encoding job did not complete".into()))?
    }
}
```

**风险（低）**：
- native 行为逐字不变（cfg 分支保留原 `spawn_blocking` 与同一错误映射）。
- wasm 侧由"必然 abort"变为"可运行"，是严格改善；**残余风险是在 wasm 事件循环上做同步编码**（会阻塞当前 tick）。批次大小受 `export.max_wire_bytes` 约束，属有界工作，与 tokio 对 emscripten `fs` 的内联 shim 策略一致。
- 需同时补一条回归：`#[cfg]` 双分支最好各有一个能被 CI 覆盖的断言点（wasm 分支只能由 SDK 用例覆盖）。

### 4.2 备选方案

| 方案 | 内容 | 评估 |
| --- | --- | --- |
| B | 给 emscripten 产物启用 pthreads（`-pthread -sUSE_PTHREADS` + SharedArrayBuffer/COOP-COEP） | **不建议**。改动链接参数、宿主环境（SDK 打包与浏览器/worker 侧）、内存模型；当前 glue 无任何线程痕迹，属重写级。 |
| C | wasm 下关闭 Langfuse 导出 | **不可取**。会让 SDK 既有验收（`expect(f.telemetry.length).toBeGreaterThan(0)`）失落，属功能回退。 |
| D | 全 crate 统一一层 `blocking_or_inline()` 辅助 | 覆盖 3.5 表的潜在站点，更彻底，但超出"修一类问题的最小面"；可作为后续独立项。 |

### 4.3 是否值得在本交付内修

1. **它属于 `langfuse-client` 这个独立包**：与"历史分页/压缩归档回溯"切片无源码耦合（本分支未触碰该 crate），**不是本切片的返工**。
2. **但它压在本交付的验收门上**：SDK 端到端用例（真实 WASM ACP Host + 真实 Turso）恒有 1 条确定性红牌，任何含该基线的分支都无法全绿；`253 pass / 1 fail` 会被读成"交付有失败"。
3. **代价小**：单文件、~15 行、native 路径零影响、有同 crate 与 tokio 双重先例。
4. **结论**：**建议在本交付内一并修**（作为独立小提交、独立说明"修的是基线缺陷"），而不是留给下一个切片；若交付纪律要求严格隔离范围，则应把它登记为独立任务并在验收报告里显式标注"该红牌为已知基线缺陷（非本切片引入）"。

---

## 5. 可离线冒烟清单

前缀约定：`WT=/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`

| # | 冒烟方式 | 命令 | 前置依赖 | 预计耗时 | 本机现状 |
| --- | --- | --- | --- | --- | --- |
| S1 | **最小 ACP stdio 冒烟**（本次自建探针） | `node /tmp/a4-acp-smoke.mjs` | 已构建 `target/debug/peri`（已存在，2026-10-14 08:42）+ node | **实测 2.0s** | ✅ **已实测通过，exit 0** |
| S2 | Rust 离线集成测试：bare print 端到端 | `cd $WT && ./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --test print_bare` | Rust 工具链；无网络/无凭据（用本地 TcpListener 假 provider） | **实测 71s**（62s 编译 + 6.6s 用例） | ✅ **已实测通过，rc=0，2 passed** |
| S3 | Rust 离线集成测试：print / 后台 / meta CLI | `... --test print_exit`（10 用例）/ `--test print_background_exit`（3）/ `--test meta_session_cli`（19） | 同 S2 | 首次含编译；已编译后约 5~20s/目标（**估计**，本次未逐条实测） | ⚠️ 未实测（编译成本）；历史矩阵记录为全绿 |
| S4 | SDK WASM ACP 端到端（bun） | `cd $WT/npm-packages/@peri-sdk && MISE_TRUSTED_CONFIG_PATHS=$WT bun test` | ①`MISE_TRUSTED_CONFIG_PATHS`（否则 sqld 起不来 → 多 1 条环境失败）②已构建 `dist/wasm/*`（已存在）③`bun` | **实测 3s** | ❌ 可运行但**恒 1 条基线失败**（§2），会遗留 1 个孤儿 sqld（§9.1） |
| S5 | e2e vitest 套件（TUI / stdio ACP 夹具） | `cd $WT/e2e && npm run e2e -- --file tests/scenarios/workspace-mcp-resources.test.ts --serial --retry 0` 或 `npm run e2e:l0` | ①`git submodule update --init e2e/tui-tester`（**网络**）②`npm ci`（**网络**，需 bun/npm 源）③Rust 构建 ④tmux（已装 3.6a）⑤L0 不需要模型凭据/网络 | tiers.mjs 声明 **L0 约 3~4 分钟**；单文件 **约 1~3 分钟**（不含首次构建，**估计**） | ❌ **当前不可运行**（依赖缺失，见 §5.1） |
| S6 | ACP 协议层单进程测试（非进程级冒烟） | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --test integration_test` | Rust 工具链 | **估计** <1 分钟（进程内 mpsc，不含子进程） | ⚠️ 未实测；性质是组件测试，不覆盖 stdio/进程退出 |

**不需要**：venv / Python 虚拟环境（全仓库 e2e 与 scripts 未出现 `venv`/`VIRTUAL_ENV` 引用，grep 为空）；S1/S2/S3 不需要 sqld、不需要网络、不需要模型凭据。

### 5.1 S5 不可运行的原因（实测证据）

```text
$ ls e2e/node_modules            → No such file or directory
$ ls e2e/node_modules/vitest/vitest.mjs → No such file or directory
$ git submodule status           → -1d73f75510232aea4ec589e7dc563f9fcb23ee04 e2e/tui-tester   （前缀 `-` = 未初始化）
                                 → -20aeb89a45d8ef465d4deb58c0ef47d955602e3d peri-cool
$ ls e2e/tui-tester              → 空目录
```

两点独立阻断，**缺一不可运行**：

1. `e2e/node_modules` 不存在 → `vitest`、`dotenv`、`tsx`、`openai` 均未安装（`npm ci` 需要网络）。
2. `e2e/tui-tester` 子模块未初始化 → `helpers/peri.ts:11` `import { TmuxTester, createTester } from "tui-tester"` 解析失败；而 `helpers/build.ts` 又 `import { PROJECT_ROOT } from "./peri.js"`，**因此连"只用 stdio 夹具、不碰 TUI"的用例（如 `workspace-mcp-resources.test.ts`）也会被这条 import 链拖住**。

→ 本次**未安装** `e2e/node_modules`、**未初始化**子模块（避免改动工作树 + 避免拉取网络依赖）；故 S5 **没有实测数据**，只有上表的静态前置结论。

---

## 6. 实测冒烟记录（S1，原始输出）

探针脚本（**写在 `/tmp`，未落仓库**）：`/tmp/a4-acp-smoke.mjs`
它做的事：起本地假 Anthropic SSE model server（随机端口）→ 隔离 HOME 下写入 `$HOME/.peri/settings.json`（provider 指向该 server）→ `spawn(target/debug/peri acp --cwd <临时工作区>)`（`detached` 独立进程组，禁代理，`NO_PROXY=localhost,127.0.0.1`）→ `initialize` → `session/new` → `session/prompt` → `session/close` → 关 stdin。

真实输出（两次运行一致，退出码 0）：

```json
{
  "steps": [
    { "name": "initialize",    "error": null, "protocolVersion": 1 },
    { "name": "session/new",   "error": null, "sessionId": "01a13804-0bd9-7311-8590-168aca20da64" },
    { "name": "session/prompt","error": null, "stopReason": "end_turn", "modelRequests": 1 },
    { "name": "session/close", "error": null }
  ],
  "exit": { "code": 0, "signal": null },
  "exitCode": 0,
  "modelRequests": [
    { "path": "/v1/messages", "model": "a4-model", "tools": 17 },
    { "path": "/v1/messages", "model": "a4-model", "tools": 0 }
  ],
  "stderrTail": ""
}
SMOKE_EXIT=0     # 耗时 1987 ms
```

覆盖到的事实：真实二进制的 ACP stdio 握手可协商（`protocolVersion=1`）；`session/new` 返回 sessionId；`session/prompt` 真正打到模型端点（主请求带 17 个工具，另有 1 次无工具的辅助请求）并以 `end_turn` 收尾；`session/close` 无错误；**进程以 0 退出**（无残留：`ps` 复查无 `peri acp` 孤儿）。

S2 真实输出：

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1m 02s
     Running tests/print_bare.rs (target/debug/deps/print_bare-187bc0fd1e68cb75)

running 2 tests
test bare_respects_explicit_builtin_shutdown ... ok
test bare_write_read_bash_exits_without_loading_user_integrations ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.61s
RUST_EXIT=0 elapsed_s=71
```

---

## 7. 未验证项与局限

1. **修复方案 A 未实施、未验证**（本任务禁止改代码）。因此"修掉 `encode_batch` 后 wasm 用例即转绿"是**推断**：3.5 表说明它当前是唯一 wasm 可达站点，但**不排除**修掉后暴露下一处失败（需重建 `wasm32-unknown-emscripten` 产物 ≈ 数分钟 + emsdk，未做）。
2. **"HEAD 单独能否编译"未通过回退构建验证**（回退等于改工作树，被禁止）。§9.2 的结论是静态判定。
3. **孤儿 sqld 的确切机制未插桩**：只观测到"失败用例后 `dispose()` 未跑完"（临时目录残留 + sqld PPID=1），未定位到 wasm abort 到 JS `finally` 之间的具体断点。
4. **S3/S6 的耗时是估计值**，非实测；S5 完全无实测数据（前置不齐）。
5. **单次运行样本**：S1/S2 各复跑 1~2 次均一致；未做大样本 flake 统计。
6. 本报告所有 `sqld` 相关数字均在**本次会话进程**范围内核对，未清理他人会话留下的 5 个历史孤儿 sqld（见 §9.1）。

---

## 8. 附带发现（不在本任务范围，未处理）

### 8.1 基线失败用例会泄漏 sqld 进程与临时目录（受控实验）

| 运行 | sqld 数（前→后） | 临时目录数（前→后） | rc |
| --- | --- | --- | --- |
| 单跑**通过**用例（`-t "second ManagedAgents owner"`） | 6 → 6 | 10 → 10 | 0 |
| 单跑**失败**用例（`-t "start, send two inputs"`） | — → +1 | 10 → 11 | 1 |
| 全量 `bun test` | — → +1（本次为 PID 32318） | — → +1 | 1 |

取证：失败运行后残留进程 `PPID=1`（已成孤儿）、`--db-path` 指向的临时目录**仍存在**（`/var/folders/.../T/peri-sdk-wasm-acp-g8aYDs`，`EXISTS`）→ 夹具的 `dispose()`（`kill("SIGTERM")` + `rm(root)`）**没有跑完**。当前机器上另有 **5 个 ≥2 小时的同前缀孤儿 sqld**，应为历次同类运行的累积。

本次已清理**自己产生**的孤儿与临时目录；会话结束时复核：`sqld=5`、`tmpdirs=10`（与开工时一致），无 `peri acp` 残留。

### 8.2 工作树不干净：分支 tip 的同文件编译修复未提交

```text
$ git status --short
 M peri-tui/src/kit/acp_events/history_page.rs      # 未提交
$ git diff peri-tui/src/kit/acp_events/history_page.rs
-                | AcpEventData::LocalUserBubble { text } => {
+                | AcpEventData::LocalUserBubble { text, .. } => {
$ git blame -L 167,167 peri-tui/src/kit/acp_events/history_page.rs
0000000000 (Not Committed Yet) 167)  | AcpEventData::LocalUserBubble { text, .. } => {
```

静态判定（**未回退构建验证**，理由见 §7 第 2 条）：`peri-tui/src/kit/acp_types/event_data.rs` 中 `LocalUserBubble` 定义为 `{ text, attachments }`（`peri-tui/src/kit/acp_types/event_data.rs:122-125`，`attachments` 由 `3a4f86591` 引入且是分支基点的祖先），而分支提交 `9b96f456a` 写下的是 `{ text }`——Rust 的 struct 变体模式在不写 `..` 时必须列全字段，故 **HEAD 单独编译会失败**；当前 `target/debug/peri`（08:42）与 S2 的编译成功都是在**带这个未提交修复**的工作树上取得的。

影响：交付前必须把这 1 行纳入提交（或修正为等价写法），否则分支不可构建。本任务未触碰该文件。

---

## 9. 证据索引（可复现命令）

```bash
WT=/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history

# 环境
mise trust --show
cd $WT/npm-packages/@peri-sdk && MISE_TRUSTED_CONFIG_PATHS=$WT mise exec -- sqld --version

# 基线失败
cd $WT/npm-packages/@peri-sdk && MISE_TRUSTED_CONFIG_PATHS=$WT bun test

# 归因
git -C $WT diff --name-only 19215d3a1 HEAD | grep langfuse-client    # 空
git -C $WT log -1 -- langfuse-client/src/batcher/recovery.rs         # 7f0de56e8
grep -c "OS can't spawn worker thread" ~/.cargo/git/checkouts/tokio-71a69293b56be585/7227f2d/tokio/src/runtime/blocking/pool.rs  # 1（:388）
grep -c Atomics $WT/npm-packages/@peri-sdk/dist/wasm/peri-wasm.js     # 0

# 冒烟
node /tmp/a4-acp-smoke.mjs
cd $WT && ./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --test print_bare
```

VERDICT: BASELINE-FIX-SMALL
