# peri-workflow 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-08-16
> 依据：docs/design/workflow-system.md（v3.0，已与代码同步）、docs/standards/architecture-contracts.md、源码（无 crate 级 CLAUDE.md）

## 架构速览

- 定位：多 Agent 编排子系统——用户 JS ESM 脚本在独立 Node.js 进程运行，经 stdio 双向 JSON-RPC（NDJSON）与主进程通信（lib.rs:1-4）；agent() 回调复用 v2 `run_react_loop`
- 数据流：`WorkflowTool::invoke → registry.register（并发限流）→ runner.run spawn Node → RpcChannel（stdin/stdout）→ agent/run 回调 AgentExecutor → 完成 → done_tx → notification_task → registry.complete 广播 → session 级 consumer（peri-agent/src/session/exec/executor.rs）→ TUI 通知（bg-task-completed）+ Defer 注入（唤醒新 turn）`
- 入口：`src/tool.rs:111` 的 `WorkflowTool::invoke`（fire-and-forget，立即返回 run_id）；`src/runner.rs:223` 的 `WorkflowRunner::run`（子进程生命周期 + 消息循环，:399）
- 稳定不变量：协议/契约类型事实源在 `peri-acp-types/src/workflow.rs`——`AgentExecutor`（:348）、`AgentRunParams`（:13）、`AgentRunResult`（:38）、`ProgressEvent`（:96）、`WorkflowRunStatus`（:269）、`WorkflowTaskResult`（:290，`to_notification` :306）；本 crate 仅 re-export（protocol.rs:40、registry.rs:10），改 wire 类型必须动 peri-acp-types + npm 侧
- 关键决策不变量：run_id 由 tool.invoke 生成（UUID v7，GAP-02）；并发上限 3（registry.rs:41-46）；journal 保留 50 个 run 目录（journal.rs:18）；通知双路径（TUI 实时 + Agent 文本感知），无 inbox 时回退直 push MQ（无 wake）
- 端口化装配：session 级 `WorkflowMiddleware`（`peri-middlewares/src/workflow/mod.rs:36`，`WorkflowMiddlewarePort` 实现 :338，Adaptor :332）经 `peri-acp-types/src/ports.rs:96` 端口注入，装配宿主 `peri-acp/src/host/workflow_agent.rs:192`（create_session_workflow_middleware）；执行体 `peri-agent/src/agent/workflow/agent.rs:141`

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改工作流定义/脚本执行 | `src/protocol.rs`（`WorkflowStartParams` :17、`WorkflowDoneParams` :55、`JournalEntry` :45）+ `src/runner.rs` | `WorkflowRunner::run`（runner.rs:223）；消息循环（:399，外层 `tokio::select!` 竞速 kill_rx :707） | wire 字段与 npm 侧 `npm-packages/@peri-workflow/src/types.ts` 必须两侧同步（protocol.rs:6-8）；`workflow/start` 15s 超时（runner.rs:347）；`workflow/done` → break + 写 state.json（runner.rs:585-632）；kill 分支：5s 超时 `workflow/kill` RPC → child.kill → msg_loop.abort（防覆写 state.json）→ 写 killed state.json → done_tx（runner.rs:707-735） |
| 改 agent 装配/执行回调 | 契约 trait `peri-acp-types/src/workflow.rs:348` `AgentExecutor`；执行体 `peri-agent/src/agent/workflow/agent.rs:141`（WorkflowAgentExecutor，透传 frozen data）；本 crate 侧 re-export（runner.rs:122） | 消息循环 `agent/run` 分支（runner.rs:426）；`parse_agent_run_params`（:124，runId 必须匹配 :130） | 注册在 spawn 前（GAP-07 原子化，runner.rs:456）；kill 后不得发成功响应（owned token 校验 + was_killed 门控，runner.rs:494-528）；phase 从 progress store 补注（runner.rs:498-505）；其他 Dead 变体仍须正常响应，防 Node Promise 永远 hang（runner.rs:512-519） |
| 改工具调用（WorkflowTool） | `src/tool.rs` | `BaseTool` impl（tool.rs:56）；`invoke`（:111）；`with_bg_registry`（:49） | deferred tool（未覆写 `is_direct`，默认 false，ARC-TOOLS-001）；scriptPath 限定 cwd 内（`resolve_script_path` :419）；resumeFromRunId 必须合法 UUID（`is_safe_run_id` :439）；1s 快速失败检测同步报错并清理 registry/bg_registry（tool.rs:280-330）；正常路径立即返回多行 run_id 文本（tool.rs:418-427） |
| 改并发限制/完成通知 | `src/registry.rs` | `WorkflowTaskRegistry::register`（:69 限流）、`complete`（:84 仅广播）、`kill`（:95）、`active_count`（:61） | 并发按 Running 状态计数；complete 不移除 run（history 保留供面板/调试）；kill 只发 kill_tx、不 abort child_handle（清理归 runner kill 分支，registry.rs:104-107）；广播唯一消费在 peri-agent executor 的 session 级 consumer（防重复通知） |
| 改进度状态（TUI 面板数据源） | `src/progress.rs` | `apply_event`（:57 reducer）；`get_run_stats`（:314）；`get_phase_summaries`（:330）；`get_all_runs_snapshot`（:206） | agent 按 agent_id 精确匹配、非 LIFO（progress.rs:3-4）；`AgentDone` 仅当 started_agents 含该 agent 才计 token/duration（resume cache-hit 只有 AgentDone，progress.rs:151-170）；RunDone 归一 Completed/Killed/Failed（:184-192）；完成态保留 5 分钟（`COMPLETED_RETENTION` :219）；`agents_as_map` serde（:290）使 IndexMap 序列化为 JSON 数组 |
| 改持久化/resume | `src/journal.rs` | `init_run`（:64）、`append`（:71）、`read_all`（:86）、`write_state`（:105 原子写）、`cleanup_old_runs`（:115）、`extract_long_texts`（:185） | `run_dir` 防路径遍历（:50-61）；state.json 先写 .tmp 再 rename（:105-111）；resume 按 `JournalEntry.key`（SHA256）cache-hit（read_all → WorkflowStartParams.resume）；`KEEP_MAX_RUNS=50` 按 mtime 清理最旧目录（:115-140）；超长文本（>200 字符）落盘 outputs/{label}.txt 并以 `${label}` 占位（:185） |
| 改 RPC 通道 | `src/rpc.rs` | `RpcChannel::send_request`（:137）、`handle_incoming`（:240）、`drain_pending`（:220）、`register_agent`（:275）、`deregister_agent`（:300）、`kill_agent`（:320）、`spawn_stdout_reader`（:346） | GAP-11：先 insert pending 再 write_line（:150-155）；stdout 关闭 → drain_pending 防 send_request 挂起（:346-373）；JSON 解析失败跳过、不阻塞后续行；kill_agent 双终止：error -32000 响应 + cancel_tx（:320-337）；deregister 先读后删，防 DashMap 同 shard 读锁/写锁死锁（:300-317） |

## 子系统

src/ 为单目录扁平结构，按文件组织。

### runner.rs（子进程生命周期 + 消息循环）

| 功能 | 入口/关键点 |
| --- | --- |
| 启动编排（run） | `run` :223：1 持久化 script → 2 resume 读旧 journal → 3 spawn（`ensure_workflow_install` :82 + `workflow_cmd` :33，本地固定安装优先、npx 兜底带显式版本）→ 4 RpcChannel + active_channels 注册（GAP-07）→ 5 stdout reader → 6 stderr buffer → 7 `workflow/start` 15s 超时（:347）→ 8 消息循环 :399 |
| 消息循环分发 | `agent/run` :426 / `progress/event` :532 / `journal/append` :550 / `journal/truncate` :562 / `log` :573（level 映射 warn/info/debug）/ `workflow/done` :585（break） |
| 单 agent kill | `kill_agent` :213（经 active_channels 找 RpcChannel） |
| kill 分支与清理 | `tokio::select!` :707：kill_rx → 5s 超时 RPC + child.kill + msg_loop.abort + killed state.json + progress RunDone(killed) + done_tx；两路退出后统一 `child.kill` + `active_channels.remove` + `cleanup_old_runs` + `cleanup_completed` |

### rpc.rs（双向 JSON-RPC）

| 功能 | 入口/关键点 |
| --- | --- |
| 行解析 | `parse_message` :40（纯函数；Response/Request 判定）；`handle_incoming` :240（匹配 pending 则消费，否则转发） |
| 请求-响应 | `send_request` :137（先 insert pending 再 write_line，GAP-11）；`send_response` :181；`send_error` :191；`send_notification` :165 |
| 挂起防护 | `drain_pending` :220（stdout 关闭时排空，`-32000 node process exited`）；`spawn_stdout_reader` :346 |
| 单 agent 追踪 | `register_agent` :275（重复 active agentId 拒绝）；`deregister_agent` :300（token 所有权校验）；`kill_agent` :320（error 响应 + cancel_tx 双终止） |

### tool.rs（LLM 可见入口）

| 功能 | 入口/关键点 |
| --- | --- |
| 工具定义 | `impl BaseTool` :56（name "Workflow"；parameters 无 required——script/scriptPath 二选一，JSON Schema 无法表达 OR） |
| invoke 流程 | `invoke` :111：解析参数 → run_id（UUID v7）→ watch/oneshot channels → spawn runner.run → registry.register（限流）→ bg_registry.register（携带 kill 闭包）→ 快速失败检测（1s）→ notification_task → 立即返回多行文本 |
| 安全校验 | `resolve_script_path` :419（cwd 限定，canonicalize 后 starts_with 检查）；`is_safe_run_id` :439（UUID + 无路径遍历字符） |
| 辅助 | `extract_workflow_name` :398（启发式匹配 `name:` 后引号字符串，兜底 "unnamed"） |

### registry.rs / progress.rs / journal.rs（状态三件套）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 运行注册表（广播 + 限流） | registry.rs | `WorkflowTaskRegistry` :33；`register` :69；`complete` :84；`kill` :95；`list_runs` :112；`WorkflowRun` :22（kill_tx 为 Option，完成/被杀后 None） |
| 内存进度 reducer | progress.rs | `WorkflowProgressStore` :42；`apply_event` :57；`get_run_stats` :314；`get_phase_summaries` :330；`get_agent_phase` :381；`RunProgress` :17（`agents: IndexMap<u64, AgentProgress>`，serde 数组化 :290） |
| 磁盘持久化 | journal.rs | `WorkflowJournalStore` :37；`init_run` :64；`append` :71；`read_all` :86（宽容解析，跳错行）；`write_state` :105；`cleanup_old_runs` :115；`extract_long_texts` :185；`RunState` :22 |

### protocol.rs / error.rs（协议与错误）

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 协议类型 | protocol.rs | `WorkflowStartParams` :17；`WorkflowKillParams` :30；`JournalEntry` :45；`WorkflowDoneParams` :55；`JsonRpcRequest/Error/Response` :67/76/84；错误码 `ERR_ABORTED=-32000` 等 :94 |
| 契约 re-export | protocol.rs:40 + registry.rs:10 | `AgentRunParams`/`AgentRunResult`/`ProgressEvent`/`Usage`/`WorkflowRunStatus`/`WorkflowTaskResult` 事实源在 `peri-acp-types/src/workflow.rs` |
| 错误模型 | error.rs | `WorkflowError` :4（SpawnFailed/Rpc/ScriptParse/ConcurrentLimit/NotFound/Io/Json） |

## 跨模块契约（指向 architecture-contracts.md，不复制正文）

- ARC-TOOLS-001：`WorkflowTool` 是 deferred tool（默认 `is_direct()=false`），只能经 `SearchExtraTools` 发现、`ExecuteExtraTool` 执行；包装层须透传
- ARC-EVENT-001：完成通知双路径——Path A 经 `BgRegistryEvent::Completed → bg-task-completed` unstable event（不再经 EventSink 直推），Path B 经 `AsyncRouter → push_defer` 注入消息流；消费唯一在 peri-agent executor 的 session 级 consumer
- ARC-FROZEN-001：workflow agent 透传 session frozen data（frozen CLAUDE.md / skills / system prompt 非精简版）；执行体 `WorkflowAgentExecutor`（`peri-agent/src/agent/workflow/agent.rs:141`）经 `AgentExecutor` trait 回调，装配由 `WorkflowMiddlewarePort`（`peri-acp-types/src/ports.rs:96`）注入
