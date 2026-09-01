# Programmatic Tool Calling 与通用 JavaScript 执行器设计

## 1. 文档地位

> 状态：现行设计

本文定义 Perihelion 的 Programmatic Tool Calling（PTC）现行架构、通用 JavaScript 执行器 seam 与 Workflow 共享边界。实现、测试和后续设计若与本文冲突，应先更新本文并说明契约变化。

PTC 在本仓库中的含义是：提供 canonical deferred-only 工具 `RunPtcCode`；模型先通过 `SearchExtraTools` 发现它，再经 `ExecuteExtraTool` 执行，JavaScript 程序通过 `tools.<ToolName>(input)` 异步调用当前 session-local 工具。PTC 默认装配（`PtcMiddleware=false` 可关闭），不改变现有 direct tools 的可见性。旧名 `run_code` 不可执行、不是 alias，仅作为 ToolSearch 迁移关键词。`RunPtcCode` 外层任意代码执行入口按 Bash 同级审批；内部调用的 policy、HITL、事件与 tool card 均投影到 effective target。模型 assistant raw wrapper call 仅为协议配对而保留。

**生产安全边界**：`RunPtcCode` 启动普通 Node.js 进程，并非 sandbox。`tools.*` 的 Permission/HITL 不约束 Node 原生文件系统、进程、环境变量或网络 API；禁用某个 RPC 工具也不阻止 JavaScript 通过 Node API 实施同类操作。runtime 提供 timeout、资源上限、进程树回收和默认环境变量清理以降低可靠性与秘密泄漏风险；PTC adapter 运行环境仅保留 `PATH`，Windows 额外 allowlist `SystemRoot`、`WINDIR`、`TEMP`、`TMP` 以满足 Node/OS 运行要求，不继承其他调用方环境变量，但仍不提供网络、文件系统或容器隔离。不得在 source、input、console、return、异常或工具参数中包含 secret。

## 2. 目标

1. 新增 deferred-only canonical `RunPtcCode`，经 `SearchExtraTools → ExecuteExtraTool` 发现和执行，不移除、不隐藏现有 direct tools。
2. 支持 JavaScript 中的 `await tools.Bash(...)`、`await tools.Read(...)` 和 `Promise.all(...)`。
3. JavaScript 工具调用通过异步 RPC 请求 Peri 执行；结果返回 Node 后恢复对应 Promise。
4. PTC 与 `ExecuteExtraTool` 复用同一套 effective-tool resolution 和 canonical dispatch，不复制工具执行语义。
5. 工具事实源始终是当前 turn 的 session-local tool view；PTC 不访问静态全局工具表。
6. 从 Workflow 中提炼通用 JavaScript Execution Host；Workflow 与 PTC 是该 host 的两个 Adapter。
7. 保留 Workflow 的现有外部语义、协议不变量、取消行为、journal 和 resume 行为。
8. 内部工具调用继续经过既有 policy、HITL、事件、tool card、取消和结果处理路径，所有投影以 effective target 为准。

## 3. 非目标

首轮实现不要求：

- 只向模型暴露 PTC 元工具而隐藏既有 direct tools；
- 引入 `Native / Ptc / Both` 模式；
- 将 PTC 建模为 Workflow；
- 让 JavaScript 通过 `agent()` 间接完成工具调用；
- 事务化或回滚已经发生的工具副作用；
- 一次性重构所有工具输出为结构化 JSON；
- 完成容器、Worker Thread 安全边界、网络隔离或模块 allowlist；
- 为内部工具调用新增一套平行 permission 或 event 系统。

## 4. 核心判断

### 4.1 PTC 是 deferred 附加能力

`RunPtcCode` 是 canonical deferred-only tool。模型通过 `SearchExtraTools` 发现，再由 `ExecuteExtraTool` 执行；`Read`、`Bash` 等既有 direct tools 仍可直接调用。

```text
LLM-visible tools = existing direct tools + SearchExtraTools + ExecuteExtraTool
PTC execution = SearchExtraTools("RunPtcCode" | "run_code") → ExecuteExtraTool(RunPtcCode)
```

PTC 不改变 `BaseTool::is_direct()` 的定义。`RunPtcCode.is_direct() = false`；旧 `run_code` 仅参与搜索迁移，不解析为可执行 alias。

### 4.2 `tools.<name>` 是 RPC 源语

JavaScript 中的：

```js
const output = await tools.Bash({
  command: "cargo test -p peri-workflow --lib",
  timeout: 300000,
  run_in_background: false,
});
```

不是 Node 内的 Bash 实现，而是动态 RPC stub：

```text
JS Promise
  → tool/call { invocationId, toolName, input }
  → Peri effective-tool dispatch
  → tool result/error
  → JSON-RPC response
  → Promise resolve/reject
```

多个 pending request 可以并存，因此 `Promise.all` 自然映射为多个并发工具调用。并发限制仍由 Peri 工具执行层决定，而不是由 PTC 绕过。

### 4.3 复用 dispatcher，不嵌套调用 wrapper tool

PTC 不另行嵌套调用第二次 `ExecuteExtraTool::invoke`。模型侧的 canonical 入口本来就是 `SearchExtraTools → ExecuteExtraTool(RunPtcCode)`；进入 `RunPtcCode` 后，JavaScript 的 `tools.*` 直接复用 `ExecuteExtraTool` 背后的 effective-tool resolution 与 canonical dispatch。

目标结构：

```text
SearchExtraTools("RunPtcCode" | "run_code")
  → ExecuteExtraTool(RunPtcCode)
  → JavaScript tools.Bash(...)
  → Shared Effective Tool Dispatcher(Bash)
```

禁止形成：

```text
ExecuteExtraTool(RunPtcCode) → ExecuteExtraTool(Bash) → Bash
```

应形成：

```text
ExecuteExtraTool(RunPtcCode) → RunPtcCode → Shared Effective Tool Dispatcher(Bash)
```

共享 dispatcher 必须：

- 从当前 session-local tool view 解析 effective tool name；
- 应用当前 agent 的 allowlist/disallowlist 和 middleware disabled 状态；
- 进入现有 permission/HITL、执行、事件和结果处理路径；
- 接收 cancellation 和 invocation identity；
- 返回可投递到 RPC peer 的规范结果或结构化错误。

## 5. 目标架构

```text
                    ┌──────────────────────────────┐
                    │ JavaScript Execution Host    │
                    │                              │
                    │ Node lifecycle               │
                    │ NDJSON JSON-RPC              │
                    │ bidirectional requests       │
                    │ pending map / cancellation   │
                    │ stdout / stderr / completion │
                    └──────────────┬───────────────┘
                                   │
                 ┌─────────────────┴─────────────────┐
                 │                                   │
       ┌─────────▼──────────┐              ┌─────────▼──────────┐
       │ Workflow Adapter   │              │ PTC Adapter        │
       │                    │              │                    │
       │ workflow/start     │              │ deferred RunPtcCode  │
       │ agent/run          │              │ via ExecuteExtraTool │
       │ progress/event     │              │ tools.<name>() RPC   │
       │ journal / resume   │              │ logs / return        │
       └────────────────────┘              └─────────┬──────────┘
                                                    │
                                          ┌─────────▼──────────┐
                                          │ Effective Tool     │
                                          │ Dispatcher         │
                                          │ session-local view │
                                          └────────────────────┘
```

### 5.1 JavaScript Execution Host

该 Module 的 interface 应保持很小，隐藏所有进程和 RPC 细节。概念接口如下，具体类型名以实现邻域为准：

```rust
pub struct JsExecutionRequest {
    pub source: String,
    pub input: serde_json::Value,
    pub router: Arc<dyn JsRpcRouter>,
    pub cancellation: CancellationToken,
}

pub struct JsExecutionResult {
    pub value: Option<serde_json::Value>,
    pub logs: Vec<String>,
}

#[async_trait]
pub trait JsExecutor {
    async fn execute(
        &self,
        request: JsExecutionRequest,
    ) -> Result<JsExecutionResult, JsExecutionError>;
}
```

Interface 的精确形态可调整，但 host 必须隐藏：

- Node 进程启动与退出；
- stdin/stdout/stderr 消费；
- NDJSON framing 和 flush；
- JSON-RPC request ID；
- 双向 pending request map；
- handshake、build identity 和协议版本；
- process exit 时 pending drain；
- cancellation/kill；
- malformed frame 与 protocol error；
- 最终 completion 唯一性。

Host 不应知道 `agent/run`、`tool/call`、workflow journal 或 PTC SDK 的业务含义。

### 5.2 Workflow Adapter

Workflow Adapter 保留：

- `workflow/start`、`workflow/done`、`workflow/kill`；
- `agent/run`；
- phase/task/progress；
- run ID、journal、resume 和 `state.json`；
- fire-and-forget `WorkflowTool` 语义；
- background completion notification。

抽取后必须保持 `ARC-WORKFLOW-RPC-001` 的所有不变量和现有 wire compatibility。

### 5.3 PTC Adapter

PTC Adapter 负责：

- `RunPtcCode` 输入与输出；
- 模型 prompt 中的 JavaScript 使用说明；
- 从 session-local tool view 生成稳定排序的工具目录；
- Node 侧 `tools` Proxy 或等价 SDK；
- `tool/call` 与可选的 `tool/cancel`；
- 内部 invocation ID 与外层 `RunPtcCode` 的关联；
- 工具结果到 JavaScript value 的转换；
- JavaScript logs、return value 和 error 到工具结果的映射。

首版允许 `tools.<name>()` 统一返回字符串，以匹配当前 `BaseTool::invoke()` 的主要结果形态。结构化 canonical value 是后续增强，不作为首版前置重构。

### 5.4 PTC artifact、启动与 handshake

生产 artifact 固定为 `@peri-code/ptc@0.2.3`。Rust runtime 在版本缓存缺失或无效时，以受控最小环境执行固定版本 npm install；`npm-packages/@peri-ptc` 的 `dist` 仅由 package 的 `bun run build`/发布流程生成，不由 Cargo 构建生成，也不作为 Rust 内嵌 artifact 跟踪。package version、build ID、protocol version、Rust 常量和已发布 npm artifact 必须作为一个原子版本面同步。

运行时将通过 identity 校验的 package 缓存在 `~/.peri/ptc/0.2.3`，并直接执行校验后的 `node <entry>`，不得使用 `--eval`/`eval`。缓存缺失或无效时，在跨进程锁保护下安装到 staging，完整校验后原子 rename；Node adapter 必须在读取或执行 source 前完成 `ptc/start` handshake，并同时校验 protocol version 与 build identity，任何缺失或不匹配都 fail closed。

默认路径允许对固定版本 `@peri-code/ptc@0.2.3` 执行受控 npm install。只有固定版本安装失败且调用方显式设置 `PERI_PTC_ALLOW_NPX_FALLBACK=1` 时，才允许精确版本 `npx` fallback；安装与 fallback 都必须使用 private `HOME`/npm cache 和最小环境变量。fallback 开关代表调用方主动接受额外的解析链路风险，不能退化为非精确版本。

### 5.5 Effective Tool Dispatcher

优先从现有 ToolSearch/`ExecuteExtraTool` 路径提炼共享执行 seam，而不是增加第二套 dispatcher。其 interface 至少表达：

```rust
pub struct EffectiveToolInvocation {
    pub invocation_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
    pub parent_invocation_id: Option<String>,
}
```

Dispatcher 的实现必须绑定当前 turn 的 session-local tool view，遵守 `ARC-TOOLS-001`。它不能使用静态核心工具清单，也不能因调用来自 JavaScript 而跳过 effective tool name 的审批。

## 6. Wire 协议

### 6.1 Node → Rust

首版新增：

```text
tool/call
tool/cancel   # 可与外层取消一起落地，不能留下不可取消的 pending 调用
```

建议参数：

```ts
interface ToolCallParams {
  invocationId: string;
  toolName: string;
  input: unknown;
}

interface ToolCancelParams {
  invocationId: string;
}
```

### 6.2 Rust → Node

正常调用使用 JSON-RPC result；失败使用可稳定匹配的结构化 error code：

```ts
type ToolCallErrorCode =
  | "UNKNOWN_TOOL"
  | "INVALID_INPUT"
  | "PERMISSION_DENIED"
  | "USER_REJECTED"
  | "CANCELLED"
  | "TIMEOUT"
  | "TOOL_FAILED";
```

错误消息不得包含 secret、环境变量、内部 debug dump 或无必要的 stderr/backtrace。

### 6.3 JavaScript interface

Node runtime 向用户代码注入 `tools`：

```ts
declare const tools: {
  [toolName: string]: (input: unknown) => Promise<string>;
};
```

实现可以使用 `Proxy`：

```js
const tools = new Proxy({}, {
  get(_target, toolName) {
    return (input) => rpc.request("tool/call", {
      invocationId: nextInvocationId(),
      toolName: String(toolName),
      input,
    });
  },
});
```

JavaScript 执行环境为 ESM-only。Node module 只能在函数体内使用动态 `await import('node:...')`；static `import` 语句与 CommonJS `require` 均不可用。

## 7. 执行语义

### 7.1 同步外层工具调用

`RunPtcCode` 经 `ExecuteExtraTool` 对模型表现为一次同步 deferred 工具调用，并等待 JavaScript：

1. 启动；
2. 完成所有 awaited host requests；
3. 返回顶层 value/logs；或
4. 失败/取消。

不得复用 WorkflowTool 的“立即返回 run_id、后台继续”语义。

### 7.2 并发

每个 `tools.<name>()` 对应独立 invocation ID 和 pending Promise。Host 必须支持并发请求；Peri dispatcher 决定实际并发和工具级限制。单 invocation 的 `tool/call` 注册与先到达的 `tool/cancel` 消费必须在同一状态临界区完成；未知或迟到 cancel 的预取消状态必须有界，不能随 session 生命周期无限增长。

### 7.3 取消

外层 `RunPtcCode` 取消必须：

1. 标记外层执行取消；
2. 取消仍在执行的内部工具 invocation；
3. reject Node 中对应 pending Promise；
4. 终止 JavaScript execution；
5. drain 双向 RPC pending map；
6. 忽略迟到结果；
7. 保持终态唯一；当 execution completion 与外层 cancellation 同时可观察时，外层 cancellation 优先。

Workflow 现有 token ownership、先注册后 spawn、kill 后禁止成功响应等模式应成为通用 host 的回归契约。

### 7.4 副作用

已经完成的工具调用不会因后续 JavaScript 异常而回滚。PTC 不提供事务语义。

## 8. 事件与权限

PTC 内部调用必须沿用 canonical 事件链，不得建立 Node → TUI 私有通道。`RunPtcCode` 与内部 effective tool invocation 应具有可关联 identity。policy、HITL、事件和 tool card 必须以 effective target 投影；模型 assistant raw wrapper call 只保留协议配对，不得作为执行、审批、事件或展示目标。

权限按 effective target 判断。例如 `tools.Bash(...)` 的敏感操作、HITL、事件和 tool card 目标都是 `Bash`，不能投影为 `RunPtcCode` 或 `ExecuteExtraTool`。`PermissionMiddleware` 和 `HumanInTheLoopMiddleware` 的独立装配语义保持不变。

适用契约：

- `ARC-TOOLS-001`：session-local 工具视图是能力事实源；
- `ARC-HITL-001`：审批与提问能力独立，包装调用不得旁路；
- `ARC-EVENT-001`：事件只走 Agent → ACP → client canonical 链；
- `ARC-CANCEL-001`：取消 identity、透传与 Agent 终态语义；
- `ARC-WORKFLOW-RPC-001`：抽取 Workflow transport 时必须保留 RPC 不变量。

## 9. 代码组织

目标状态建议新增独立 crate `peri-js-runtime`，因为 Workflow 和 PTC 是两个真实 Adapter，且让 PTC 依赖名为 `peri-workflow` 的通用 runtime 会制造错误依赖语义。

建议结构：

```text
peri-js-runtime/
  src/
    executor.rs
    process.rs
    rpc.rs
    protocol.rs
    error.rs

peri-workflow/
  src/
    runner.rs          # Workflow Adapter
    protocol.rs
    tool.rs

peri-middlewares/src/ptc/
  mod.rs
  tool.rs
  router.rs
  prompt.rs

npm-packages/
  @peri-js-runtime/    # 通用 Node RPC/runtime
  @peri-workflow/      # Workflow Adapter
  @peri-ptc/           # tools Proxy 与 PTC completion
```

实现可以先在现有 crate 内做行为保持的提炼，再迁移到新 crate；最终依赖方向必须清晰，避免循环依赖。新增 crate 前必须以根 `Cargo.toml` 和邻近 crate manifest 为事实源。


## 10. 首个端到端场景

```js
const manifests = await tools.Glob({
  path: ".",
  pattern: "**/Cargo.toml",
});

const contents = await Promise.all(
  manifests.map((file) => tools.Read({ file_path: file })),
);

return {
  count: contents.length,
  totalBytes: contents.reduce((sum, value) => sum + value.length, 0),
};
```

该场景必须验证：

1. `RunPtcCode` 不直接出现在 LLM tools，须经 `SearchExtraTools → ExecuteExtraTool`；现有 direct tools 同时可见且行为不变；
2. `Glob`/`Read` 来自当前 session-local view；
3. Node 发出多个异步 `tool/call`；
4. Rust 完成工具调用后按 invocation ID 投递响应；
5. JavaScript 从 await 点继续执行并返回最终 value；
6. 取消时所有 pending 调用与 Node execution 一起结束；
7. Workflow 原有测试不受影响。
