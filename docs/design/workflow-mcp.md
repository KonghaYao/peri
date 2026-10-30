# Workflow as MCP 设计

**状态**：已批准目标设计；MCP 实例与任务信道已落地，残留目标见 §7。
**范围**：`peri-workflow` 子系统从 middleware 槽位迁移为 builtin MCP 实例；Node 引擎保留为实例执行后端；agent 回调改为「任务资源发布 + 工具回填」的标准信道语义。
**代码事实源**：`peri-middlewares/src/mcp/builtin/`、`peri-acp-types/src/builtin_mcp.rs`、`peri-workflow/`、`npm-packages/@peri-workflow/`、`peri-acp/src/host/{workflow_agent,requests/workflow}.rs`。

## 1. 背景与取代关系

Workflow 原以 `ChainSlot::Workflow` middleware 槽位与私有 `agent/run` 请求接入，工具、资源、订阅与关闭都不走标准 MCP 面，双方以「server 向 client 发请求」表达执行依赖。本设计以 builtin MCP 实例承载编排能力，并把 agent 回调改为「任务资源发布 + 工具回填」的标准信道；旧槽位、端口与旧 middleware 目录已由该形态取代。

## 2. 目标与非目标

### 2.1 目标

1. workflow 以 **builtin MCP 实例**（`workflow`，per-session 池内、同进程 `rmcp::serve_server`）接入；工具、资源、订阅、关闭全部走标准 MCP 面。
2. agent 回调改为 **MCPP Channel 语义**：agent 任务作为 outbound 资源（event-log）发布，宿主消费者发现并执行，经 inbound 工具回填结果。不新增私有 JSON-RPC method 或 vendor capability。
3. 删除 `ChainSlot::Workflow`、`WorkflowMiddlewarePort`、`WorkflowMiddlewareAdaptor`、`SessionState.workflow_middleware` 与 `peri-resources/src/workflow.rs` 门面。旧槽位、端口与旧 middleware 目录已删除；`peri-resources/src/workflow.rs` 门面仍保留（残留目标，见 §7）。
4. 用户可见配置语义保持兼容：MetaHarness 关闭键 `"WorkflowMiddleware"` 沿用（从中间件表迁到 builtin 实例关闭表）；ACP 方法名（`workflow/*`）与 TUI 交互不变。

### 2.2 非目标

1. 不让 Node 引擎进程直接实现标准 MCP server（外部 stdio 直连）——内置实例已覆盖目标；引擎独立分发留待将来在同一语义上升级。
2. 不迁移 journal 落盘位置（`.claude/workflow-runs/` 保持现状）；归属另行裁决。
3. 不改 ACP 方法名、TUI 面板与 `BgTaskKind::Workflow`；不改 `peri-acp-types/src/workflow.rs` 契约类型。
4. 不做跨进程任务恢复与旧执行恢复：任务表为运行期状态，恢复语义仍由 journal + `resume` 承担（遵循「不恢复旧 Agent 执行」）。
5. 不做 workflow 的跨实例/远程分发。

### 2.3 分发红线

1. **Rust 侧零 JS 嵌入**：`peri-workflow` 及任何 Rust 侧代码不得以 `include_bytes!`、`include_str!`、embed 或等价机制把 JS 产物（如 `@peri-workflow` 的 dist bundle）嵌入二进制。该红线优先于自包含、简化分发等诉求；任何分发缺口都不得以把 JS 产物嵌入二进制的方式解决。
2. **`@peri-workflow` 分发结构要求**：分发须保持「**构建可复现、不入库**（dist 为构建产物，`bun run build` 可复现字节；由发布链在打包前构建并随发布包携带）+ **发布结构**（发布包保留入口产物与包元数据契约）+ **离线可用**（无网络即可获得并可运行已验证 artifact）+ **完整性校验**（artifact 的身份与内容在使用前经受校验；pin = 构建产物 SHA-256 == 发布产物 SHA-256）」。该要求必须以非嵌入途径实现（文件系统读取、摘要校验、网络获取等类别），不得以嵌入机制作为解法。
3. **现状与边界**：Rust 侧嵌入已移除；各项分发要求已以非嵌入方式落实——dist 不入库、由发布链在打包前构建并随发布包携带，消费方默认离线、仅使用通过包身份与入口内容（固定 SHA-256 pin）校验的 artifact；网络获取须显式 opt-in 且内容仍须通过同一摘要校验。安装位置、校验算法、获取时机等具体机制不属本节内容，本节只记录红线与原则；落实与验收证据见 `ARC-WORKFLOW-DIST-001`。

标准依据：`ARC-WORKFLOW-DIST-001`（[architecture-contracts.md](../standards/architecture-contracts.md)）。

## 3. 目标架构

```
┌───────────────────────────── Peri 进程（per-session 池）─────────────────────────────┐
│  peri-agent（模型面）              peri-acp host（宿主面）                            │
│   │ ToolSearch 索引                 │ workflow/* ACP 方法（TUI 轮询/终止/恢复）        │
│   │ mcp__workflow__*（deferred）    │ WorkflowTaskConsumer（agent 任务消费者）         │
│   ▼                                 ▼                                              │
│  ┌──────────────── MCP client 池（session_scoped = true）────────────────┐          │
│  │  workflow 实例连接（duplex + rmcp serve_server）                       │          │
│  │   ├─ tools：Workflow / WorkflowStatus / WorkflowKill（模型面，deferred）│          │
│  │   ├─ tools：WorkflowAgentResult 与消费者鉴权工具（host-only）           │          │
│  │   ├─ resources：workflow://runs、runs/{id}[、events、tasks]、tasks      │          │
│  │   └─ subscriptions：runs / run 快照 / 事件 / 任务                       │          │
│  └──────────────────────────┬────────────────────────────────────────────┘          │
│                             │ WorkflowMcpServer（handler，per-session 状态）        │
│                             │  ├─ WorkflowRuntime（领域控制接口，唯一权威）          │
│                             │  │   start_run / kill / resume / status / 任务表      │
│                             │  └─ 内核：runner / registry / progress / journal      │
│                             ▼                                                       │
│                    Node 引擎进程（per-run，NDJSON wire = WorkflowRuntime 实现细节）   │
└─────────────────────────────────────────────────────────────────────────────────────┘
```

**角色**：workflow 实例是 MCP server；Peri 池是唯一 client。模型经工具桥消费 deferred 工具；宿主经池的 `tools/call`/`resources/read` 消费宿主面工具与资源；消费者订阅任务资源、执行 agent、回填。

**领域控制接口**：MCP adapter（handler）背后的唯一控制面是 `WorkflowRuntime`（start_run / kill / resume / status / 任务表 / 资源快照）。Node wire（`workflow/start`、`agent/task`、`agent/result` 等）是 `WorkflowRuntime` 的实现细节；MCP 对外契约（工具/资源/订阅）不依赖 wire 形态，wire 改造以该领域接口为边界。

**会话隔离**：池按 session 构建（`assemble.rs` `session_scoped=true`），实例与消费者随会话创建、随会话关闭销毁；实例上下文按 `BuiltinInstanceContext` 会话装配期一次性注入（对标 `workspace_resources` 范式）。per-call 身份只走 `peri.invocation.initiatorSessionId`；`_meta["peri/taskOwner"]` 仅注入 builtin workspace 与 `WorkspaceRemote`，workflow 来源不接收该字段。

**信道裁决记录**：

- 为何 builtin 而非外部 stdio server：workflow 执行依赖 session 内对象（frozen prompt、provider、执行依赖），builtin 形态经会话装配注入可在进程内安全传递；外部形态需跨进程传递会话身份，破坏现有执行不变量。
- 为何「资源 + 回填」而非 sampling / 反向请求：MCP 的 server→client 请求（sampling/elicitation）语义是「生成/询问」，不匹配「委托执行完整 agent」；MCPP Channel 的 outbound 资源 + inbound 工具是标准信道内表达双向依赖的唯一形态。

## 4. MCP 面设计

### 4.1 实例声明

`peri-acp-types/src/builtin_mcp.rs` 声明：

- `WORKFLOW_TOOLS`：`Workflow` / `WorkflowStatus` / `WorkflowKill`（模型面，`direct: false`），以及 `WorkflowAgentResult` / `WorkflowAgentClaim` / `WorkflowConsumerReady` / `WorkflowCompletionAck` / `WorkflowClose` / `WorkflowTaskObserved`（host-only，`model_visible: false`）。
- `BUILTIN_MCP_INSTANCES` 条目：`name = "workflow"`、`policy_key = "WorkflowMiddleware"`（沿用旧槽位键，用户配置兼容）。
- `BUILTIN_RESERVED_INSTANCE_NAMES` 包含 `"workflow"`。
- `BuiltinMcpTool::model_visible` 区分模型可见面与 host-only 工具；其余实例默认 `true`。

### 4.2 工具与授权边界

| 工具（original_name） | 可见性 | 语义 | 参数 |
| --- | --- | --- | --- |
| `Workflow` | 模型面（deferred） | 启动/恢复 workflow，异步返回 `run_id` | 现有 `WorkflowTool` 参数全集（script/args/maxConcurrency/budgetTotal/maxAgents/maxToolCalls/maxElapsedMs/resumeFromRunId/name/strictPreflight/writeIntent） |
| `WorkflowStatus` | 模型面（deferred） | 查询 run 列表或单 run 快照 | `{ runId?: string, limit?: number }` |
| `WorkflowKill` | 模型面（deferred） | 终止 run 或单 agent | `{ runId: string, agentId?: string }` |
| `WorkflowAgentResult` | **host-only** | 回填 agent 任务结果 | `{ runId, taskId, result: AgentRunResult }` |
| `WorkflowAgentClaim` | **host-only** | 唯一认领一个任务；同一 `claimId` 重试幂等 | `{ runId, taskId, claimId }` |
| `WorkflowConsumerReady` | **host-only** | 声明消费者就绪（基线读取完成）；`false` 回落就绪并结算遗留执行 | `{ ready: boolean }` |
| `WorkflowCompletionAck` | **host-only** | 确认已接受终态交付，释放待确认结果 | `{ runId }` |
| `WorkflowClose` | **host-only** | 密封准入并排空实例执行 | `{}` |
| `WorkflowTaskObserved` | **host-only** | 确认 run 已进入消费者任务投影 | `{ runId }` |

**可见性 ≠ 授权**。三层分别定义：

1. **发现过滤**：桥接构建跳过 `model_visible = false` 的工具（不进 ToolSearch 索引与 LLM tools 参数）；覆盖 `tool_bridge` 构建、动态重建、`system_mcp_tools` 提升路径的正反向测试。
2. **调用授权（服务端）**：`WorkflowAgentResult` 在 handler 内校验消费者令牌，并要求 `taskId` 属于当前实例任务表且状态为 `executing`（外部回填只接受已被唯一 claim 的任务），否则返回幂等结果或明确错误；不信任调用方参数声明的身份。即使模型猜中完整工具名，`ensure_model_visible` 与调用授权双重拦截。
3. **执行授权**：消费者执行任务时使用 session 既有过滤链（`ToolFilterPolicy`、agent definition、skill 预载）；Node 提供的 `allowedTools` 等字段只能**收缩**、不能授予权限，与现状语义一致。

### 4.3 资源与订阅

| URI | 形态 | 内容 | 订阅 |
| --- | --- | --- | --- |
| `workflow://background-tasks` | state | 后台任务快照与待确认完成结果（宿主投影数据源） | 轮询读取 |
| `workflow://runs` | state | run 索引快照：`[{ runId, name, status, startedAt, progress 摘要 }]` | ✅ |
| `workflow://runs/{runId}` | state | 单 run 快照：四维状态、阶段、agent 统计、`eventWatermark` | ✅ |
| `workflow://runs/{runId}/events` | event-log | progress 事件（run / phase / agent / log）与 `RunDone` 终态；单调 `sequence` + `headSequence` + 保留边界 | ✅ |
| `workflow://runs/{runId}/tasks` | event-log | 任务事件：`published` / `executing` / `completed` / `failed` / `cancelled`（`eventId = {runId}:{taskId}:task.{kind}`） | ✅ |
| `workflow://tasks` | event-log | session 级任务流（跨 run 聚合，消费主入口） | ✅ |

**发现与补读协议**（通知只是唤醒信号，不承担可靠投递）：

1. 消费者订阅 `workflow://tasks`；订阅确认后**立即 `resources/read` 建立基线**（全量扫描未终态任务），之后处理增量。
2. 内核保证「先可见后通知」：任务先进入任务表与事件日志，再发 `notifications/resources/updated`；消费者收到通知后 read，按 eventId 去重（幂等）。
3. 通知丢失/连接重建：重新 `subscriptions/listen` 后重复步骤 1 的全量扫描；扫描幂等，不重复执行已终态任务。
4. 事件日志读取使用稳定索引 URI + 分页参数（`?after={sequence}&limit=N`）；`headSequence` 为已提交上界；读取方发现游标早于保留边界时重新建立快照，不假装连续。
5. 清理：终态 run 与终态任务按 5 分钟保留窗口回收（`WorkflowProgressStore::cleanup_completed`、`TASK_RETENTION_MS`）；清理后 `workflow://runs/{runId}*` 返回 not-found，`workflow://tasks` 聚合仍保留其终态摘要直至窗口过期。

资源内容为不可信模型输入边界：事件日志正文不经模型上下文注入；TUI/ACP 投影由宿主读取资源生成（数据源单一，不新增第二事实源——内存投影 + journal 为真相）。

### 4.4 关闭

- 关闭键 `"WorkflowMiddleware"` 在 `BUILTIN_INSTANCE_POLICY_KEYS` 中声明；`closed_instances` 命中时实例不 spawn、桥接不建、宿主消费者不启动（与 workspace 同构）。
- `MIDDLEWARE_NAMES` 不含该键，`MIDDLEWARE_TOOL_NAMES` 不含 `"Workflow"`；两表零交集由装配测试断言。
- 工具归一/过滤面（`tool_catalog`、`permission`）按实例身份自动生效（`mcp__workflow__*`）。
- 子 Agent 按实例身份整体排除 `workflow`（`CHILD_ABSENT_BUILTIN_INSTANCES`），不另立逐工具名单。

## 5. Agent 任务信道

### 5.1 任务状态机与幂等契约

任务表（per-session、process-local）：

```
published ──► executing ──► completed | failed | cancelled
    │             │
    └─────────────┴──► cancelled（kill / 会话关闭）
```

- 单次终态提交：状态转换由任务表单点 compare-and-set，只允许一次进入终态；重复回填返回既有终态（幂等），不同结果冲突以首次为准并记录告警。
- 结果投递契约（而非未经定义的端到端恰好一次）：**可重试投递 + 单次终态提交 + Node promise 单次结算**。任务结果先提交任务表，再投递 `agent/result`；Node 侧按 taskId 去重，已结算的重复投递忽略。
- `taskId` 作用域 = 单次 run；恢复场景不复用旧 taskId（resume 从 journal 缓存命中，不重放未完成任务）。
- 任务的持久边界：任务表与事件日志为运行期状态（process-local durability，不写 journal）；跨进程恢复不相关（非目标 4）。journal 写入仍由 Node 在拿到结果后按现状 `journal/append` 完成，二者不共享写入权威、互不阻塞。

### 5.2 流程

1. Node 引擎 `agent()` 调用发布任务：`agent/task`（通知，`{ taskId, runId, agentId, prompt, schema, model, maxTokens, agentType, isolation, allowedTools, label, phase }`）。
2. `WorkflowRuntime` 登记任务（taskId 唯一）→ 状态 `published` → 追加 `workflow://tasks` 与 `workflow://runs/{runId}/tasks` 事件 → 发 `resources/updated`。
3. 消费者收到唤醒 → read → 经 `WorkflowAgentClaim`（稳定 claimId，同一临界区容量判定）逐任务进入 `executing`：用会话执行依赖运行 `WorkflowAgentExecutor`（保留 workflow capability 投影、skill 预载、agent definition 解析、过滤链）→ 调 `WorkflowAgentResult` 回填。
4. `WorkflowRuntime` 提交终态 → 追加任务事件 → 将 `AgentRunResult` 投递回 Node 等待方（`agent/result`）。
5. 并发：消费者**有界并发**（与 run `maxConcurrency`/limits 同一执行权威，不另立计数）；排队任务在达到上限时不进入 `executing`，取消可作用于排队或执行中任务。

### 5.3 取消与竞态

- 每任务持有取消句柄、单一终态提交点。覆盖并必须验证的竞态：
  - kill 先于消费：任务直接转 `cancelled`，不进入执行。
  - 执行中 kill：取消执行（等待执行 future 结束/排空），终态 `cancelled`。
  - 结果与 kill 同时到达：以先到终态转换者为准（compare-and-set），另一路径观察到幂等结果。
  - Node 退出/超时/池断开/会话关闭：任务被取消或有界等待；不存在 detached 执行。
  - 迟到结果：任务已终态后到达的回填按幂等处理（`completed` 保留首次，`cancelled` 不被覆盖）。
- `cancelled` 编码为现有 `AgentRunResult`（对齐 `Skipped{reason}` / `Dead{reason}` 语义），Node 侧按引擎约定抛 `WorkflowAbortedError`。
- 单 agent kill 与 run kill 走同一任务表与取消句柄路径。

### 5.4 Node wire

保留：`workflow/start`、`workflow/kill`（宿主→Node 请求）；`progress/event`、`journal/append|truncate`、`log`、`workflow/done`（Node→宿主通知）。

变更：Node→宿主 `agent/task`（通知）发布任务，宿主→Node `agent/result`（通知）回填结果；原 `agent/run` 请求/响应不再使用。

`npm-packages/@peri-workflow/src/{adapter,rpc,types}.ts`：adapter 维护 `taskId → pending` 等待表，等待 `agent/result`；超时与 abort 语义保持。`peri-workflow/src/{protocol,runner/message_loop}.rs` 负责任务登记与回填投递；Rust wire 类型与 npm `types.ts` 保持两侧同步。wire 以 `WorkflowRuntime` 的领域接口为边界，不与 MCP 面切换耦合。

### 5.5 消费者（WorkflowTaskConsumer）

- 装配于 `peri-acp/src/host/`，session 生命周期内持有；绑定 session 池与实例，是执行依赖的唯一持有者（executor 由 `create_executor(WorkflowAgentContext)` 构造，经会话装配注入）。
- 就绪屏障：实例可先初始化，但 `Workflow` 工具在消费者订阅就绪前拒绝新启动（返回明确错误），避免「首个任务无人消费」。
- 关闭顺序（与 `session_close` 链路同序）：① 停止接受新 run 与新任务发布 → ② 取消或有界等待已登记任务（超时后强制收敛并记录）→ ③ 保留回填通道完成终态处理 → ④ 收割执行任务与 Node 进程 → ⑤ 释放订阅与池。

## 6. 会话装配与依赖注入

1. `BuiltinInstanceContext` 持有 workflow 槽位 `WorkflowInstanceInput`（对标 `WorkspaceInstanceInput`）：槽位只承载 `Arc<WorkflowRuntime>` 的首次绑定，`cwd`、journal 根、`session_id`、执行依赖与关闭位都在 runtime 内；注入时机与 workspace 同点（`assemble.rs` 池装配期，`run_initialize` 前），会话关闭按 `close_builtin_task` 收敛。
2. 装配顺序：上下文注入 → 实例 spawn → 会话创建后绑定 runtime → 消费者订阅并就绪；任何一步失败按会话初始化失败处理并回收已建资源。
3. ACP 面：`host/requests/workflow.rs` 的 handler 经池调用 `WorkflowStatus` / `WorkflowKill` / `Workflow`（resume = 携带 `resumeFromRunId` 调 `Workflow`，内核完成 state/journal 读取与启动，ACP 不拼装参数）；端口面为 `McpPoolPort::{call_instance_tool, read_instance_resource, subscribe_instance_resource}`。
4. ACP 调用的权限/错误映射保持现状语义：错误类型、排序、不存在 run 的行为进入兼容契约测试。

## 7. 待裁决记录

- journal 落盘归属（工作区工具环境 vs Session Store）：保持现状（`.claude/workflow-runs/`），独立裁决；原裁决 issue 已归档，可经 Git 历史检索。
- `peri-resources/src/workflow.rs` 门面删除：未落地残留目标；`peri-resources` 仍依赖 workflow，删除需独立裁决。
- Node 引擎独立分发为外部 MCP server：本设计在同语义上保留升级路径（`agent/task`/`agent/result` ↔ 标准资源/工具），不在本范围实施。

## 8. MCP 数据边界契约

- `WorkflowInstanceInput` 为实例绑定独立执行管理器；Host 任务目录不注入内核。
- Host 通过 `workflow://background-tasks` 读取后台任务快照与待确认完成结果，重建外部任务投影。完成结果先交付会话，再调用鉴权的 `WorkflowCompletionAck`；断线与拒收不确认，重复扫描沿稳定终态身份去重。不再共享业务 broadcast。
- `WorkflowAgentClaim`、`WorkflowConsumerReady`、`WorkflowCompletionAck`、`WorkflowClose`、`WorkflowTaskObserved` 与 `WorkflowAgentResult` 采用同一消费者 `_meta` 能力令牌。领取、就绪、取消、完成和关闭均经过 MCP，不通过进程内任务表接口。
- `WorkflowClose` 密封准入并等待实例执行管理器清理；Host 在协议确认成功、补读终态后停止投影与消费者，最后释放 MCP 池。失败返回未完成，保留重试能力。
- 待确认结果为当前实例内存状态，服务端进程崩溃后的恢复不在本变更保证范围内。

- 后台启动响应须等待 `WorkflowTaskObserved`（运行中已登记）或完成确认（终态已交付）；协议确认前不向主循环宣称后台工作已移交。等待超时显式报错并请求取消，保留实际 run 身份。
