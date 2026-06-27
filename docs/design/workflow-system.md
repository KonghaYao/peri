# Workflow 系统设计文档

**版本**: 2.0
**日期**: 2026-06-23
**关联分支**: `feature/workflow-ultracode`
**状态**: 已实现 / 已与代码同步

> **v2.0 变更**（2026-06-23）：基于实现代码的逐模块对比，修正 60+ 处差异。主要变更：
> §3.1 协议字段补全、§3.2 消息循环实际模型、§3.4 Registry 职责纠错、§3.5 AgentStatus 5 变体、
> §3.7 System prompt 非精简版、§3.8 Tool 参数补全 + deferred 层级修正、§4.4 防重复机制完全重写、
> §8 TUI 布局/字段修正 + pull 轮询为主要更新路径 + `/workflows` 面板命令文档化、
> §11.4 Ultracode Skill 完整文档化（LLM 操作手册）

---

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
│  ├─ WorkflowTracker      事件累积器 (HashMap<run_id, Snapshot>)        │
│  ├─ /workflow 命令       自动发现 .claude/workflows/*.js               │
│  └─ BackgroundTaskCompleted 通知条渲染                                  │
├──────────────────────────────────────────────────────────────────────┤
│  peri-acp (ACP 服务层)                                                 │
│  │                                                                     │
│  │  SessionState ─── 持有 session 级共享状态                           │
│  │  ├─ WorkflowMiddleware      聚合容器（持有 Runner/Registry/          │
│  │  │                          Progress/Journal 等所有 workflow 状态）  │
│  │  ├─ WorkflowRunner          子进程管理器（含 Node binary 查找）       │
│  │  ├─ WorkflowTaskRegistry    并发控制 + 完成广播                      │
│  │  ├─ WorkflowProgressStore   reducer 内存状态                        │
│  │  └─ WorkflowJournalStore    磁盘持久化 (.claude/workflow-runs/)      │
│  │                                                                     │
│  │  WorkflowTool               Deferred tool → invoke() → fire-and-forget │
│  │  WorkflowAgentExecutor      SubAgent 构建器 (完整中间件链)           │
│  │  Notification Consumer      双路径通知 (TUI + Agent)                │
├──────────────────────────────────────────────────────────────────────┤
│  peri-workflow (Rust crate)                                            │
│  ├─ protocol.rs   JSON-RPC 协议类型 (请求/响应/通知)                    │
│  ├─ progress.rs   ProgressStore (内存 reducer)                         │
│  ├─ journal.rs    JournalStore (磁盘 append-only)                      │
│  ├─ registry.rs   WorkflowTaskRegistry (广播 + 并发限流)               │
│  ├─ runner.rs     WorkflowRunner (子进程 spawn/kill)                   │
│  └─ rpc.rs        RpcChannel (双向 JSON-RPC, pending 追踪)             │
├──────────────────────────────────────────────────────────────────────┤
│  @peri-code/workflow (npm 包, Node.js 进程)                                 │
│  ├─ runner.js     RPC handler + AgentAdapter + WorkflowPorts 实现      │
│  └─ 依赖 @claude-code-best/workflow-engine (零修改复用)                │
│                                                                        │
│  用户脚本 (JavaScript ESM)                                             │
│  └─ workflow(meta, fn) → agent/parallel/pipeline/phase/log            │
└──────────────────────────────────────────────────────────────────────┘
```

**通信协议**：Rust ←→ Node 通过 stdin/stdout 双向 JSON-RPC 2.0（NDJSON 格式）。Node 侧 RPC server，Rust 侧 RPC client。

---

## 3. 核心组件设计

### 3.1 RPC 协议

在 `peri-workflow/src/protocol.rs` 定义所有协议类型。采用 JSON-RPC 2.0，每行一条完整 JSON（NDJSON），基于 `serde_json::Value` 编码。

**请求消息**（Rust → Node）：

| 方法 | 用途 | 关键参数 |
|------|------|----------|
| `workflow/start` | 启动脚本执行 | runId, script, args, maxConcurrency, resume, cwd, budgetTotal, workflowName |
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
3. `resolve_binary()` — 三段式查找 Node 执行文件（环境变量 → PATH → `~/.peri/peri-workflow`）
4. `spawn_node_process()` — 启动 `node` 子进程，继承 cwd 和 PATH
5. 创建 `RpcChannel`（绑定 child stdin/stdout，启动 `spawn_stdout_reader()` 线程）
6. `send_request("workflow/start")`，**15 秒超时** — Node runner.js 开始执行
7. **消息循环**（`tokio::spawn` 内 `while let Some(msg) = msg_rx.recv().await`，外层 `select!` 竞速 `kill_rx` 和消息循环 join_handle）：
   - `agent/run` → `spawn_agent_execution()` → 异步执行 → 发送 response
   - `progress/event` → `progress_store.apply_event(event)`
   - `journal/append` → `journal_store.append(entry)`
   - `journal/truncate` → `journal_store.truncate(run_id)`
   - `log` → 按 level 字段（error/warn/info/debug）映射到 tracing 级别
   - `workflow/done` → **break** 退出循环 → 记录 `finished_at` 时间戳 → 写入 `state.json` → `done_tx.send()`
   - `kill_tx` 信号 → `send_request("workflow/kill")` + `child.kill()`
8. 清理：`child.kill().await`、移除 `active_channels`、`cleanup_old_runs()`

**Agent 执行 spawn**：每个 `agent/run` 请求触发 `tokio::spawn`，通过 `AgentExecutor` trait 回调到 `peri-acp` 的 `WorkflowAgentExecutor`。RpcChannel 通过 `pending_agents` 追踪所有活跃 agent（用于单 agent kill 查找）。

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
| `kill(run_id)` | 触发 kill_tx + `runs.remove()` + `child_handle.abort()`（三层清理） |
| `list_runs()` | 返回运行中任务列表 |
| `status(run_id)` | 返回给定 run 的状态 |
| `active_count()` | 返回当前活跃 run 数 |

**完成通知**：`tool.rs` 的 notification_task 在 done_rx 收到后，从 `ProgressStore::get_run()` 读取 `agent_count` 和 `tool_calls_count`，连同 `status`/`duration_ms`/`error` 构造 `WorkflowTaskResult`，调用 `registry.complete()` **仅做广播**（不读 ProgressStore，不移除 run——history 保留供调试）。`WorkflowTaskResult::to_notification()` 方法格式化 `<system-reminder>` 文本块。

**广播消费**：broadcast receiver 在 `peri-acp/src/session/executor.rs` 被唯一 consumer（session 级 forwarder）消费——单一消费者确保无重复通知。

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
    meta: Option<Value>,      // 原始 JSON，未结构化（始终为 None，转换未实现）
    phases: Vec<PhaseProgress>,
    agents: Vec<AgentProgress>,
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
| `run_started` | 创建 RunProgress（meta 设为 None——原始 JSON 携带但未结构化） |
| `phase_started/done` | 创建/更新阶段 |
| `agent_started` | 创建 AgentProgress（**Pending** 状态）→ 然后设为 **Running** |
| `agent_progress` | 更新 token_count/tool_count |
| `agent_done` | 更新 status + result + tool_count（从 result 提取） |
| `log` | 无需存储（tracing 转日志） |
| `run_done` | 更新 RunStatus |

**额外方法**（未在设计初期体现）：`get_all_runs_snapshot()` — ACP 序列化用；`cleanup_completed()` — 清理已完成 runs；`active_runs()` — 仅返回活跃 runs。

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

`peri-acp/src/agent/workflow_agent.rs` — workflow 内部 agent 的构建器。

**核心职责**：将 `AgentRunParams`（来自 Node 的 `agent/run` RPC）转换为完整的 ReActAgent 执行。

**构建链**（相对于 Main Agent 的差异）：

| 方面 | Main Agent | Workflow Agent |
|------|-----------|----------------|
| 中间件数量 | 19 个 | ~10 个（不含条件注册的 CompactMiddleware 时为 10，含则 11） |
| Frozen data | 完整 | **透传**（从 session frozen） |
| System prompt | 完整（13 段） | **完整 frozen system prompt**（`ctx.system_prompt.clone()`，非精简版） |
| LLM Model | 用户选择 | 跟随 session provider（`ctx.provider.clone().into_model()`，无 Anthropic 回退） |
| max_iterations | 500 | 200 |
| HITL | 完整 | 共享 session 权限模式 |
| Langfuse | 完整 | 启用 |

**透传的 WAI 上下文**（`WorkflowAgentContext` 含 15 个字段，远超最初设计的 5 个）：
- CLAUDE.md 内容 / Skills 摘要 / System prompt（完整 frozen）
- session_id / compact_config / cancel token
- broker / permission_mode（HITL 共享）
- AgentPool（LLM 实例缓存池）
- Langfuse session / tracer
- ThreadStore（持久化）

**条件注册**：
- `CompactMiddleware`：仅当未设置 `DISABLE_AUTO_COMPACT` 时
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
5. `tokio::spawn(runner.run())` — 后台启动执行
6. `tokio::spawn(notification_task(receiver))` — 等待 done_rx
7. **立即返回**（多行格式）：
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

workflow 完成通知采用双路径 PULL 模型：

```
workflow 完成
    ├─ registry.complete() → broadcast::Sender.send(WorkflowTaskResult)
    │
    ▼ Session 级 Consumer (executor.rs, 首次 execute_prompt 时 spawn)
    │
    │   永久运行，独立于 agent turn 生命周期
    │
    ├─ Path A (TUI 通知) ─────────────────────────────────────┐
    │   notify_sink.push_event(BackgroundTaskCompleted)        │
    │   → MpscTransport → peri-tui 事件泵                      │
    │   → 通知条渲染 + WorkflowTracker 更新                     │
    │                                                          │
    └─ Path B (Agent 感知) ────────────────────────────────────┤
        agent_notify_tx.send(notification_text)                │
        → WorkflowMiddleware.notification_buffer (mpsc)        │
        → 下轮 execute_prompt() 开头 drain                    │
        → BaseMessage::human() 注入消息流                      │
        → LLM 在下一轮看到通知                                  │
```

### 4.2 Session 级 Consumer

`executor.rs:957-1023` — 在首次 `execute_prompt()` 时 spawn，永久运行直到 session 结束。

**Path A 输出格式**：
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

**Path B 输出格式**（注入 Agent 消息流）：
```html
<system-reminder>
[后台任务 019ef440 已完成] workflow-audit (5230ms, 3 agents, 12 tool calls)
</system-reminder>
```

### 4.3 PULL 模型

Path B 通知缓冲在 `WorkflowMiddleware.notification_buffer_rx`（mpsc unbounded channel），仅在 `execute_prompt()` 开头 drain（`executor.rs:350-356`）。`execute_prompt()` 只在用户输入时运行 → **PULL 模型**。

**PULL 模型的补充——TUI auto-continuation**：

TUI 在 `handle_background_task_completed()` 检测 `agent_name.starts_with("workflow:")`，自动推入 `pending_messages` 队列。agent 空闲时通过 `flush_pending_messages()` → `submit_message()` 触发新的 `execute_prompt()`，达到"无需用户输入、自动处理 workflow 结果"的效果。

### 4.4 防重复机制

防重复通过 **session 级一次 spawn + AtomicBool guard** 实现：

1. `init_notification_buffer()` 时设置 `NOTIFY_SPAWNED: AtomicBool = false`
2. `execute_prompt()` 首次运行时 `compare_exchange(false, true)` → spawn session 级 consumer
3. 后续 `execute_prompt()` 调用检查 `NOTIFY_SPAWNED` 已为 `true` → 跳过 spawn
4. WorkflowMiddleware 不再持有 per-turn forwarder；`builder.rs` 的 workflow 中间件注册区注释明确写明："完成通知由 executor.rs 的 session 级 consumer 处理"

**关键演變**：早期设计曾尝试 per-turn forwarder + `swap_forwarder_abort()` 方案，但在 `b0d91529`（PULL 模型简化）中被 session 级 AtomicBool 方案替代——因为单一 session 级 consumer 语义更清晰，且 broadcast channel 天然保证单消费者无重复。

---

## 5. Kill 机制

### 5.1 三层 Kill

| 层级 | 触发方式 | 实现 |
|------|----------|------|
| **整个 workflow** | `registry.kill(run_id)` / TUI `d` 键 | `kill_tx.send(())` → select! 分支 → `send_request("workflow/kill")` + `runs.remove()` + `child_handle.abort()` + `child.kill().await` |
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
| `WorkflowMiddleware` | session | `session/new` | session end |
| `WorkflowProgressStore` | session | `session/new` | session end |
| `WorkflowTaskRegistry` | session | `session/new` | session end |
| `WorkflowJournalStore` | session | `session/new` | session end |
| `WorkflowRunner` | session | `session/new` | session end |
| `RpcChannel` | per-run | `runner.run()` 内 | `runner.run()` 返回 |
| Node 子进程 | per-run | `runner.run()` spawn | 执行完成或 kill |
| notification buffer | per-session | set-once `init_notification_buffer()` | session end |

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

**实时更新**：面板通过**双路径**获取数据：
1. **Pull 路径**（主要）：1s 间隔 ACP 轮询（`poll_workflow_runs()`）→ `workflow/list_runs` RPC → 从 `progress_store` 拉取快照 → `deserialize_snapshot()` 更新面板
2. **Push 路径**（辅助）：`AgentEvent::WorkflowProgress` → `tracker.apply()` 用于在面板打开时初始化初始数据。实时更新主要依赖 Pull 轮询（事件管线 PULL 架构的结果）

### 8.2 命名 Workflow 命令

扫描 `.claude/workflows/` 目录，自动发现 `{name}.js|mjs|ts` 文件，注册 `/{name}` slash command。流程：

1. 启动时 `discover_named_workflows()` 扫描目录
2. `register_named_workflow_commands()` 创建 `WorkflowCmd { name, script_path }`
3. 用户输入 `/{name}` → 读取脚本内容 → 注入 prompt（含 `args` 参数映射）→ LLM 调用 Workflow 工具执行

注：存在一个独立的 `/workflows` 面板命令（`WorkflowsCommand`，`command/panel/workflows.rs`），用于打开 WorkflowPanel。与命名 `/{name}` 命令是两条独立命令。

### 8.3 WorkflowProgressTracker

事件累积器，位于 `peri-tui/src/app/workflow_tracker.rs`：

```rust
pub struct WorkflowProgressTracker {
    runs: HashMap<String, WorkflowRunSnapshot>,  // 字段名为 runs（非 snapshots）
}

pub struct WorkflowRunSnapshot {
    run_id: String,
    workflow_name: String,
    status: String,          // 简化：用 String 而非 RunStatus 枚举
    phases: Vec<PhaseSnapshot>,     // PhaseSnapshot 用 String status
    agents: Vec<AgentSnapshot>,     // AgentSnapshot 用 String status
    // 注：不含 agent_count/tool_calls_count/duration_ms 字段
    // 注：不含 result: Option<AgentRunResult> 字段
}
```

通过 `apply(WorkflowProgressPayload)` 更新快照（**不接受** `BackgroundTaskCompleted`——完成通知由 `agent_events_bg.rs` 独立处理）。`snapshots()` 按活跃状态优先 + run_id 排序。`clear()` 方法清空所有数据。

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
| 脚本执行环境 | 独立 Node.js 进程 | 沙箱隔离，不污染 Rust 运行时；复用现有 workflow-engine |
| run_id 生成 | tool.invoke() 中（not runner） | 立即返回 LLM，不等待子进程启动 |
| 通知模型 | PULL（等待 execute_prompt drain）| 避免 push 通知打断正在执行的 agent；通知作为消息流注入，GPT cache 友好 |
| 完成通知 | 双路径（TUI + Agent） | TUI 需要实时反馈；Agent 需要文本感知才能行动 |
| Consumer 模型 | session 级一次 spawn + AtomicBool guard | 替代 per-turn forwarder 方案：语义更清晰，broadcast channel 天然单消费者 |
| progress 消费 | TUI 轮询（event payload） | 删除 8 跳 push 管线（~190 行），降为 ~100 行轮询 |
| agent 执行 | 完整 SubAgent 中间件链 | 复用 frozen data、HITL、tool 系统、LLM 管理 |
| 并发限制 | Registry 上限 3 | 防止 LLM 无限 spawn workflow |
| journal 保留 | 最多 50 个 run 目录 | 避免磁盘膨胀，同时保留足够历史供 resume 和调试 |
| system prompt | 全程 unchanged | 遵循 Prompt Cache 规则——中途变更导致 cache 失效 + 行为漂移 |
| 纠正消息 | `BaseMessage::human()` + `<system-reminder>` | 遵循 [TRAP]——禁止用 `system` 消息注入纠正（会被 hoist 到 top 导致 frozen prompt 污染） |

---

## 11. 配置与约束

### 11.1 系统提示词

`peri-tui/prompts/sections/16_workflow.md` — LLM 可见的 workflow 简介（~10 行），内容简练：
- Workflow 是 deferred tool，需通过 `SearchExtraTools` 发现
- 主要用途说明（编排多 agent 并行/流水线）
- **重定向到 ultracode skill**（"For detailed guidance on writing workflow scripts, invoke the `ultracode` skill."）
- 原语（parallel/pipeline/phase/log）、配额说明、注意事项等已委托给 ultracode skill，不在 prompt section 中重复

### 11.2 推荐用法

- **单脚本**：WorkflowTool { script: "<inline>" } — 简单测试场景
- **脚本路径**：WorkflowTool { scriptPath: ".claude/workflows/audit.js" } — 生产场景
- **Resume**：WorkflowTool { scriptPath: "...", resumeFromRunId: "prev-id" } — 中断恢复
- **命名命令**：`/{name}` — 用户直接触发（通过 slash command）

### 11.3 限制

- 单 session 最多 50 个历史 run 目录
- 并发最多 3 个 workflow
- agent 最多 200 轮迭代（vs Main Agent 500 轮）
- workflow agent 无 SubAgent 能力（避免递归嵌套）
- 用户需要预装 `@peri-code/workflow` npm 包（含 workflow-engine 依赖）

### 11.4 Ultracode Skill — LLM 可见的 Workflow 操作手册

`peri-middlewares/src/skills/builtin/skills/ultracode/SKILL.md` — 这是 LLM 使用 Workflow 系统的**完整操作手册**。当 LLM 需要编排多 Agent 时，它加载此 skill 获得全部所需知识。

**在系统中的角色**：

```
用户说 "/ultracode" 或 "workflow" 或 "parallel"
        │
        ▼
Skill 系统匹配 ultracode skill → 注入 LLM 上下文
        │
        ▼
LLM 阅读 SKILL.md → 学会如何用 Workflow 工具
        │
        ├─ SearchExtraTools("workflow") → 发现 deferred tool
        ├─ ExecuteExtraTool("Workflow", { script: "..." }) → 启动 workflow
        └─ /workflows → 打开面板监控进度
```

**为什么用 Skill 而非 System Prompt**：

| 方案 | System Prompt 全量注入 | Skill 按需加载 |
|------|------------------------|----------------|
| Prompt 体积 | 每次对话都携带（~145 行） | 仅在触发时注入 |
| Cache 友好性 | 占用 prompt cache，降低有效上下文 | 不影响日常对话 |
| 更新灵活性 | 修改 system prompt 需重启 session | 修改 SKILL.md 即时生效 |
| 职责分离 | system prompt 混杂操作细节 | system prompt 只做简介（~10 行），skill 提供完整指南 |

**Skill 内容结构**：

| 段落 | 内容 | 目的 |
|------|------|------|
| **When to Use** | 适用/不适用场景清单 | 防止 LLM 滥用 workflow（简单任务直接做，不要编脚本） |
| **How to Use** | 发现 deferred tool 的两步流程 + 六种原语定义 | 教 LLM 正确的工具调用序列和脚本语法 |
| **Examples** | Parallel / Pipeline / Phase / Sub-workflow 完整示例 | 可复制的模板，降低 LLM 写出错误脚本的概率 |
| **Best Practices** | label、allowedTools、phase、maxConcurrency 的使用建议 | 确保 LLM 产出的 workflow 具备可观察性和安全性 |
| **Script Constraints** | 沙箱限制（禁止 Date、meta 必须含 name+description）| 防止 LLM 写出因沙箱限制而失败的脚本 |

**关键细节**：

- **`parallel` 陷阱警告**：Skill 明确标注 `parallel` 入参必须是工厂函数 `() => agent(...)` 而非直接 `agent(...)`。直接传 Promise 会被 runtime 静默吞掉，workflow 以「假成功 + 全 null」结束。这是一条从真实 bug 中提炼的防御性文档。
- **Deferred tool 发现流程**：Workflow 不是 core tool——LLM 必须先调 `SearchExtraTools("workflow")` 再用 `ExecuteExtraTool` 调用。Skill 将此两步流程作为第一项操作说明，防止 LLM 尝试直接调用。
- **`userInvocable: true`**：用户可通过 `/ultracode [task]` 直接触发，`argumentHint` 提供参数提示。
- **触发词匹配**：`description` 字段包含 "ultracode"、"workflow"、"parallel agents"、"pipeline" 等关键词，skill 系统据此自动激活。

**与 §11.1 系统提示词的关系**：System prompt section `16_workflow.md`（~10 行）仅做简介 + 显式重定向：*"For detailed guidance on writing workflow scripts, invoke the `ultracode` skill."* 实际的操作细节、示例、约束全部在 SKILL.md 中。这是一个两层架构：system prompt 告诉 LLM "有这个能力"，skill 告诉 LLM "怎么用它"。
