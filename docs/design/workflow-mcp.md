# Workflow as MCP 设计

**状态**：已批准目标设计（2026-10-11 用户裁决；同日对抗审查后修订）
**范围**：`peri-workflow` 子系统从 middleware 槽位迁移为 builtin MCP 实例；Node 引擎保留为实例执行后端；agent 回调改为「任务资源发布 + 工具回填」的标准信道语义。
**代码事实源**：`peri-middlewares/src/mcp/builtin/`、`peri-acp-types/src/builtin_mcp.rs`、`peri-workflow/`、`npm-packages/@peri-workflow/`、`peri-acp/src/host/{workflow_agent,requests/workflow}.rs`。
**实施进度**：`spec/issues/2026-10-11-workflow-as-mcp.md`。

## 1. 动机

Workflow 是目前唯一仍以「middleware 槽位 + 私有 wire 协议」接入 Peri 的大能力，其余能力（web / artifact / cron / workspace）已全部迁移为 builtin MCP 实例。现状与目标差距分四类：

1. **注册面特例**：工具经 `ChainSlot::Workflow` 与 `WorkflowMiddlewarePort` 注入，不享受 MCP 工具面的统一机制（桥接、命名、deferred 面、`system_mcp_tools`、关闭语义、权限面）。
2. **信道私有**：Node 引擎与宿主的 `agent/run`、`progress/event`、`workflow/done` 属定制协议；进度/完成不进入标准资源与订阅面，TUI 依赖 ACP 轮询（2s `workflow/list_runs`）。
3. **边界未定**：`peri-middlewares/src/workflow/mod.rs` 经 `peri-resources/src/workflow.rs` 门面 re-export 编排 crate，使 Resources 依赖 Workflow；`spec/issues/2026-10-11-module-dependency-boundary-audit.md` 的 Workflow 两项边界待裁决。
4. **双向依赖不合规**：workflow 引擎需要宿主执行 agent（LLM ReAct + 工具 + skill），现状以「server 向 client 发请求」表达；标准信道上应表达为「任务发布（资源）+ 结果回填（工具）」。

## 2. 目标与非目标

### 2.1 目标

1. workflow 以 **builtin MCP 实例**（`workflow`，per-session 池内、同进程 `rmcp::serve_server`）接入；工具、资源、订阅、关闭全部走标准 MCP 面。
2. agent 回调改为 **MCPP Channel 语义**：agent 任务作为 outbound 资源（event-log）发布，宿主消费者发现并执行，经 inbound 工具回填结果。不新增私有 JSON-RPC method 或 vendor capability。
3. 删除 `ChainSlot::Workflow`、`WorkflowMiddlewarePort`、`WorkflowMiddlewareAdaptor`、`peri-resources/src/workflow.rs` 门面与 `SessionState.workflow_middleware`。
4. 用户可见配置语义保持兼容：MetaHarness 关闭键 `"WorkflowMiddleware"` 沿用（从中间件表迁到 builtin 实例关闭表）；ACP 方法名（`workflow/*`）与 TUI 交互不变。

### 2.2 非目标

1. 不让 Node 引擎进程直接实现标准 MCP server（外部 stdio 直连）——内置实例已覆盖目标；引擎独立分发留待将来在同一语义上升级。
2. 不迁移 journal 落盘位置（`.claude/workflow-runs/` 保持现状）；归属属 `2026-10-11-module-dependency-boundary-audit.md` 的独立裁决项。
3. 不改 ACP 方法名、TUI 面板与 `BgTaskKind::Workflow`；不改 `peri-acp-types/src/workflow.rs` 契约类型。
4. 不做跨进程任务恢复与旧执行恢复：任务表为运行期状态，恢复语义仍由 journal + `resume` 承担（遵循「不恢复旧 Agent 执行」）。
5. 不做 workflow 的跨实例/远程分发。

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
│  │   ├─ tools：WorkflowAgentResult（host-only，回填）                     │          │
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

**领域控制接口**：MCP adapter（handler）背后的唯一控制面是 `WorkflowRuntime`（start_run / kill / resume / status / 任务表 / 资源快照）。Node wire（`workflow/start`、`agent/task`、`agent/result` 等）是 `WorkflowRuntime` 的实现细节；MCP 对外契约（工具/资源/订阅）不依赖 wire 形态，**wire 改造与 MCP 面落地解耦实施**。

**会话隔离**：池按 session 构建（`assemble.rs` `session_scoped=true`），实例与消费者随会话创建、随会话关闭销毁；实例上下文按 `BuiltinInstanceContext` 会话装配期一次性注入（对标 `workspace_resources` 范式）。per-call 身份沿用 `_meta["peri/taskScope"]` 能力令牌 + `peri.invocation.initiatorSessionId` 通道，为 workflow 源接线。

**信道裁决记录**：

- 为何 builtin 而非外部 stdio server：workflow 执行依赖 session 内对象（frozen prompt、provider、执行依赖），builtin 形态经会话装配注入可在进程内安全传递；外部形态需跨进程传递会话身份，破坏现有执行不变量。
- 为何「资源 + 回填」而非 sampling / 反向请求：MCP 的 server→client 请求（sampling/elicitation）语义是「生成/询问」，不匹配「委托执行完整 agent」；MCPP Channel 的 outbound 资源 + inbound 工具是标准信道内表达双向依赖的唯一形态。

## 4. MCP 面设计

### 4.1 实例声明

`peri-acp-types/src/builtin_mcp.rs` 新增：

- `WORKFLOW_TOOLS`：4 个工具声明（3 个模型面 `direct: false` + 1 个 host-only）。
- `BUILTIN_MCP_INSTANCES` 条目：`name = "workflow"`、`policy_key = "WorkflowMiddleware"`（沿用现有键，用户配置兼容）。
- `BUILTIN_RESERVED_INSTANCE_NAMES` 增 `"workflow"`。
- `BuiltinMcpTool` 新增 `model_visible: bool`（host-only 支持；现表内其余实例默认 `true`）。

### 4.2 工具与授权边界

| 工具（original_name） | 可见性 | 语义 | 参数 |
| --- | --- | --- | --- |
| `Workflow` | 模型面（deferred） | 启动/恢复 workflow，异步返回 `run_id` | 现有 `WorkflowTool` 参数全集（script/args/maxConcurrency/budgetTotal/maxAgents/maxToolCalls/maxElapsedMs/resumeFromRunId/name/strictPreflight/writeIntent） |
| `WorkflowStatus` | 模型面（deferred） | 查询 run 列表或单 run 快照 | `{ runId?: string, limit?: number }` |
| `WorkflowKill` | 模型面（deferred） | 终止 run 或单 agent | `{ runId: string, agentId?: string }` |
| `WorkflowAgentResult` | **host-only** | 回填 agent 任务结果 | `{ runId, taskId, result: AgentRunResult }` |

**可见性 ≠ 授权**。三层分别定义：

1. **发现过滤**：桥接构建跳过 `model_visible = false` 的工具（不进 ToolSearch 索引与 LLM tools 参数）；覆盖 `tool_bridge` 构建、动态重建、`system_mcp_tools` 提升路径的正反向测试。
2. **调用授权（服务端）**：`WorkflowAgentResult` 在 handler 内校验 `taskId` 属于当前实例的活跃任务表且状态为 `published`，否则返回幂等结果或明确错误；不信任调用方参数声明的身份。即使模型猜中完整工具名，`ensure_model_visible` 与调用授权双重拦截。
3. **执行授权**：消费者执行任务时使用 session 既有过滤链（`ToolFilterPolicy`、agent definition、skill 预载）；Node 提供的 `allowedTools` 等字段只能**收缩**、不能授予权限，与现状语义一致。

### 4.3 资源与订阅

| URI | 形态 | 内容 | 订阅 |
| --- | --- | --- | --- |
| `workflow://runs` | state | run 索引快照：`[{ runId, name, status, startedAt, progress 摘要 }]` | ✅ |
| `workflow://runs/{runId}` | state | 单 run 快照：状态、阶段、agent 统计、终态、limits 使用、`eventWatermark` | ✅ |
| `workflow://runs/{runId}/events` | event-log | progress 事件、journal 摘要、done 终态；单调 `sequence` + `headSequence` + 保留边界 | ✅ |
| `workflow://runs/{runId}/tasks` | event-log | 任务事件：`task.published` / `task.completed` / `task.failed` / `task.cancelled` | ✅ |
| `workflow://tasks` | event-log | session 级任务流（跨 run 聚合，消费主入口） | ✅ |

**发现与补读协议**（通知只是唤醒信号，不承担可靠投递）：

1. 消费者订阅 `workflow://tasks`；订阅确认后**立即 `resources/read` 建立基线**（全量扫描未终态任务），之后处理增量。
2. 内核保证「先可见后通知」：任务先进入任务表与事件日志，再发 `notifications/resources/updated`；消费者收到通知后 read，按 eventId 去重（幂等）。
3. 通知丢失/连接重建：重新 `subscriptions/listen` 后重复步骤 1 的全量扫描；扫描幂等，不重复执行已终态任务。
4. 事件日志读取使用稳定索引 URI + 分页参数（`?after={sequence}&limit=N`）；`headSequence` 为已提交上界；读取方发现游标早于保留边界时重新建立快照，不假装连续。
5. 清理：run 终态后保留窗口（对标 `cleanup_old_runs` 上限 50）；清理后 `workflow://runs/{runId}*` 返回 not-found，`workflow://tasks` 聚合仍保留其终态摘要直至窗口过期。

资源内容为不可信模型输入边界：事件日志正文不经模型上下文注入；TUI/ACP 投影由宿主读取资源生成（数据源单一，不新增第二事实源——内存投影 + journal 为真相）。

### 4.4 关闭

- 关闭键 `"WorkflowMiddleware"` 进 `BUILTIN_INSTANCE_POLICY_KEYS`；`closed_instances` 命中时实例不 spawn、桥接不建（与 workspace 同构）。`MIDDLEWARE_NAMES` 移除该键，`MIDDLEWARE_TOOL_NAMES` 移除 `"Workflow"`。
- 工具归一/过滤面（`tool_catalog`、`permission`）按实例身份自动生效（`mcp__workflow__*`）。
- 关闭与 policy 键迁移、装配切换、fork 排除**必须同批原子完成**，不得跨批次留下策略失效窗口。

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
3. 消费者收到唤醒 → read → 逐任务进入 `executing`：用会话执行依赖运行 `WorkflowAgentExecutor`（保留 workflow capability 投影、skill 预载、agent definition 解析、过滤链）→ 调 `WorkflowAgentResult` 回填。
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

变更：`agent/run` 请求/响应改为 Node→宿主 `agent/task`（通知）与宿主→Node `agent/result`（通知）。

`npm-packages/@peri-workflow/src/{adapter,rpc,types}.ts` 重做：adapter 维护 `taskId → pending` 等待表，等待 `agent/result`；超时与 abort 语义保持。`peri-workflow/src/{protocol,runner/message_loop}.rs` 分派改为任务登记/回填投递；Rust wire 类型与 npm `types.ts` 保持两侧同步注释。**wire 改造独立批次实施**，以 `WorkflowRuntime` 的领域接口为边界（先接口、后替换），不与 MCP 面切换捆绑。

### 5.5 消费者（WorkflowTaskConsumer）

- 装配于 `peri-acp/src/host/`，session 生命周期内持有；绑定 session 池与实例，唯一所有者持有执行依赖（executor 构造链从 `create_session_workflow_middleware` 迁移，保留 `create_executor(WorkflowAgentContext)` 全参数）。
- 就绪屏障：实例可先初始化，但 `Workflow` 工具在消费者订阅就绪前拒绝新启动（返回明确错误），避免「首个任务无人消费」。
- 关闭顺序（与 `session_close` 链路同序）：① 停止接受新 run 与新任务发布 → ② 取消或有界等待已登记任务（超时后强制收敛并记录）→ ③ 保留回填通道完成终态处理 → ④ 收割执行任务与 Node 进程 → ⑤ 释放订阅与池。

## 6. 会话装配与依赖注入

1. `BuiltinInstanceContext` 新增 workflow 槽位 `WorkflowInstanceInput`（对标 `WorkspaceInstanceInput`）：`cwd`、journal 根、`session_id`、执行依赖句柄（executor 工厂）、关闭位；注入时机与 workspace 同点（`assemble.rs` 池装配期，`run_initialize` 前）；会话关闭按 `close_builtin_task` 收敛。
2. 装配顺序：实例 spawn → 输入注入 → 消费者创建并订阅（就绪）→ 开放工具面；任何一步失败按会话初始化失败处理并回收已建资源。
3. ACP 面：`host/requests/workflow.rs` 四 handler 改为经池调用 `WorkflowStatus` / `WorkflowKill` / `Workflow`（resume = 携带 `resumeFromRunId` 调 `Workflow`，内核完成 state/journal 读取与启动，ACP 不拼装参数）。`McpPoolPort` 增加 `call_tool` / `read_resource` 端口面（或在 host 层使用 concrete pool 并登记豁免），作为前置工作；端口保留/删除与 wire 改造解耦。
4. ACP 调用的权限/错误映射保持现状语义：错误类型、排序、不存在 run 的行为进入兼容契约测试。

## 7. 迁移与删除清单

**新增**

- `mcp-packages/workflow/`（`WorkflowMcpServer`，形态对照 cron/workspace）：工具、资源、订阅、`WorkflowRuntime` 包装。
- `peri-acp-types/src/builtin_mcp.rs`：`WORKFLOW_TOOLS`、实例条目、保留名、`model_visible` 字段。
- `peri-middlewares/src/mcp/builtin/{dispatch,context}.rs`：handler 分支与 `WorkflowInstanceInput`。
- `WorkflowTaskConsumer`（`peri-acp/src/host/`）。
- Node wire：`agent/task` / `agent/result`（独立批次）。

**修改**

- `peri-middlewares/src/mcp/builtin/{mod,runtime}.rs`：overlay / spawn / supervisor 分支。
- `meta_harness.rs`：`"WorkflowMiddleware"` 迁 `BUILTIN_INSTANCE_POLICY_KEYS`；删 `MIDDLEWARE_TOOL_NAMES` "Workflow"；同步 `app.rs` 校验与相关测试（原子批）。
- `peri-agent/src/session/factory.rs`、`peri-middlewares/src/assembly.rs`：删 `ChainSlot::Workflow` 装配；`fork.rs`、`tool_search/core_tools.rs` 名称排除改实例身份（子 Agent 对 Status/Kill 的排除按实例整体处理）。
- `peri-acp/src/host/{mod,requests,requests/workflow,session_lifecycle,prepared}.rs`：状态源改为池/实例。
- `peri-workflow/src/tool.rs`：`WorkflowTool::invoke` 逻辑迁入 `WorkflowRuntime::start_run`（参数校验/GitBaseline/preflight/reserve/cancel 通道保留）。

**删除**

- `ChainSlot::Workflow` 及 arm、`WorkflowMiddlewareAdaptor`、`WorkflowMiddlewarePort`、`SessionState.workflow_middleware`、`peri-resources/src/workflow.rs` 门面、`WorkflowMiddlewareFactory` 端口链（由实例上下文替代）。

**实施顺序原则**：先落 MCP 面与 `WorkflowRuntime` 领域接口（保留旧装配，双轨可验证）→ 再原子切换装配/策略/删除旧链 → wire 改造独立批次（以接口为边界）。任一批次不得让启动、取消、恢复语义处于不可验证状态。

## 8. 验收

**功能与行为矩阵**

1. 工具面：`Workflow` 经 `mcp__workflow__Workflow` 可发现、可调用；`WorkflowAgentResult` 不出现在任何模型可见面（ToolSearch、ExecuteExtraTool、动态重建、提升路径）；关闭后实例不 spawn、工具不可见。
2. 资源面：run 索引/快照/事件/任务可读、可订阅；`updated` 通知在状态变化后到达；分页游标、`headSequence`、保留边界与清理语义正确。
3. 任务信道：发布 → 消费者执行 → 回填 → Node 收到结果；任务先可见后通知；通知丢失后重建订阅可完整发现（扫描幂等、不重复执行）；kill 各竞态按 5.3 收敛；迟到回填幂等。
4. 并行与限制：两个任务可同时进入执行；达到 `maxConcurrency`/limits 时排队与取消正确；limit 计费与现状一致。
5. 身份与安全：跨 session 回填被拒绝；任务参数不能扩大工具权限；模型猜中 host-only 工具名不能调用。
6. 会话生命周期：双会话并发 run、任务、执行上下文互不串线；关闭顺序无残留（无 detached 执行、无未结算 promise）；就绪屏障有效。
7. 兼容回归：MetaHarness 关闭键、ACP 方法行为、TUI 面板；resume cache、Git postcondition、`writeIntent`、limits、long-output 提取、ADLC 检查产物与既有语义一致；启动失败释放 reservation、启动/取消竞态无僵尸 Node 进程。
8. 边界与验证：`peri-resources` 不再依赖 workflow；删除清单完成后全仓编译、契约测试、`scripts/check-layer-imports.sh` 通过；`git diff --check` 干净。

## 9. 待裁决记录

- journal 落盘归属（工作区工具环境 vs Session Store）：保持现状，独立裁决（`2026-10-11-module-dependency-boundary-audit.md` 第 2 项）。
- Node 引擎独立分发为外部 MCP server：本设计在同语义上保留升级路径（`agent/task`/`agent/result` ↔ 标准资源/工具），不在本范围实施。
