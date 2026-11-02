# Workflow 系统设计

**状态**：现行设计
**范围**：workflow 子系统运行内核——脚本与 Node wire、runner 与进程所有权、agent 任务与进度、journal/state 落盘、resume/kill、TUI 投影、CLI 与 artifact 分发。
**接入语义**：MCP 实例（工具/资源/订阅）、任务信道（`agent/task` + `agent/result`）、会话装配与关闭以 [workflow-mcp.md](workflow-mcp.md) 为权威；本文不重复其设计，只保留运行侧事实。
**代码事实源**：`peri-workflow/`、`mcp-packages/workflow/`（crate `peri-mcp-workflow`）、`npm-packages/@peri-workflow/`、`peri-acp/src/host/{workflow_agent,workflow_consumer,workflow_projection,requests/workflow}.rs`、`peri-js-runtime/`；定位入口见 [peri-workflow 代码索引](../code-index/peri-workflow.md)。

## 1. 概述

Workflow 是 Peri 的多 Agent 编排子系统：用户在 JS 脚本中声明并行、流水线、分阶段的多 Agent 计划，脚本在独立 Node.js 进程（引擎）中运行，经 stdio NDJSON 与宿主双向 JSON-RPC 通信。职责边界：

- **宿主拥有 agent 执行权**：LLM 调用、工具执行、权限与审批由 Peri 侧完成；
- **引擎拥有编排权**：agent 之间的并行、流水线与阶段推进由脚本与 `workflow-engine` 决定。

能力以 builtin MCP 实例 `workflow` 接入会话；实例 handler 背后的唯一控制面是领域接口 `WorkflowRuntime`（`peri-workflow`），Node wire 是该接口的实现细节（[workflow-mcp.md](workflow-mcp.md) §3）。

### 1.1 脚本与核心原语

脚本是 **AsyncFunction body**，不是普通 ESM 源码：引擎剥离唯一 `export const meta = { name, description }` 后执行函数体；禁止 `import`、`export default` 与其他 `export`，结果由顶层 `return` 给出。函数体顶部直接注入以下原语与 `args`、`budget`：

| 原语 | 语义 |
|------|------|
| `agent(prompt, opts)` | 调度一个 agent 执行；`opts` 含 `label`/`phase`/`model`/`maxTokens`/`allowedTools`/`agentType`/`isolation`/`schema` 等 |
| `parallel(thunks)` | 并发执行零参工厂函数数组（必须写 `() => agent(...)`，不能传已调用的 promise） |
| `pipeline(items, ...stages)` | 对每个元素顺序执行全部 stage，`stage(prev, item, index)` 返回下一 stage 的 `prev` |
| `phase(title)` | 切换阶段标记，后续 agent 自动归属该阶段 |
| `log(message)` | 运行日志 |
| `workflow(nameOrScriptPath, args?)` | 运行子 workflow（一层嵌套上限；按名解析 `.claude/workflows/`） |

参数契约（含 `scriptPath`）与用法校验归 `WorkflowRuntime` 的 `params` 单一权威；`strictPreflight` 恒拒绝——workflow 原语与调用图不可静态验证。面向模型的手册是 builtin skill `ultracode`（见 §13）。

### 1.2 设计目标

- **异步 fire-and-forget**：工具调用立即返回 `run_id`，workflow 后台运行，完成后经完成通知唤醒主循环。
- **SubAgent 复用**：workflow 内 agent 复用 v2 `run_react_loop` 与冻结会话数据，不引入第二套执行栈。
- **单一任务信道**：agent 委托表达为「任务发布（资源）+ 结果回填（工具）」，不新增私有 server→client 请求语义（[workflow-mcp.md](workflow-mcp.md) §5）。
- **可观察性**：progress reducer、run 事件日志、资源订阅与 TUI 轮询；完成结果进入 MessageQueue 唤醒 Receive。
- **可恢复**：journal + `state.json` 支持 resume 与缓存复用；不恢复旧 Agent 执行。

---

## 2. 运行架构

### 2.1 组件与依赖方向

```
模型面（peri-agent）
  mcp__workflow__{Workflow,WorkflowStatus,WorkflowKill}（deferred 桥接）
宿主面（peri-acp host）
  ACP workflow/* handler（经池调用实例）· WorkflowTaskConsumer · WorkflowProjection · 会话装配
实例面（peri-mcp-workflow）
  WorkflowMcpServer：tools / resources / subscriptions（协议映射，业务语义委托 runtime）
领域面（peri-workflow）
  WorkflowRuntime：runner / registry / progress / journal / 任务表 / 事件日志 / 订阅广播
传输面（peri-js-runtime）
  JsExecutionHost + RpcChannel：NDJSON、pending、stderr、取消
引擎（npm-packages/@peri-workflow，per-run 子进程）
  RPC/adapter + @claude-code-best/workflow-engine（零修改复用）
```

依赖方向自下而上：引擎与传输面不感知会话；领域面不依赖 ACP 与 middlewares；实例面只做协议映射；宿主面负责会话装配与资源消费。

**端口归属**：`WorkflowRuntime` 由会话装配创建（`peri-acp/src/host/workflow_agent.rs` 的 `create_session_workflow_chain`），经实例槽位绑定给 builtin `workflow` 实例（`peri-mcp-workflow` 的 `WorkflowInstanceInput`）；MCP 面与宿主消费者共享同一份状态，绑定首次为准，跨实例冲突是可观察的装配错误。

**通信协议**：宿主 ←→ 引擎经 stdin/stdout 双向 JSON-RPC 2.0（NDJSON，每行一条完整消息）。通用 framing、pending 登记与进程停止语义在 `peri-js-runtime`；workflow 特有 method 语义在 `peri-workflow/src/protocol.rs` 与 `runner/message_loop.rs`。

### 2.2 Node wire

| 方向 | 方法 | 形态 | 用途 |
|------|------|------|------|
| 宿主 → Node | `workflow/start` | 请求（15s 超时） | 启动脚本；ack 校验协议版本与 build id |
| 宿主 → Node | `workflow/kill` | 请求 | 终止 run |
| Node → 宿主 | `agent/task` | 通知 | 发布 agent 任务（`taskId` + 运行参数） |
| Node → 宿主 | `progress/event` | 通知 | 进度事件流（阶段 / agent 状态与统计） |
| Node → 宿主 | `journal/append` / `journal/truncate` | 通知 | 增量写入 / resume 截断 agent 日志 |
| Node → 宿主 | `log` | 通知 | 运行日志；级别映射与诊断保真见 `ARC-SECRET-001`（消费点差距见 §11） |
| Node → 宿主 | `workflow/done` | 通知 | 终态：status / returnValue / error |
| 宿主 → Node | `agent/result` | 通知 | 回填任务结果（按 taskId 幂等结算） |

类型约定：两侧 DTO 均为 camelCase；`peri-workflow/src/protocol.rs` 与 `npm-packages/@peri-workflow/src/types.ts` 必须同步修改。任务发布/回填的状态机、幂等与取消语义以 [workflow-mcp.md](workflow-mcp.md) §5 为权威，`npm-packages/@peri-workflow/DESIGN.md` 记录引擎侧协议细节。

---

## 3. 运行内核（peri-workflow）

### 3.1 WorkflowRuntime

领域控制接口（`peri-workflow/src/runtime.rs`），编排能力的唯一权威：

| 操作 | 说明 |
|------|------|
| `start_run` / `start_prepared_run` | 参数解析与校验、准入、并发槽、后台任务登记与执行提交（§5.1） |
| `kill_run` / `kill_agent` | 任务表栅栏 + 取消 + registry/引擎收敛（§6） |
| `resume` | 读 `state.json` 与 journal 后按 resume 重启（§7） |
| `runs_snapshot` / `run_snapshot` / `event_watermark` | 资源与 ACP 投影的快照读取 |
| `subscribe` / `run_events` | 订阅唤醒信号与 run 事件日志（资源面数据源） |
| `bind_consumer_token` / `verify_consumer_token` / `set_consumer_ready` | 回填调用授权与就绪屏障（[workflow-mcp.md](workflow-mcp.md) §4.2/§5.5） |
| `mark_closed` / `is_closed` | 关闭位：拒绝新 run 与迟到任务发布 |

内部状态：`WorkflowRunner`、`WorkflowTaskRegistry`、`WorkflowProgressStore`、`WorkflowJournalStore`、`WorkflowTaskTable`、run 事件日志、订阅广播、run 执行预算表（`RunExecutionBudget`）与 `CloseGate`。

### 3.2 Runner 与 JS 执行宿主

`WorkflowRunner::run`（`peri-workflow/src/runner.rs`）拥有单次 run 的完整所有权：

1. `registry.reserve` 与后台任务登记已在 `start_prepared_run` 完成；runner 在独立执行任务内运行（`RunCompletion::spawn`）。
2. 脚本 preflight：引擎 `validate` 子命令在临时目录校验脚本；临时目录与 kill-on-drop 子进程由 preflight 持有，任何退出路径都不残留。
3. artifact 准备与 spawn：解析固定版本引擎包（§10），无可用路径时 `SpawnFailed`（离线 fail closed）。
4. `workflow/start` 握手：15 秒超时；ack 校验（协议版本、build id）失败即失败启动并回收。
5. 消息循环（`runner/message_loop.rs`）按 §2.2 分派；`maxElapsedMs` 由循环内的有界等待强制，超限或资源限额终止循环并投影失败终态。
6. 终态：`workflow/done` 或 kill → `runner/terminal.rs` 收尾（Git postcondition、`state.json`、progress 投影）→ 结果经完成通道发布。
7. 清理：终止子进程、移除 active channel、`cleanup_old_runs()`、`cleanup_completed()`。

**RunScope**（`runner/scope.rs`）：run 内子 future 的归属与其排空；取消必须等待全部子 future 结束，外加 JS host 与 stdout reader 退出。无法证明排空时返回 `CleanupFailed`，会话 owner 保持未结清。

**进程诊断**：Node stderr 由 JS host 并行消费；run 终态携带其末尾若干行 tail，用于向调用方报告脚本加载/启动类快速失败。

**取消收敛**：kill 分支在有界超时内发送 `workflow/kill`、终止子进程并写入 `killed` 终态；kill 后 `killed` 是唯一终态，message loop 或 EOF 不得覆盖为 `failed`（`ARC-WORKFLOW-RPC-001`）。

### 3.3 任务表与 agent 回调

`agent/task` 通知经 `AgentTaskSink`（装配期注入）登记为任务表记录（`WorkflowTaskTable`）：

- **publish**：`taskId` 唯一；实例关闭返回 `PublishOutcome::Closed`；超 `maxAgents` 返回限额错误并使 run 以限额错误终止。
- **拒绝路径不留悬挂 promise**：参数非法（能取到 taskId）、任务信道未接线、实例关闭时就地回投 `agent/result`（`dead` 编码）。
- **claim / settle**：消费者唯一认领并登记取消句柄；终态单次提交，重复回填幂等。
- **投递**：终态提交后向 Node 发送 `agent/result`；取消/中止按现有 `AgentRunResult` 变体编码（Node 侧映射为 `WorkflowAbortedError`）。

任务表是 per-session 运行期状态（process-local，不写 journal）；消费主循环、订阅与就绪语义见 [workflow-mcp.md](workflow-mcp.md) §5.5，入口为 `peri-acp/src/host/workflow_consumer.rs`。

结果与进度类型是契约层单一事实源（`peri-acp-types/src/workflow.rs`）：`AgentRunParams`、`AgentRunResult`（`Ok { output, usage, model, … }` / `Skipped` / `Dead { reason, detail }`）、`ProgressEvent`、四维状态与 `WorkflowTaskResult`；`AgentRunResult` 的 schema/字符串编码由执行体投影统一决定。

### 3.4 WorkflowTaskRegistry

`peri-workflow/src/registry.rs`：session 级运行注册表。

| 方法 | 语义 |
|------|------|
| `reserve` | 并发限流检查（默认上限 3，`with_max_concurrent` 可调）后登记 run（含 kill 通道） |
| `attach_child` | 绑定执行任务句柄；记录已被取消时不 abort，kill 信号仍拥有清理证明 |
| `complete` | 更新 run 状态，写入完成 outbox 并发出内部唤醒（保留历史供查询与调试） |
| `kill` | 触发 kill 通道并移除记录，返回是否命中运行中的 run；进程与任务清理由 runner 收敛 |

run 终态进入 registry 的完成 outbox（`pending_completions` / `acknowledge_completion`），由宿主 `WorkflowProjection` 经 `workflow://background-tasks` 读取并按 `WorkflowCompletionAck` 确认；TUI 后台任务视图消费同一外部任务投影。

### 3.5 WorkflowProgressStore

`peri-workflow/src/progress.rs`：基于 reducer 的内存投影。

- `apply_event(event)` 按 `ProgressEvent` 变体更新 `RunProgress`（阶段、agent 状态与统计、run 终态）；按 `agent_id` 精确匹配而非栈顶，避免并发交错漂移。
- 单一提交器（内部串行锁）保证事件顺序；`ProgressObserver` 在提交后回调，**不得重入提交**，runtime 经它记录 run 事件日志并发订阅唤醒。
- `RunProgress` 携带 `started_at_ms`（run 索引稳定排序依据）与四维终态字段；`agents` 以 JSON 数组序列化，供 TUI DTO 消费。
- 完成保留期 300 秒后 `cleanup_completed()` 回收；与 journal 磁盘保留（50 个 run 目录）相互独立。

### 3.6 WorkflowJournalStore

`peri-workflow/src/journal.rs`：`.claude/workflow-runs/{run_id}/` 结构化落盘。

```
.claude/workflow-runs/{run_id}/
├── script.js        # 脚本副本（resume 与审计）
├── journal.jsonl    # append-only：每行一条 { key, seq, result }
├── outputs/         # 长文本外置：return_value 中超过 200B 的字符串写文件，原值替换为标签
└── state.json       # 终态快照（先写 .tmp 再 rename 的原子写入）
```

- `key` 是 agent 参数的 SHA256（resume cache-hit 匹配依据），`seq` 是调用顺序号。
- `state.json` 保存 status、四维状态、`write_intent`、limits、`args`、`max_concurrency`、attempts 与 `return_value`；写入失败必须使对外结果降级为 failed/blocked。
- 保留策略：按 mtime 保留最新 50 个目录，run 结束时清理。
- resume 读取使用 `read_all_strict`：缺失、损坏或序号不连续/重复一律报错，不降级为无缓存新运行。
- 调用方读取长结果必须跟随 `outputs/` 占位符，不把 agent 全文作为顶层返回值。

`journal/git.rs`（`GitBaseline`）与 `journal/output.rs`（长文本提取）同属该模块边界。

---

## 4. Agent 执行体（WorkflowAgentExecutor）

`peri-agent/src/agent/workflow/`：把 `agent/task` 的运行参数转换为一次完整的 ReActAgent 执行并产出 `AgentRunResult`。

| 方面 | 主 Agent | Workflow agent |
|------|----------|----------------|
| 中间件链 | 生产链蓝本 | 专用 workflow 链（`build_workflow_middlewares`）；不按主链推断能力 |
| Frozen data | 完整 | 透传（frozen 指令 / skills 摘要 / 语言 / MetaHarness） |
| System prompt | 冻结 base + request-time contribution | 用同一份冻结输入按 workflow 链能力事实投影重建 |
| 模型 | 用户选择 | 跟随 session provider；脚本 `model` 先按 alias 解析，失败按字面量使用 |
| max_iterations | 会话配置 | agent definition 的 `max_iterations`，否则默认 200 |
| 审批 | 完整 | 仅 broker + permission_mode 齐备时启用；生产构造点为 None ⇒ 禁用 |
| 工具 | 会话工具视图 | 既有工具集 + builtin 实例 direct bridge（Web / Artifact / workspace 工具）；无 ToolSearch/deferred |
| Langfuse | 完整 | 经执行体 hooks 与事件旁路处理器；会话级 `publish_hook=None`（workflow 事件不进协议化事件流，统一发射接线另议） |

执行体复用 v2 `run_react_loop`、`DefaultSubagentV2ContextBuilder` 与冻结资源授权；工具与中间件装配经 `WorkflowAgentFactory` 端口注入（peri-middlewares 实现、宿主装配点注入），ACP 侧 `host/workflow_agent.rs` 只保留装配薄壳与注入面构造 helper。

链内条件注册（受 MetaHarness 关闭位约束）：`AgentsMdMiddleware`、`SkillsMiddleware`、`SkillPreloadMiddleware`、`GitAttributionMiddleware`、`TodoMiddleware`、`PermissionMiddleware`、`HumanInTheLoopMiddleware`；自动 compact 由 v2 `stages` 统一接管，链上不装配 compact middleware。文件与终端工具不再由链提供，改由 builtin 实例 direct bridge 提供。结构化结果按 schema 做有限子集校验，不宣称完整 JSON Schema。

---

## 5. 启动、完成与通知

### 5.1 启动准入

`start_prepared_run` 的固定顺序（任一步失败不留 detached 执行）：

1. **关闭门**：`CloseGate::admit` 与 run 注册同临界区；`mark_closed` 等待在途准入结束后不再放行新 run。
2. **执行 owner**：`ExecutionOwner::admit` 经统一后台任务注册表登记外部执行；未启动即释放。
3. **并发槽**：`registry.reserve` 先于 spawn，失败不产生孤儿 run。
4. **预算登记**：`RunExecutionBudget`（maxConcurrency / maxAgents / maxToolCalls）先于 Node 进程登记，供消费者容量判定与 wire 准入共用。
5. **后台任务登记**：`BgTaskKind::Workflow` + kill 闭包（取消转发到任务表与 registry）；登记失败回滚并发槽。
6. **执行任务**：`RunCompletion::spawn` 启动唯一执行任务并 attach 句柄；调用方在 1 秒快速窗口观察同一终态，调用者取消等待不取消执行。

参数校验（`params::parse_start_input`）：`maxConcurrency` 默认 3、上限 16；`scriptPath` 限定在 workflow cwd 内（canonicalize 后读取，消除 check-then-read 竞态）；`strictPreflight` 恒拒绝；limits 使用 JSON 安全整数上界；`writeIntent` 校验并捕获 `GitBaseline`。Git write intent 是声明式后置条件，不是权限边界或文件系统沙箱。

### 5.2 完成与终态投影

- **execution**：由 run 状态投影（completed / failed / killed）。
- **acceptance**：语义验收不由运行内核判定；内核当前投影为 `unknown`，验收结论属 ADLC/skill 层（`ADLC-DELIVERY-STATUS-001`，[Ultra-ADLC 设计](ultra-adlc.md)）。
- **post-processing**：Git postcondition 结果——read_only 通过为 `not_required`，write 通过为 `passed`，失败或缺少可验证基线为 `failed`/`blocked`。
- **delivery**：内核只投影 `blocked` / `unknown`——execution 完成、acceptance 非 failed、存在 write intent 且 post-processing 通过时为 `unknown`，其余为 `blocked`；`deliverable` 判定属引擎之外的验收层（ADLC/skill），内核不产出。

Git postcondition 只比较 Workflow 前后发生变化的路径，已有且未变化的无关 dirty path 不阻塞；异常只报告并 blocked，不执行 add/commit/stash/reset/restore/clean。`state.json` 写入失败使对外结果降级 failed/blocked——不能继续通知 completed。

### 5.3 通知与投递

`RunCompletion::project` 结算外部执行证据后写入 registry 完成 outbox，并发出仅供唤醒的内部通知：

- **Agent 面**：宿主 `WorkflowProjection` 扫描 `workflow://background-tasks`，把未确认终态经 `TaskManager::restore_external_terminal` 投影为会话后台完成并唤醒 Receive；结果先交付会话，再调用鉴权的 `WorkflowCompletionAck`，断线与拒收不确认，重复扫描按稳定终态身份去重。
- **TUI 面**：后台任务完成事件（`BgTaskKind::Workflow`）。
- **状态词**：`completed` / `killed` / `failed`；delivery blocked 时不得显示成 completed。

投递契约、去重与恢复规则见 [Session 异步任务架构](session-async-tasks.md)。

---

## 6. Kill 与取消

| 层级 | 入口 | 实现 |
|------|------|------|
| 整个 run | `WorkflowKill {runId}` / ACP `workflow/kill_run` / TUI 面板 Enter / 后台任务 kill 闭包 | 任务表 `cancel_run`（栅栏 + 取消全部未终态任务，含并发到达的发布）→ registry `kill` → runner kill 分支：有界超时内 `workflow/kill` RPC → 终止子进程 → 写入 `killed` 终态 |
| 单 agent | `WorkflowKill {runId, agentId}` / ACP `workflow/kill_agent` | 任务表 `cancel_agent` + 引擎侧 `kill_agent` 兜底（同一条任务表路径） |
| 用户 cancel（会话关闭等） | 取消令牌 | 经后台任务 kill 闭包转发，与 run kill 同路径 |

顺序固定：先置任务表栅栏并取消，再发 registry kill——kill 生效前发布的迟到任务命中栅栏，不逃逸出本轮 kill。取消必须等待实际排空，无法证明排空返回 `CleanupFailed`。已完成 run 的 kill 返回未命中（不是「终止了运行中的 run」），仅清理留档。

---

## 7. Resume

两条路径共用 `resume_from` 机制：

1. **resume-only**（`resumeFromRunId` 且无 `script`/`scriptPath`）：内核读 `state.json`（校验 run_id 与请求一致、拒绝路径穿越与非 UUID 目录名、拒绝仍在运行的状态）与 journal，按原 `args`/`max_concurrency`/limits 重启。该形态只接受 `resumeFromRunId`（`strictPreflight:false` 为允许的 no-op；`strictPreflight:true` 与 start 同文案拒绝）；`args`、`maxAgents`、`maxToolCalls`、`maxElapsedMs`、`budgetTotal`、`maxConcurrency`、`name`、`writeIntent`、`script`/`scriptPath` 等启动期字段一律显式拒绝，不会被静默忽略——改变输入须携带完整 script（或 `scriptPath`）+ `resumeFromRunId` 走 journal 重放路径。
2. **journal 缓存重放**（携带 script 的 `resumeFromRunId`）：`read_all_strict` 读取上一 run journal，仅把从 `seq=0` 开始的连续 `ok` 前缀交给 Node；首个 dead/skipped 及后缀重新执行，Node 按调用 key 匹配前缀。

- 缺失、IO 错误、损坏 JSON、序号不连续/重复均报错，不得降级成无缓存新运行；序号损坏需依据工作包 checkpoint 显式重新规划。
- Node 仍按调用 key 匹配前缀，不能用调用缓存代替工作包依赖与产物有效性检查。
- `state.json` 保存启动 `args` 与并发数并原样恢复；legacy 缺失参数不可重建，并发兼容默认 3；改变契约或恢复计划时通过 `Workflow` 工具显式传完整新 script（或 `scriptPath`）+ `resumeFromRunId`。
- 现有 budget/limits 是物理运行上限，不是跨运行计费账本；cache-hit 不重复计数。

---

## 8. 生命周期

| 组件 | 作用域 | 创建 | 销毁 |
|------|--------|------|------|
| builtin `workflow` 实例 | session | 会话装配（池） | session end；命中关闭集则不 spawn |
| `WorkflowRuntime` | session | 会话装配（`create_session_workflow_chain`） | session end |
| `WorkflowTaskConsumer` | session | 装配后订阅 `workflow://tasks` | session end（取消 + 有界排空） |
| `WorkflowProgressStore` / `WorkflowTaskRegistry` / `WorkflowJournalStore` / `WorkflowRunner` | session | 随 runtime 构造 | session end |
| `RpcChannel` 与 run 任务表记录 | per-run | runner 启动 | run 结束或 kill |
| Node 子进程 | per-run | runner spawn | 正常退出、kill 或崩溃 |
| `WorkflowProjection`（完成投递） | per-session | 装配后轮询 `workflow://background-tasks` | session end；未确认结果保留至实例清理 |
| TUI 快照轮询 | 进程（有会话时） | 启动轮询任务 | CancellationToken 取消 |

---

## 9. TUI 集成

### 9.1 WorkflowPanel

```
┌─ Workflows ──────────────────────────────────────────┐
│ [▶ review:verify @parallel] [✓ audit @pipeline]       │  ← Run Tab 行
├──────────────┬────────────────────────────────────────┤
│ Phases (30%) │ Agents (70%)                           │
│ ✓ Review     │ #0 coder ✓ 500t 3tools                 │
│ ▶ Verify     │ #1 review:bugs ▶ 120t 1tool            │
│ ○ Deploy     │ #2 review:perf ⊘ dead                  │
└──────────────┴────────────────────────────────────────┘
```

布局：顶部 Run Tab + 双栏（Phases 30% / Agents 70%）。快捷键：`Tab`/`Shift+Tab` 切换 run，`←`/`→` 切换栏位，`↑`/`↓` 导航，`Enter` 终止选中 run（`workflow/kill_run`），`Esc` 关闭。面板是运行状态投影，不拥有 workflow lifecycle；kill、resume、journal 与终态仍由 runtime/registry 决定。面板操作限于终止运行，resume 经 ACP `workflow/resume` 提供；footer 只展示快捷键，不渲染四维状态。

### 9.2 快照与轮询

`peri-tui/src/kit/workflow_snapshot.rs`：DTO 镜像 `RunProgress`（含四维状态字段，legacy 缺省为 `unknown`），经 `WORKFLOW_SNAPSHOT` atom 供面板订阅。数据获取是 **Pull 路径**：后台任务按 2 秒间隔调用 ACP `workflow/list_runs`（handler 经会话池读取 `workflow://runs`）；无 push 路径，无 session 时写入空快照，使面板从 loading 过渡到空态。

---

## 10. CLI 与 artifact 分发

- **CLI**：`peri workflow <子命令>` 在配置与会话初始化之前分发（`peri-tui/src/cli_workflow.rs` → `peri_acp::workflow_cli::run` → `peri-workflow/src/cli.rs`），子命令经白名单校验后使用同一份已校验 artifact 运行。
- **artifact 解析**（`runner/artifact.rs`）：显式注入目录（`PERI_WORKFLOW_ARTIFACT_DIR`）→ 本机缓存 `~/.peri/workflow/<version>/<内容身份>/` → 随包 sidecar（可执行文件旁 `peri-resources/workflow/<内容身份>/`）。校验包身份（name/version/main/协议/buildId）、入口不越界与入口 SHA-256；本地来源均不可用时默认离线 fail closed。
- **网络获取**：仅显式 `PERI_WORKFLOW_ALLOW_NPX_FALLBACK=1` 允许一次固定版本 registry 获取，获取内容仍须通过同一身份与摘要校验；不存在直接执行网络内容的路径。
- **红线**：Rust 侧零 JS 嵌入（`ARC-WORKFLOW-DIST-001`，[architecture-contracts.md](../standards/architecture-contracts.md)）；分发缺口不得以嵌入 JS 产物解决，表述以 [workflow-mcp.md](workflow-mcp.md) §2.3 为准。
- **引擎包**：自述协议与 CLI 子命令（`read`/`list`/`validate`/`boundary`/`adlc`）以 `npm-packages/@peri-workflow/{README,DESIGN}.md` 为准；`dist/` 为构建产物（`bun run build` 可复现字节）、不入库，由发布链在打包前构建并随发布包携带，消费方仍按固定 SHA-256 pin 校验。

---

## 11. 错误处理策略

| 层级 | 错误 | 处理 |
|------|------|------|
| 工具分派 | 参数结构错误、未知工具、调用授权失败 | 协议级 JSON-RPC error（`invalid_params`）；业务失败保留工具级 `is_error` 文本 |
| 启动 | 并发限流、关闭、未就绪、脚本校验失败 | 工具级错误；快速窗口内返回诊断文本 |
| Runner | artifact 缺失/摘要不符/握手失败 | `SpawnFailed`；启动失败先移除注册项并释放准入 |
| wire | 协议帧错误、资源限额 | 终止消息循环并投影失败终态 |
| Node 崩溃 | stdout 关闭 / EOF | drain pending，pending 请求收到错误；不覆盖 `killed` 终态 |
| Agent 执行 | ReActAgent 异常 | 转为 `AgentRunResult::Dead`，不中断 workflow |
| 取消 | 无法证明排空 | `CleanupFailed`，会话 owner 保持未结清 |
| Journal | `journal/append` 写失败 | 记 warn，不中断执行 |
| 持久化 | `state.json` 写失败 | 对外结果降级 failed/blocked，不再通知 completed |
| Node `log` | 级别与正文未按契约进入诊断出口 | **实现差距**：当前消费点只记固定 debug 信号，低于 `ARC-SECRET-001` 的跨层诊断保真要求；修复跟踪见 `spec/issues/2026-10-07-architecture-documentation-drift-audit.md`（P2-02） |
| 通知 | 消费者滞后 / 投递失败 | 记 warn 后继续；投递失败保持未结清，由后续投递或会话关闭收敛 |

---

## 12. 关键设计决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 脚本执行环境 | 独立 Node.js 进程 | 编排进程与宿主隔离生命周期并复用 workflow-engine；这不是 OS 级安全 sandbox |
| 接入形态 | builtin MCP 实例 | 工具/资源/订阅/关闭走标准 MCP 面；会话内对象（frozen、provider、执行依赖）同进程传递（[workflow-mcp.md](workflow-mcp.md) §3） |
| agent 回调 | 任务资源 + 工具回填 | MCP 的 server→client 请求语义是「生成/询问」，不匹配「委托执行完整 agent」 |
| run_id 生成 | 启动入口（runtime） | 立即返回 LLM，不等待子进程启动 |
| 通知模型 | Defer 进入 MessageQueue | 避免 push 打断正在执行的 agent；结果先入队再结清 active count |
| 完成通知 | Session 任务入口 | TUI 与 Agent 的投递契约见 [Session 异步任务架构](session-async-tasks.md) |
| progress 消费 | 资源订阅 + TUI 轮询 | 宿主消费者用订阅唤醒；TUI 维持 2s 拉取，不新增 push 管线 |
| agent 执行 | 专用 workflow 链 | 复用 frozen data、工具与 LLM 基础设施，同时显式限制子代理、提问等能力 |
| 并发限制 | Registry 上限 3 + 任务表容量 | 防止无限 spawn；消费者与 wire 准入同源取值，不另立计数 |
| journal 保留 | 最多 50 个 run 目录 | 避免磁盘膨胀，同时保留足够历史供 resume 和调试 |
| system prompt | 冻结 base + request-time contribution seam | 遵循 `ARC-FROZEN-001` 与 `ARC-SERIAL-001`，不把 workflow 状态写入缓存前缀 |
| 完成语义 | 四维状态独立投影 | execution completed 不等于可交付；不把 blocked 显示成 completed |

---

## 13. 能力暴露与关闭

- **工具面**：`Workflow` / `WorkflowStatus` / `WorkflowKill` 经 MCP 桥接成为 deferred 工具（`mcp__workflow__*`，经 `SearchExtraTools → ExecuteExtraTool` 发现与调用）；`WorkflowAgentResult` 是 host-only 回填通道，不进入模型可见面。工具与资源声明以 [workflow-mcp.md](workflow-mcp.md) §4 为权威。
- **关闭**：策略键 `"WorkflowMiddleware"`（沿用旧链槽位名，用户配置兼容）进 `BUILTIN_INSTANCE_POLICY_KEYS`；workflow 是唯一物理关闭特例——命中关闭集时不 spawn 实例、不建桥接、宿主消费者不启动（`ARC-CAPABILITY-CLOSURE-001`）。
- **模型手册**：builtin skill `ultracode`（`mcp-packages/workspace/src/resources/builtin/skills/ultracode/SKILL.md`，宿主注册表经 `include_str!` 引用），按需加载，不常驻系统提示词；复杂全生命周期交付由 `ultra-adlc` skill 与 [Ultra-ADLC 设计](ultra-adlc.md)约束。
- **入口命令**：`/workflows` 打开面板（`PanelKind::Workflow`）；ACP 方法名（`workflow/list_runs`、`workflow/kill_run`、`workflow/kill_agent`、`workflow/resume`）与 TUI 交互保持不变。
