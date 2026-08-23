# RunPtcCode Node API 契约漂移与错误诊断信息丢失

**状态**：Implemented / pending acceptance
**优先级**：高
**类型**：缺陷 / 工具契约
**创建日期**：2026-08-23
**来源**：用户报告 + 运行时分层测试 + 静态代码核查

## 当前实现事实

当前 canonical 工具名为 `RunPtcCode`，且为 deferred-only：模型须经 `SearchExtraTools → ExecuteExtraTool` 执行；既有 direct tools 不受影响。旧 `run_code` 不可执行、不是 alias，仅保留为搜索迁移关键词。policy、HITL、事件与 tool card 投影 effective target；模型 assistant raw wrapper call 只保留协议配对。执行环境采用 ESM-only，Node module 只能使用动态 `await import(...)`，static `import` 与 `require` 均不可用。

## 当前 artifact 与启动契约

本 issue 最初记录的 `run_code` 与 eval 启动均为历史旧称/旧实现叙述；当前 canonical 工具为 `RunPtcCode`，生产 artifact 为 `@peri-code/ptc@0.2.1`。仓库跟踪 deterministic `dist`，Rust 通过 `include_bytes!` 内嵌；版本缓存 `~/.peri/ptc/0.2.1` 必须通过 package identity 与内容 hash 校验，每次执行再复制到 private temp，以 `node <entry>` 启动，避免 TOCTOU。Node 在接收 source 前完成 `ptc/start` protocol/build handshake；package version、build ID、protocol version、Rust 常量与 `dist` 不同步时默认 fail closed。

仅当 `PERI_PTC_ALLOW_NPX_FALLBACK=1` 时，才允许精确版本 `@peri-code/ptc@0.2.1` 的 `npx` fallback；它使用 private `HOME`/cache 与最小环境变量，但仍增加 registry/download 供应链风险，因此必须保持显式 opt-in。发布操作先运行 `npm run prepublishOnly`，成功后再运行 `npm publish`。

## 问题

`run_code` 的 schema 与工具描述称其在“普通 Node.js 进程”中执行 JavaScript，且“direct Node.js APIs”可直接使用。实际运行环境虽为 Node.js，但用户代码由 ESM adapter 内的 `new Function(...)` 执行，未注入 CommonJS `require`。模型按常见 Node.js 写法调用 `require('node:crypto')`、`require('node:zlib')` 或 `require('node:perf_hooks')` 时，执行会失败。

失败又被第二个问题掩盖：adapter 已通过 JSON-RPC error frame 提供稳定错误消息与 `data.code`，Rust 的 `JsRuntimeError::RpcResponse` 却只显示固定文本 `JavaScript RPC request failed`。因此 `ReferenceError`、语法错误、循环引用、`BigInt` 序列化失败和 adapter 资源限制在工具调用面呈现为完全相同的错误，用户和模型无法区分原因。

首次观察到的复杂示例并非因计算量、`Promise.all`、压缩或哈希本身触发限制；直接原因是 `require` 未定义。同等功能改为 `await import('node:crypto')` 与 `await import('node:zlib')` 后成功。

## 运行环境

- 日期：2026-08-23
- 平台：macOS / `darwin`
- `run_code` 子进程：Node.js `v22.20.0`
- 默认限制（静态核查）：60 秒 wall timeout、256 KiB source、1 MiB input、4 MiB frame/result、1 MiB logs、16 个 internal calls、4 个 concurrent executions

## 最小复现

### R1. CommonJS Node API 失败

```js
const crypto = require('node:crypto');
return { hash: crypto.createHash('sha256').update('perihelion').digest('hex') };
```

实际结果：

```text
Tool execution failed: run_code - JavaScript RPC request failed
```

环境探针：

```js
return {
  requireType: typeof require,
  processType: typeof process,
  bufferType: typeof Buffer,
  consoleType: typeof console,
};
```

实际成功值：

```json
{
  "requireType": "undefined",
  "processType": "object",
  "bufferType": "function",
  "consoleType": "object"
}
```

动态 import 可完成同等操作：

```js
const crypto = await import('node:crypto');
return { hash: crypto.createHash('sha256').update('perihelion').digest('hex') };
```

该调用成功并返回 SHA-256。`node:zlib` + `node:util` 的 gzip/gunzip 往返测试也通过。

### R2. 不同执行错误退化为同一文本

以下用例均只返回：

```text
Tool execution failed: run_code - JavaScript RPC request failed
```

```js
throw new Error('intentional sentinel error');
```

```js
this is invalid javascript !!!
```

```js
return { bigint: 42n };
```

```js
const value = {};
value.self = value;
return value;
```

超过 4 MiB result 或超过 1 MiB logs 的 adapter 资源限制也显示相同文本，无法辨认 `RESOURCE_LIMIT`。

## 分层测试结果

| 测试面 | 用例 | 结果 | 结论 |
| --- | --- | --- | --- |
| 基础计算 | 生成 40 个质数、构造 `8×8` 矩阵、聚合 checksum | 成功 | 基础执行正常 |
| 异步 | `Promise.all` 20 项纯计算 | 成功 | 并发 Promise 不是触发因素 |
| 全局对象 | `process.version`、`process.platform`、`Buffer` | 成功 | Node 全局对象部分可用 |
| CommonJS | `require('crypto')`、`require('node:crypto')` | 失败 | `require` 未注入 |
| ESM | `await import('node:crypto')` | 成功 | 动态 import 是当前可用路径 |
| 压缩 | 动态 import `node:zlib`，gzip/gunzip 往返 | 成功 | 首次失败不是 zlib 或 promisify 限制 |
| 日志 | `console.log/info/warn/error` 小日志 | 成功 | 日志捕获正常 |
| JSON 特殊值 | `undefined`、`NaN`、`Infinity` | 成功但归一为 `null` | 符合 `value ?? null` / JSON stringify 行为，但应文档化 |
| 容器 | `Map`、`Set` | 成功但返回 `{}` | 原生 JSON 序列化语义，应文档化 |
| 不可序列化值 | `BigInt`、循环引用 | 失败且仅泛化错误 | 诊断丢失 |
| 结果限制 | 约 100 KiB result | 成功 | 正常结果路径可承载中等输出 |
| 结果限制 | 约 4.3 MiB result | 泛化 RPC failure | `RESOURCE_LIMIT` 未透出 |
| 日志限制 | 约 1.1 MiB logs | 泛化 RPC failure | `RESOURCE_LIMIT` 未透出 |
| CPU | 5,000,000 次整数循环 | 成功 | 普通计算负载正常 |
| timer | 50 ms `setTimeout` | 成功 | event loop 正常 |
| wall timeout | 永不 settle 的 Promise | 60 秒后明确 `JavaScript execution timed out after 60s` | Rust host timeout 能保留分类 |
| internal calls | 17 个并发不存在工具调用，代码内捕获错误 | 成功，17 个 `UNKNOWN_TOOL` | internal tool errors 在 JS 内可结构化捕获；16 是并发槽位而非总调用硬上限 |

## 根因

### C1. 执行上下文与工具契约不一致

- `peri-js-runtime/src/executor.rs` 使用已验证的 `@peri-code/ptc@0.2.1` artifact，并以 `node <entry>` 启动；下述 `node --input-type=module --eval` 是问题发现时的历史旧叙述，不再代表当前实现。
- `npm-packages/@peri-ptc/src/adapter.js` 顶层是 ESM，并以 `new Function("tools", "input", "console", ...)` 构造用户函数。
- 该函数可访问 Node globals（例如 `process`、`Buffer`），但 lexical scope 中没有 CommonJS `require`。
- `peri-middlewares/src/ptc/mod.rs` 的 `RunCodeTool::description`、参数 schema 和 prompt contribution 只说“normal Node.js process”与“direct Node.js APIs”，没有说明模块加载必须使用动态 `import()`。

这会诱导模型生成在常规 CommonJS Node.js 中成立、但在 `run_code` 中必然失败的代码。

### C2. JSON-RPC error 的安全分类在 Rust Display 边界丢失

- adapter 捕获错误后发送 `error.message`：`JavaScript execution failed` 或 `JavaScript resource limit exceeded`，以及 `error.data.code`：`TOOL_FAILED` 或 `RESOURCE_LIMIT`。
- `peri-js-runtime/src/rpc.rs::RpcChannel::send_request` 将该结构保存在 `JsRuntimeError::RpcResponse(JsonRpcError)`。
- `peri-js-runtime/src/error.rs` 为 `RpcResponse` 定义的 Display 固定为 `JavaScript RPC request failed`；内部 `JsonRpcError` 未进入可见错误文本。
- `RunCodeTool::invoke` 直接以 `?` 向外传播该错误，因此工具调用面只能得到固定文本。

adapter 有意不回传原始 exception 文本与 stack，这符合秘密保护要求；但稳定、脱敏的 adapter message 和 `data.code` 也被一起抹掉，属于过度降级。

## 影响

- 模型会把 `require` 失败误判为负载、RPC transport 或权限问题，产生无效重试。
- 用户无法判断是代码错误、JSON 序列化错误、资源限制还是协议故障。
- 资源限制缺少明确反馈，调用方无法通过缩小 input/result/logs 自助恢复。
- 工具描述声称的能力大于实际可用语法，降低 `run_code` 的可靠性和可预测性。
- 当前错误脱敏是必要安全边界；修复若直接暴露 exception/stack，可能泄露 source、input、路径或秘密，因此不能简单透传原始异常。

## 修复目标

1. 明确 `RunPtcCode` 的 ESM-only 模块系统契约：tool description、schema 与 prompt contribution 必须要求动态 `await import('node:...')`，并明确 static `import` 与 `require` 不可用。
2. 工具失败至少向调用方保留稳定、脱敏的错误类别：`TOOL_FAILED`、`RESOURCE_LIMIT`、`CANCELLED`、`TIMEOUT`、`PROTOCOL_ERROR`。
3. adapter 资源限制应显示可操作的安全消息，例如 `JavaScript resource limit exceeded`，但不得包含用户 source、input、console 内容、原始 exception message、stack、环境变量或内部 debug chain。
4. 普通用户代码异常应显示安全的 `JavaScript execution failed`；协议故障应与用户代码失败区分。
5. 明确返回值必须 JSON-compatible；文档说明 `undefined`/`NaN`/`Infinity`、`Map`/`Set` 的归一化行为，或在 adapter 中主动拒绝容易误解的值并返回稳定错误。

## 建议实现

### D1. 模块加载契约

优先采用最小变更：保留 ESM-only 启动方式，更新三个现有描述面，明确示例使用动态 import：

```js
const fs = await import('node:fs/promises');
```

若产品要求兼容 `require`，应在 adapter 中显式、安全地注入由 `node:module::createRequire(import.meta.url)` 创建的 `require`，并增加测试；不要依赖偶然的全局作用域行为。

### D2. 安全错误投影

不要修改 `JsonRpcError` 的 redacted `Debug`，也不要直接把任意远端 message/data 原样拼接到错误中。建议在 `peri-js-runtime` 增加窄的 public error 投影：

- 只接受 adapter 定义的 allowlist message/code；
- 未识别值统一降级为 `JavaScript execution failed` / `PROTOCOL_ERROR`；
- `RpcResponse` 的 Display 或 `RunPtcCode` invoke 显示稳定 public message + stable code；
- 原始 exception 与 stack 继续留在 adapter 内且不写日志。

## 验收标准

- [x] `RunPtcCode` 工具描述与实际模块加载语义一致。
- [x] ESM-only 描述中包含可复制的 `await import('node:crypto')` 示例，并明确 static `import` 与 `require` 不可用。
- [ ] 若选择兼容 `require`，`require('node:crypto')` 与动态 import 均有契约测试且成功。（不适用：采用 ESM-only）
- [x] `throw new Error(...)` 返回脱敏的 `TOOL_FAILED` / `JavaScript execution failed`，不包含 sentinel 文本或 stack。
- [x] 语法错误返回同一安全的用户代码失败分类，不泄露 source。
- [x] 超过 result/log 限制返回 `RESOURCE_LIMIT` / `JavaScript resource limit exceeded`。
- [x] wall timeout 继续返回明确 `TIMEOUT`，cancel 继续返回 `CANCELLED`。
- [x] `BigInt` 与循环引用的失败可与 transport/protocol failure 区分。
- [x] 增加跨 `adapter.js → RpcChannel → JsExecutor → RunPtcCode` 的错误投影测试。
- [x] `cargo test -p peri-js-runtime --lib`、`cargo test -p peri-middlewares --lib ptc`、`cargo fmt --check` 与 `git diff --check` 通过。

最终工具错误格式为 `<STABLE_CODE>: <FIXED_SAFE_MESSAGE>`；验证命令包括 adapter test/typecheck、两个目标 Rust test、fmt、clippy 与 diff check。

## 后续强力测试 TODO

- [ ] Live wall timeout：验证 `TIMEOUT`、实际耗时边界与 Node 子进程及时终止。
- [ ] Live cancellation：从 ACP/TUI 取消执行，验证 `CANCELLED`、事件终态与无残留子进程。
- [ ] `tools.*` RPC：覆盖成功、未知工具、内部工具失败与并发调用。
- [ ] 进程压力：连续大量调用、并发 session、Node 崩溃后重新拉起，并观测 FD 与内存增长。
- [ ] 协议破坏：覆盖畸形 NDJSON、异常 stdout、Node 提前退出和 JSON-RPC id 错配。
- [ ] 完整交互链路：覆盖 `peri-tui → peri-acp → peri-agent → SearchExtraTools → ExecuteExtraTool(RunPtcCode)` 的错误事件展示与退出语义。

## 相关事实源

- `npm-packages/@peri-ptc/src/adapter.js`
- `peri-js-runtime/src/executor.rs`
- `peri-js-runtime/src/rpc.rs`
- `peri-js-runtime/src/error.rs`
- `peri-middlewares/src/ptc/mod.rs`
- `docs/code-index/peri-middlewares.md`
- `docs/standards/architecture-contracts.md` 的 `ARC-TOOLS-001`、`ARC-SERIAL-001`
