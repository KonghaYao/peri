# Workflow 系统设计

**状态**：现行设计

## 1. 概述

Workflow 系统是 Peri 的多 Agent 编排子系统，允许用户通过 JavaScript ESM 脚本定义并行、流水线、分阶段的多 Agent 执行计划。脚本在独立的 Node.js 进程中运行，通过双向 JSON-RPC 与 Peri 主进程通信。

### 1.1 核心原语

| 原语 | 语义 | 示例 |
|------|------|------|
| `agent(prompt, opts)` | 调度一个 SubAgent 执行 | `agent("review this file", { allowedTools: ["Read"] })` |
| `parallel(tasks)` | 并发执行所有任务 | `parallel([() => agent("A"), () => agent("B")])` |
| `pipeline(items, fn)` | 顺序处理每个元素 | `pipeline(files, f => agent(\`read \${f}\`))` |
| `phase(name)` | 标记阶段边界 | `phase("Review")` |
| `log(msg)` | 记录阶段日志 | `log("starting analysis...")` |
| `workflow(meta, fn)` | 定义工作流入口（默认导出） | `export default workflow({ name: "scan" }, ...)` |

### 1.2 设计目标

- **异步 fire-and-forget**：LLM 调用工具后立即返回，workflow 后台运行，完成后通知 agent
- **SubAgent 复用**：workflow 内部 agent 复用 Peri 现有 SubAgent 基础设施（中间件链、LLM 管理、工具系统）
- **零代码冗余**：不引入新框架，复用 `workflow-engine` 作为脚本执行引擎
- **可观察性**：journal 持久化、progress store、TUI 面板实时展示

---

## 2. 架构总览

```
┌──────────────────────────────────────────────────────────────────────┐
│  peri-tui (ratatui 终端界面)                                          │
│  ├─ WorkflowPanel        三级树实时展示 (run/phase/agent)              │
│  ├─ WorkflowSnapshot     WORKFLOW_SNAPSHOT atom (2s ACP 轮询)            │
│  ├─ /workflows 命令      PanelKind::Workflow (panel_registry.rs)      │
│  └─ bg-task-completed unstable event → 通知条渲染                      │
├──────────────────────────────────────────────────────────────────────┤
│  peri-agent (Agent 执行层)                                             │
│  │  agent/workflow/agent.rs    WorkflowAgentExecutor 执行体 (SubAgent) │
│  │  session/exec/executor.rs   通知消费者 (registry.complete →         │
│  │                             BgRegistryEvent::Completed →            │
│  │                             bg-task-completed event + Defer 注入)   │
├──────────────────────────────────────────────────────────────────────┤
│  peri-acp (ACP 服务层 / 装配面薄壳)                                     │
│  │  host/workflow_agent.rs     create_session_workflow_middleware()    │
│  │                             装配 session 级 WorkflowMiddleware      │
│  │                             (经 WorkflowMiddlewarePort 端口注入)     │
│  │  WorkflowMiddlewarePort     peri-acp-types/src/ports.rs (契约端口)   │
│  │  WorkflowTaskResult         peri-acp-types/src/workflow.rs           │
├──────────────────────────────────────────────────────────────────────┤
│  peri-middlewares (中间件层)                                           │
│  │  workflow/mod.rs           WorkflowMiddleware 具体实现 (聚合容器)     │
│  │                            持有 Runner/Registry/Progress/Journal 等 │
│  │                            所有 workflow 状态)                      │
│  │  WorkflowMiddlewareAdaptor  per-turn 中间件适配器 (collect_tools)    │
├──────────────────────────────────────────────────────────────────────┤
│  peri-workflow (Rust crate)                                            │
│  ├─ protocol.rs   JSON-RPC 协议类型 (请求/响应/通知)                    │
│  ├─ progress.rs   ProgressStore (内存 reducer)                         │
│  ├─ journal.rs    JournalStore (磁盘 append-only)                      │
│  ├─ registry.rs   WorkflowTaskRegistry (广播 + 并发限流)               │
│  ├─ runner.rs     WorkflowRunner (子进程 spawn/kill)                   │
│  ├─ rpc.rs        RpcChannel (双向 JSON-RPC, pending 追踪)             │
│  └─ tool.rs       WorkflowTool (deferred tool)                         │
├──────────────────────────────────────────────────────────────────────┤
│  @peri-code/workflow (npm 包, Node.js 进程)                                 │
│  ├─ runner.js     RPC handler + AgentAdapter + WorkflowPorts 实现      │
│  └─ 依赖 @claude-code-best/workflow-engine (零修改复用)                │
│                                                                        │
│  用户脚本 (JavaScript ESM)                                             │
│  └─ workflow(meta, fn) → agent/parallel/pipeline/phase/log            │
└──────────────────────────────────────────────────────────────────────┘
```

**端口归属**：session 级 workflow 状态由 `WorkflowMiddleware`（peri-middlewares）持有，经 `WorkflowMiddlewarePort`（`peri-acp-types/src/ports.rs`）注入 ACP；`peri-acp/src/host/workflow_agent.rs` 是装配宿主，`WorkflowAgentExecutor` 与通知消费者归 Agent 执行层。

**通信协议**：Rust ←→ Node 通过 stdin/stdout 双向 JSON-RPC 2.0（NDJSON 格式）。Node 侧 RPC server，Rust 侧 RPC client。

---

## 3. 核心组件设计

### 3.1 RPC 协议

在 `peri-workflow/src/protocol.rs` 定义所有协议类型。采用 JSON-RPC 2.0，每行一条完整 JSON（NDJSON），基于 `serde_json::Value` 编码。

**请求消息**（Rust → Node）：

| 方法 | 用途 | 关键参数 |
|------|------|----------|
| `workflow/start` | 启动脚本执行 | runId, script, args, maxConcurrency, resume, cwd, budgetTotal |
| `workflow/kill` | 终止执行 | runId |

**请求消息**（Node → Rust）：

| 方法 | 用途 | 关键参数 |
|------|------|----------|
| `agent/run` | 调度 agent 执行 | runId, agentId, prompt, schema, model, allowedTools, maxTokens, agentType, isolation, label, phase |

**通知消息**（Node → Rust，携带 RPC id 但无 response）：

| 方法 | 用途 |
|------|------|
| `progress/event` | 进度事件流 (8 种变体) |
| `journal/append` | 增量写入 agent 日志 |
| `journal/truncate` | 截断日志（resume 时） |
| `log` | 运行日志（含 level 字段，映射到 tracing 级别） |

**响应消息**（Rust → Node）：

`AgentRunResult`（enum）：`Ok { output, usage, model, tool_count, token_count }` | `Skipped` | `Dead { reason, detail }`，其中 `Usage { output_tokens }`。

**完成消息**（Node → Rust 单向）：

| 方法 | 用途 | 关键参数 |
|------|------|----------|
| `workflow/done` | 执行完成通知 | status, returnValue, error |

**类型约定**：全部 `#[serde(rename_all = "camelCase")]`，确保 Rust struct 字段（snake_case）与 Node JSON（camelCase）互转兼容。

### 3.2 WorkflowRunner

`peri-workflow/src/runner.rs` — 管理 Node.js 子进程生命周期。

```rust
pub struct WorkflowRunner {
    agent_executor: Arc<dyn AgentExecutor>,              // 回调 → WorkflowAgentExecutor
    active_channels: DashMap<String, Arc<RpcChannel>>,   // run_id → channel (单 agent kill)
    cwd: String,
}
```

**执行流程**（`run()` 方法）：

1. 生成 `run_id` (UUID v7)
2. `journal_store.init_run(run_id)` — 创建 `.claude/workflow-runs/{run_id}/script.js`
3. 检测 bun 环境 → 优先 `bunx @peri-code/workflow@<version>`，否则 `npx -y @peri-code/workflow@<version>`（npx 兜底带显式版本，避免全局旧版被静默复用）
4. 启动子进程，继承 cwd 和 PATH
5. 创建 `RpcChannel`（绑定 child stdin/stdout，启动 `spawn_stdout_reader()` 线程）
6. `send_request("workflow/start")`，**15 秒超时** — Node runner.js 开始执行
7. **消息循环**（`tokio::spawn` 内 `while let Some(msg) = msg_rx.recv().await`，外层 `select!` 竞速 `kill_rx` 和消息循环 join_handle）：
   - `agent/run` → `spawn_agent_execution()` → 异步执行 → 发送 response
   - `progress/event` → `progress_store.apply_event(event)`
   - `journal/append` → `journal_store.append(entry)`
   - `journal/truncate` → `journal_store.truncate(run_id)`
   - `log` → 按 level 字段映射：`error`/`warn` → `warn!`，`info` → `info!`，其他 → `debug!`
   - `workflow/done` → **break** 退出循环 → 收集 stderr_tail（最后 20 行）→ 写入 `state.json`（含 finished_at 时间戳）→ `done_tx.send(WorkflowResult)`（含 stderr_tail）
   - `kill_rx` 信号 → 5s 超时发送 `workflow/kill` RPC → `child.kill()` → `msg_loop.abort()`（防止覆写 state.json）→ 写入 `killed` state.json → `done_tx.send(WorkflowResult)`
8. 清理：`child.kill().await`（防止僵尸进程）、移除 `active_channels`、`cleanup_old_runs()`、`progress_store.cleanup_completed()`

**Agent 执行 spawn**：每个 `agent/run` 请求触发 `tokio::spawn`，通过 `AgentExecutor` trait（`peri-acp-types/src/workflow.rs`，契约层）回调到 `peri-agent` 的 `WorkflowAgentExecutor`。RpcChannel 通过 `pending_agents` 追踪所有活跃 agent（用于单 agent kill 查找）。

**WorkflowResult 结构体**（`runner.rs`）：
```rust
pub struct WorkflowResult {
    pub run_id: String,
    pub status: String,
    pub return_value: Option<Value>,
    pub error: Option<String>,
    pub stderr_tail: Option<String>,  // Node 进程 stderr 最后 20 行，用于诊断快速失败
}
```
`stderr_tail` 在消息循环正常退出和 kill 路径中均有收集（stderr_lines 缓冲区 + `.rev().take(20)`），仅在 status 为 `"failed"` 或 `"killed"` 时可能有值。用于 `tool.rs` 的快速失败检测中向 LLM 报告脚本加载/沙箱错误。

### 3.3 RpcChannel — 双向 RPC 通道

`peri-workflow/src/rpc.rs` 实现 Rust ←→ Node 双向 JSON-RPC。

**核心数据结构**：

```rust
pub struct RpcChannel {
    stdin: Mutex<ChildStdin>,
    pending_requests: DashMap<u64, oneshot::Sender<Result<Value, JsonRpcError>>>,  // 等待响应的请求
    pending_agents: DashMap<(String, u64), PendingAgent>,  // (run_id, agent_id) → agent 追踪
    next_id: AtomicU64,
}
```

**辅助类型**：`IncomingMessage { id, method, params }` 枚举（Request/Response/Notification），`parse_message()` 纯函数解析 NDJSON 行，`handle_incoming()` 按 type 路由到 `pending_requests` 或入站处理器。

**关键方法**：`send_request()`, `send_response()`, `send_notification()`, `send_error()`, `register_agent()`, `deregister_agent()`, `kill_agent()`, `drain_pending()`.

**请求-响应匹配**：`send_request()` 以 `next_id` 作为 JSON-RPC id，`pending_requests.insert(id, tx)` 保存 oneshot sender。`stdout_reader` 线程收到 response 时按 id 查找 sender → `oneshot.send()`。

**竞态防护**（GAP-11）：先 `pending_requests.insert()` 再 `stdin.write_line()`。若 Node 响应极快，response 已在 pending_requests 中 → 匹配成功。反序（先 write 后 insert）会导致"response 到达但无 sender"的竞态。

**agent 追踪**：`pending_agents: DashMap<(String, u64), PendingAgent{rpc_id, cancel_tx}>`。spawn 前 `register_agent()` 插入，完成后 `deregister_agent()` 移除。`kill_agent()` 通过 `cancel_tx.send()` + `send_error(-32000)` 双重终止。

**错误传播**：
- stdout 关闭 → `drain_pending()` → 所有 `pending_requests` 收到 "node process exited" 错误
- JSON 解析失败 → `tracing::warn!` 跳过该行，不阻塞后续消息
- stdin write 失败 → 返回 `WorkflowError::Io`

### 3.4 WorkflowTaskRegistry — 运行管理

`peri-workflow/src/registry.rs` — 全局运行注册表。

```rust
pub struct WorkflowTaskRegistry {
    runs: Mutex<HashMap<String, WorkflowRun>>,
    notification_tx: broadcast::Sender<WorkflowTaskResult>,
    max_concurrent: usize,  // 默认 3
}

pub struct WorkflowRun {
    run_id: String,
    workflow_name: String,
    script_preview: String,
    status: WorkflowRunStatus,                // Running | Completed | Failed | Killed
    started_at: Instant,
    child_handle: JoinHandle<()>,
    kill_tx: Option<oneshot::Sender<()>>,     // None if run already killed/completed
}

pub enum WorkflowRunStatus { Running, Completed, Failed, Killed }
pub enum RegistryError { ConcurrentLimit, NotFound }
```

**核心方法**：

| 方法 | 功能 |
|------|------|
| `register(run)` | 并发限流检查（`active_count() < max_concurrent`）→ 插入 runs map |
| `complete(run_id, result)` | 更新 run.status → 通过 broadcast channel 发送 WorkflowTaskResult |
| `kill(run_id)` | 触发 `kill_tx.send()` + `runs.remove()`（两层清理）。`child_handle.abort()` 已被有意移除，由 `runner.rs` 的 kill 分支统一处理清理 |
| `list_runs()` | 返回运行中任务列表 |
| `active_count()` | 返回当前活跃 run 数 |

**完成通知**：`tool.rs` 的 notification_task 在 done_rx 收到后，从 `ProgressStore::get_run_stats()` 读取 `agent_count` 和 `tool_calls_count`，连同 `status`/`duration_ms`/`error` 构造 `WorkflowTaskResult`，调用 `registry.complete()` **仅做广播**（不读 ProgressStore，不移除 run——history 保留供调试）。`WorkflowTaskResult::to_notification()` 方法（`peri-acp-types/src/workflow.rs`，契约层）格式化 `<system-reminder>` 文本块。

**广播消费**：broadcast receiver 在 Agent session 执行层由单一 session 级 consumer 消费，避免重复通知；具体入口以 `docs/code-index/peri-agent.md` 为准。

### 3.4a WorkflowMiddlewareAdaptor — 中间件链适配器

`peri-middlewares/src/workflow/mod.rs` — Per-turn 中间件适配器，将 session 级 `WorkflowMiddleware` 接入中间件链。

```rust
pub struct WorkflowMiddlewareAdaptor {
    inner: Arc<WorkflowMiddleware>,
}

impl Middleware for WorkflowMiddlewareAdaptor {
    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(self.inner.create_tool())]
    }
    // before_agent 为空实现
}
```

**适配器模式**：`builder.rs` 每轮创建此适配器（持有 `Arc<WorkflowMiddleware>`），通过 `collect_tools()` 让 executor 自动收集 `WorkflowTool` 到 `shared_tools`。`create_tool()` 内部根据 `bg_registry` 是否注入来决定是否附加 `BgTaskRegistry`。

### 3.5 ProgressStore — 内存进度状态

`peri-workflow/src/progress.rs` — 基于 reducer 模式的内存状态机。

```rust
pub struct WorkflowProgressStore {
    runs: RwLock<HashMap<String, RunProgress>>,
}

pub struct RunProgress {
    run_id: String,
    workflow_name: String,
    status: RunStatus,        // Running | Completed | Failed | Killed
    meta: Option<WorkflowMeta>,  // 结构化类型 { name, description?, phases[] }，当前 run_started 处理时始终设为 None——转换未实现
    phases: Vec<PhaseProgress>,
    #[serde(with = "agents_as_map")]
    agents: IndexMap<u64, AgentProgress>,  // serde 序列化为 JSON 数组格式，O(1) 按 agent_id 查找
    #[serde(skip)]
    completed_at: Option<Instant>,         // 完成时间戳，仅用于 cleanup_completed() 时间判断，不序列化
}

pub struct WorkflowMeta {
    name: String,
    description: Option<String>,
    phases: Vec<MetaPhase>,
}

pub struct AgentProgress {
    agent_id: u64,
    label: Option<String>,
    phase: Option<String>,
    status: AgentStatus,      // Pending | Running | Done | Dead | Skipped
    token_count: Option<u64>,
    tool_count: Option<u64>,
    result: Option<AgentRunResult>,
}
```

**事件处理**：`apply_event(event: ProgressEvent)` 根据事件类型更新 `RunProgress`：

| 事件 | 处理 |
|------|------|
| `run_started` | 创建 RunProgress（`meta` 始终设为 None——`WorkflowMeta` 转换未实现） |
| `phase_started/done` | 创建/更新阶段 |
| `agent_started` | 创建 AgentProgress（**Pending** 状态）→ 然后设为 **Running** |
| `agent_progress` | 更新 token_count/tool_count |
| `agent_done` | 更新 status + result + tool_count（从 result 提取） |
| `log` | 无需存储（tracing 转日志） |
| `run_done` | 更新 RunStatus |

**额外方法**（未在设计初期体现）：
- `get_all_runs_snapshot()` — ACP 序列化用
- `active_runs()` — 仅返回活跃 runs
- `get_run_stats(run_id) -> Option<(usize, usize)>` — 返回 `(agent_count, tool_calls_count)`，避免 clone 整个 RunProgress。被 `tool.rs` 和 `workflow/mod.rs` 用于构造 `WorkflowTaskResult`
- `cleanup_completed()` — 清理已完成 runs。使用 `completed_at` 时间戳，保留完成状态 runs **5 分钟**（`COMPLETED_RETENTION = 300s`）后清理，与 journal 的 `KEEP_MAX_RUNS=50` 磁盘策略独立

**关键修复**：`agent_done` 处理中，`tool_count` 从 `AgentRunResult::Ok{ tool_count }` 提取到 `AgentProgress.tool_count`，采用 `.or(agent.tool_count)` 语义——若 result 中有值则更新，否则保留 AgentProgress 事件已设的值。

**并发安全**：`RwLock<HashMap<>>`。`set_or_update_agent()` 按 `agent_id` 精确匹配（非 LIFO），避免并发 agent 事件交错时的索引漂移竞态。

### 3.6 WorkflowJournalStore — 磁盘持久化

`peri-workflow/src/journal.rs` — 以 `.claude/workflow-runs/` 为根目录的结构化存储。

**目录结构**：
```
.claude/workflow-runs/
└── {run_id}/
    ├── script.js          — 用户脚本副本
    ├── journal.jsonl      — append-only agent 日志
    ├── state.json         — 终态快照（原子写入）
    └── state.json.tmp     — 原子写入临时文件
```

**写入策略**：
- `journal.jsonl`：**append-only**，每行一条 `JournalEntry { key, seq, result }`。`key` 是 agent 参数的 SHA256 哈希（用于 resume cache-hit），`seq` 是顺序号
- `state.json`：**原子写入**——先写 `.tmp`，再 `rename`，确保崩溃不产生损坏文件

**保留策略**：`cleanup_old_runs()` 按 mtime 保留最新 `KEEP_MAX_RUNS=50` 个目录，定期在 `runner.run()` 结束时执行。

**额外方法**：`list_runs()` — 列出现有 `state.json` 的运行 ID；`read_state(run_id)` — 读取并解析 `state.json`；`run_dir(run_id)` — 获取运行目录路径。

### 3.7 WorkflowAgentExecutor — SubAgent 复用

`peri-agent/src/agent/workflow/agent.rs` — workflow 内部 agent 的构建器；`peri-acp/src/host/workflow_agent.rs` 只保留装配薄壳，经 `WorkflowMiddlewareFactory` 端口注入中间件链、工具与 resolver。

**核心职责**：将 `AgentRunParams`（来自 Node 的 `agent/run` RPC）转换为完整的 ReActAgent 执行。

**构建链**（相对于 Main Agent 的差异）：

| 方面 | Main Agent | Workflow Agent |
|------|-----------|----------------|
| 中间件链 | 主 session production chain | 专用 Workflow Agent chain；不按主链数量推断能力 |
| Frozen data | 完整 | **透传**（从 session frozen） |
| System prompt | 冻结 base + request-time contribution | 继承父 session frozen base，并按 Workflow Agent chain 应用能力闭包 |
| LLM Model | 用户选择 | 跟随 session provider（`ctx.provider.clone().into_model()`，无 Anthropic 回退） |
| max_iterations | 500 | 200 |
| HITL | 完整 | 共享 session 权限模式 |
| Langfuse | 完整 | 启用 |

**透传的 Workflow Agent 上下文**：
- CLAUDE.md 内容 / Skills 摘要 / System prompt（完整 frozen）
- session_id / compact_config / cancel token
- broker / permission_mode（HITL 共享）
- AgentPool（LLM 实例缓存池）
- Langfuse session / tracer
- ThreadStore（持久化）

**条件注册**：
- `CompactMiddleware`：**已移除**。Workflow agent 的自动 compact 由 v2 `stages/compact.rs` 统一接管（`run_react_loop` 在每轮开头调 `compact_v2::run_compact`）
- `SkillPreloadMiddleware`：允许预加载
- `GitAttributionMiddleware`：跟随 git 贡献格式

**AgentPool 复用**：LLM 实例通过 `AgentPool` 按 `(provider, model_type)` 缓存，避免重复创建 `reqwest::Client` 和昂贵初始化。

### 3.8 WorkflowTool — LLM 可见入口

`peri-workflow/src/tool.rs` — 实现 `BaseTool` trait，作为 **deferred tool** 注册（需 `SearchExtraTools` → `ExecuteExtraTool` 发现，LLM 不可见）。

```rust
fn name() -> "Workflow"
fn description() -> "Launch a workflow with multiple agents working in parallel or pipeline..."
fn parameters() -> JSON Schema { script, scriptPath, name, args, maxConcurrency, resumeFromRunId }
// script 或 scriptPath 二选一（JSON Schema 无法表达 OR，required: []）
```

**invoke() 执行流程**：

1. 解析参数：`script` 或 `scriptPath`（二选一），`name`（显示名，可选），`args`, `maxConcurrency`, `resumeFromRunId`
2. `extract_workflow_name(script)` — 启发式从脚本中提取 `name:` 字段（可选）
3. 生成 `run_id` (UUID v7)
4. `registry.register(run, ...)` — 并发限流检查
5. `tokio::spawn(runner.run())` — 后台启动执行（watch channel: done_tx/done_rx + kill_tx/kill_rx）
6. `bg_registry.register_workflow()` — 注册到统一后台任务系统（BackgroundTaskRegistry），可选步骤
7. **快速失败检测**（1s timeout）：clone watch channel `fast_rx` + `tokio::select!` + `sleep(1s)`，在 spawn 后 1 秒内检测 workflow 是否快速失败（如 Node 二进制不存在、脚本语法错误）。快速失败时：
   - **同步**向 LLM 返回 `Err`（含 `stderr_tail` 等诊断信息）；
   - **仅**调用 `registry.complete()` 广播失败态 `WorkflowTaskResult`，**不在** `WorkflowTool` 内调用 `TaskManager::complete()`（#117：须在 consumer `push_defer` 之后再递减 `active_count`）；
   - BgTaskArea（Path A）与 Defer（Path B）由 session consumer 在 Defer 入队**之后**写入 `BackgroundTaskResult`（§4.2）。
8. `tokio::spawn(notification_task(receiver))` — 等待 done_rx；完成后**只**调用 `registry.complete()`（**不**在 notification task 内 `TaskManager::complete()`；bg 终态由 consumer 在 Defer 之后处理，与慢路径一致）
9. **立即返回**（多行格式）：
   ```
   Workflow 'xxx' started.
   run_id: {uuid}

   The workflow is running in the background.
   You will be notified when it completes with a result summary.
   Results will be saved to .claude/workflow-runs/{uuid}/state.json
   ```

**Resume 支持**：当 `resumeFromRunId` 非空时，从 `journal_store.read_all(prev_run_id)` 读取历史 journal entries，传入 `WorkflowStartParams.resume`。Node 引擎按 `journalEntry.key` (SHA256) 匹配 cache-hit——命中则直接返回缓存结果，未命中则重新执行。

---

## 4. 通知管线（Notification Pipeline）

### 4.1 整体设计

workflow 完成通知采用双路径模型：

```
workflow 完成
    ├─ registry.complete() → broadcast::Sender.send(WorkflowTaskResult)
    │
    ▼ Session 级 Consumer (peri-agent/src/session/exec/executor.rs,
    │   首次 turn 构建时 spawn，init_notification_buffer() set-once gate 去重)
    │
    │   永久运行，独立于 agent turn 生命周期
    │
    ├─ Path A (TUI 通知) ───────────────────────────────────────────┐
    │   BackgroundTaskResult 写入 registry                           │
    │   → BgRegistryEvent::Completed                                 │
    │   → bg-task-completed unstable event                          │
    │   → peri-tui 事件泵 (AcpEventData::BgTaskCompleted)            │
    │   → 通知条渲染 + bg 面板更新                                    │
    │   （不再经 EventSink 直推 BackgroundTaskCompleted——           │
    │     该映射为死路径，见 spec/issues/2026-08-05-                 │
    │     background-task-completed-event-dead-path）                │
    │                                                                │
    └─ Path B (Agent 感知) ─────────────────────────────────────────┤
        AsyncRouter → InboxHandle → push_defer(Defer kind)           │
        → wake Notify 唤醒新 turn                                   │
        → append_messages_to_transcript 统一包裹注入消息流           │
        → LLM 在下一轮看到通知                                       │
```

### 4.2 Session 级 Consumer

consumer 在首次 turn 构建时 spawn，并持续到 session 结束。spawn 去重由 `init_notification_buffer()` 的 set-once gate 保证；具体入口以 `docs/code-index/peri-agent.md` 和源码为准。

**处理顺序（#117）**：对每个 `WorkflowTaskResult`，consumer **先** Path B（`AsyncRouter::route_workflow_event` → `push_defer` + wake），**再** Path A（`TaskManager::complete` → `BgRegistryEvent::Completed`）。`WorkflowTool` 与 notification task **不得**在 broadcast 之前或与之并发地调用 `TaskManager::complete()`。

实现：`peri-agent/src/session/workflow_completion.rs` 的 `apply_workflow_task_result`（由 `exec/executor/agent_build.rs` 的 broadcast 循环调用）。

**Path A 输出格式**（`BackgroundTaskResult`，写入 registry 触发 `BgRegistryEvent::Completed`）：
```json
{
  "agent_name": "workflow:audit",
  "task_id": "019ef440...",
  "success": true,
  "output": "Workflow 'audit' finished with status Completed (5230ms, 3 agents, 12 tool calls). Results in .claude/workflow-runs/{run_id}/state.json",
  "tool_calls_count": 12,
  "duration_ms": 5230
}
```

**Path B 输出格式**（`Defer` 消息，注入 Agent 消息流；不包裹 `<system-reminder>`——`append_messages_to_transcript` 统一包裹所有 Defer/Info）：
```
Workflow 'audit' completed. (5230ms, 3 agents, 12 tool calls)
- review: 3 agents
- deploy: 2 agents, 50 tokens
Results saved to .claude/workflow-runs/{run_id}/state.json
```
status 文本区分 `completed` / `killed` / `failed`（幽灵完成事件防护，issue 2026-08-05：killed/failed 不得显示为 "completed"）。

### 4.3 唤醒模型

Path B 通知经 `AsyncRouter → InboxHandle → push_defer(Defer kind)` 注入消息流并触发 wake Notify，由下一轮 Receive 统一排空并写入 Transcript，因此无需用户输入，也不会中断正在执行的模型或工具调用。`init_notification_buffer()` 只承担 consumer 的 set-once gate。

**无 inbox 回退**：AsyncRouter 不可用（无 inbox 场景）时，consumer 回退为直接 push 到 v2 message queue（`QueuedMessage::new(Defer, WorkflowComplete, human(...))`，无 wake），并关闭 `notify_bg` 任务计数以消除 Defer 堆积竞态窗口（issue 2026-08-05）。

### 4.4 防重复机制

防重复通过 **session 级一次 spawn + AtomicBool guard** 实现：

1. `init_notification_buffer()` 以 `AtomicBool::compare_exchange(false, true, ...)` 实现 set-once gate（`peri-middlewares/src/workflow/mod.rs`）
2. 首次 turn 构建（`build_and_execute_agent`）时 `init_notification_buffer()` 返回 true → spawn session 级 consumer
3. 后续 turn 构建调用返回 false → 跳过 spawn
4. WorkflowMiddleware 不再持有 per-turn forwarder；通知消费统一由 executor 的 session 级 consumer 处理（见 §4.2）

---

## 5. Kill 机制

### 5.1 三层 Kill

| 层级 | 触发方式 | 实现 |
|------|----------|------|
| **整个 workflow** | `registry.kill(run_id)` / TUI `d` 键 | `registry.kill()` → `kill_tx.send()` + `runs.remove()` → runner 的 kill 分支：5s 超时发送 `workflow/kill` RPC → `child.kill()` → `msg_loop.abort()`（防止覆写 state.json）→ 写入 `killed` state.json → `done_tx.send()` |
| **单 agent** | `runner.kill_agent(run_id, agent_id)` / TUI `x` 键 | 从 `pending_agents` 查找 → `cancel_tx.send()` + `send_error(-32000)` → `deregister_agent()` |
| **用户 cancel** | TUI cancel 信号 | 转发为 `workflow/kill` |

### 5.2 Node 侧响应

- `abortController.abort()` 传播给所有活跃 agent（workflow 级别 kill）
- RpcAdapter 检测 error code `-32000` → `throw WorkflowAbortedError` → 引擎不会 retry

---

## 6. Resume 机制

用于中断恢复或缓存复用。流程：

1. LLM 调用 `WorkflowTool { resumeFromRunId: "prev-run-id" }`
2. `journal_store.read_all("prev-run-id")` → `Vec<JournalEntry>`
3. 传入 `WorkflowStartParams.resume`
4. Node 引擎逐条对比 `journalEntry.key`（SHA256 hash of agent params）
   - cache-hit → 直接返回缓存结果（不执行 agent）
   - cache-miss → 正常调用 agent，结果 `journal/append` 增量写入

---

## 7. 生命周期管理

| 组件 | 作用域 | 创建 | 销毁 |
|------|--------|------|------|
| `WorkflowMiddleware` | session | `session/new` | session end（支持 `set_bg_registry()` 延迟注入统一后台任务注册表） |
| `WorkflowProgressStore` | session | `session/new` | session end |
| `WorkflowTaskRegistry` | session | `session/new` | session end |
| `WorkflowJournalStore` | session | `session/new` | session end |
| `WorkflowRunner` | session | `session/new` | session end |
| `RpcChannel` | per-run | `runner.run()` 内 | `runner.run()` 返回 |
| Node 子进程 | per-run | `runner.run()` spawn | 执行完成或 kill |
| 通知消费者（spawn gate） | per-session | set-once `init_notification_buffer()` | session end |

---

## 8. TUI 集成

### 8.1 WorkflowPanel

```
┌─ Workflows ──────────────────────────────────────────┐
│ [▶ review:verify @parallel] [✓ audit @pipeline]       │  ← Run Tab 行
├──────────────┬────────────────────────────────────────┤
│ Phases (30%) │ Agents (70%)                           │  ← 双栏布局
│ ✓ Review     │ #0 coder ✓ 500t 3tools                 │
│ ▶ Verify     │ #1 review:bugs ▶ 120t 1tool            │
│ ○ Deploy     │ #2 review:perf ⊘ dead                  │
└──────────────┴────────────────────────────────────────┘
```

**布局**：顶部 Run Tab 行 + 双栏（Phases 30% / Agents 70%），Tab 键切换 Run，←/→ 切换 Phases↔Agents 焦点区域，↑/↓ 移动选中项。StatusBar 显示快捷键提示（`Esc`/`q` 关闭，`x` kill agent，`d` kill run，`r` resume）。

**注**：`x`/`d`/`r` 三键目前标记为 "integration pending"（GAP-07/GAP-04），快捷键已注册但实际 kill/resume 逻辑尚未接入。

**实时更新**：面板完全依赖 **Pull 路径**——2s 间隔 ACP 轮询（`spawn_workflow_poll()` → `workflow/list_runs` RPC → `WORKFLOW_SNAPSHOT` atom 更新）。无 Push 路径。

### 8.2 命名 Workflow 命令（未实现）

扫描 `.claude/workflows/` 目录，自动发现 `{name}.js|mjs|ts` 文件，注册 `/{name}` slash command。流程：

1. 启动时 `discover_named_workflows()` 扫描目录
2. `register_named_workflow_commands()` 创建 `WorkflowCmd { name, script_path }`
3. 用户输入 `/{name}` → 读取脚本内容 → 注入 prompt（含 `args` 参数映射）→ LLM 调用 Workflow 工具执行

注：存在一个独立的 `/workflows` 面板命令（`peri-tui/src/kit/panel_registry.rs` 的 `PanelMeta { kind: PanelKind::Workflow, slash_command: "workflows" }`），用于打开 WorkflowPanel。与命名 `/{name}` 命令是两条独立命令。

### 8.3 WorkflowSnapshot — TUI 快照数据

位于 `peri-tui/src/kit/workflow_snapshot.rs`：

```rust
pub struct WorkflowSnapshot {
    pub runs: Vec<TuiRunProgress>,
}

pub struct TuiRunProgress {
    pub run_id: String,
    pub workflow_name: String,
    pub status: String,          // "running" | "completed" | "failed" | "killed"
    pub phases: Vec<TuiPhaseProgress>,
    pub agents: Vec<TuiAgentProgress>,
}
```

DTO 类型镜像 `peri_workflow::progress::RunProgress`（避免直接 crate 依赖）。`agents` 字段使用 JSON 数组格式（服务端 `agents_as_map` serde helper 将 `IndexMap` 转为 `Vec`）。

**数据获取**：通过 `WORKFLOW_SNAPSHOT` atom 存储，由 `spawn_workflow_poll()` 后台任务以 **2s 间隔**轮询 `workflow/list_runs` RPC 更新。`CancellationToken` 控制生命周期（session 结束时取消）。无 session 时写入空 `WorkflowSnapshot`，使 WorkflowPanel 从 loading 状态过渡到空态。

---

## 9. 错误处理策略

| 层级 | 错误 | 处理 |
|------|------|------|
| Tool invoke | 脚本参数缺失、并发限流 | 返回 `ToolError` 给 LLM（含错误描述 + 建议） |
| Runner | Node 进程 spawn 失败 | `WorkflowError::SpawnFailed` → tool spawn 任务记录 warn |
| RPC | Node 崩溃（stdout 关闭） | `drain_pending()` → 所有 pending requests 收到错误 |
| Agent 执行 | ReActAgent 异常 | 转为 `AgentRunResult::Dead { reason }`，不中断 workflow |
| Kill | 子进程残留 | `child.kill().await` 在正常退出和 kill 路径均执行 |
| Journal | 磁盘满、权限错误 | 记录 warn，不中断 workflow 执行 |
| notification | done_rx 断开 | notification_task 标记 failed，仍读取 progress_store 获取可用的统计字段 |

---

## 10. 关键设计决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 脚本执行环境 | 独立 Node.js 进程 | 独立管理进程生命周期并复用 workflow-engine；这不是 OS 级安全 sandbox |
| run_id 生成 | tool.invoke() 中（not runner） | 立即返回 LLM，不等待子进程启动 |
| 通知模型 | Defer 消息流注入（Receive 阶段消费，AsyncRouter → push_defer）| 避免 push 通知打断正在执行的 agent；通知作为消息流进入下轮 Receive |
| 完成通知 | 双路径（TUI + Agent） | TUI 需要实时反馈（bg-task-completed unstable event）；Agent 需要文本感知才能行动（Defer 注入） |
| Consumer 模型 | session 级一次 spawn + AtomicBool guard（`peri-agent/src/session/exec/executor.rs`）| 替代 per-turn forwarder 方案：语义更清晰，broadcast channel 天然单消费者 |
| progress 消费 | TUI 轮询（event payload） | 删除 8 跳 push 管线（~190 行），降为 ~100 行轮询 |
| agent 执行 | 专用 Workflow Agent 链 | 复用 frozen data、tool 与 LLM 基础设施，同时显式限制递归 SubAgent 等能力 |
| 并发限制 | Registry 上限 3 | 防止 LLM 无限 spawn workflow |
| journal 保留 | 最多 50 个 run 目录 | 避免磁盘膨胀，同时保留足够历史供 resume 和调试 |
| system prompt | 冻结 base + request-time contribution seam | 遵循 ARC-FROZEN-001 与 ARC-SERIAL-001，不把 workflow 状态写入缓存前缀 |
| 纠正消息 | Defer 进入 MessageQueue | 禁止用 `system` 消息注入运行时状态；Receive 统一写入 Transcript |

## 11. 能力暴露与使用入口

Workflow 是 deferred capability：只有 workflow executor 可用且
`WorkflowMiddleware` 在当前 session 生产链中时，`Workflow` 才进入 session-local
tool view，并经 `SearchExtraTools → ExecuteExtraTool` 发现和调用。关闭 middleware 或
缺少 executor 时，tool、目录、提示贡献、SubAgent/Workflow 继承与客户端投影必须一起
消失。

面向模型的操作手册是 builtin skill
`peri-middlewares/src/skills/builtin/skills/ultracode/SKILL.md`，按需加载；系统提示词不
常驻复制完整 workflow 教程。复杂、全生命周期交付另由 `ultra-adlc` skill 与
[Ultra-ADLC 设计](ultra-adlc.md)约束。

Workflow host 优先使用本地固定版本 bundle；不可用时按实现契约使用精确版本的
`npx` fallback。运行需要 Node.js，但不要求用户全局安装 `@peri-code/workflow`。
artifact identity、handshake、环境清理与失败语义以代码和相邻测试为准。

用户可通过 `/workflows` 查看运行快照。面板是运行状态投影，不拥有 workflow
lifecycle；kill、resume、journal 与 terminal state 仍由 runner/registry 事实源决定。

---
