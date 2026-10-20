# peri-workflow 代码索引

Workflow 的运行耗时、RPC 有界等待和终态时间戳使用 [peri-time](peri-time.md)；
预算与持久化字段语义仍由 Workflow 自身定义。

> 速查表：把「我想做什么」映射到稳定符号；细节以代码为准。更新：2026-10-11（Workflow as MCP：builtin 实例 + `agent/task` 信道；artifact 改为非嵌入 sidecar + 摘要 pin）
> 依据：`docs/design/workflow-mcp.md`（现行目标设计）、`docs/design/workflow.md`（迁移前旧链描述）、`docs/standards/architecture-contracts.md`、源码（无 crate 级 CLAUDE.md）

## 架构速览

- 定位：多 Agent 编排子系统。用户 JS AsyncFunction body（剥离唯一 `export const meta` 后执行）在独立 Node.js 进程运行，经 stdio NDJSON 与 Rust host 双向 JSON-RPC；agent 回调复用 v2 `run_react_loop`。
- 主链：`WorkflowRuntime::start_run`（经 MCP `Workflow` 工具）→ preflight/GitBaseline → registry.reserve → RunCompletion::spawn → WorkflowRunner::run → peri-js-runtime → Node engine → `agent/task` → 实例任务表 → MCP 资源读取/`WorkflowAgentClaim` → `WorkflowTaskConsumer` 执行 → MCP `WorkflowAgentResult` → `agent/result` → Git postcondition/state.json → RunCompletion::project → 完成 outbox → MCP `workflow://background-tasks` → Host task projection → ACP/TUI 与 Defer。
- 入口：`peri-workflow/src/runtime.rs::WorkflowRuntime`（start_run/kill/resume/status/任务表；经 `mcp-packages/workflow/src/{server,resources}.rs` 暴露 MCP 工具/资源/订阅）；执行与终态：`peri-workflow/src/runner.rs::WorkflowRunner::run`；通用进程 host 与 NDJSON framing/pending：`peri-js-runtime/src/{host,rpc}.rs`；Workflow agent ownership/kill：`peri-workflow/src/rpc.rs`（P6 后引擎侧无登记方，kill 由任务表栅栏承担）。
- 契约事实源：`peri-acp-types/src/workflow.rs` 的 `AgentExecutor`、`AgentRunParams`、`AgentRunResult`、`ProgressEvent`、四维状态、`WorkflowAttempt`、`WorkflowTaskResult`。wire 变更须同步 `npm-packages/@peri-workflow/src/types.ts`。
- Workflow Agent 的持久执行边界在 `peri-agent/src/agent/workflow/agent/execution.rs`：从直接发起会话继承 binding、frozen 与资源授权，保存真实 child 后交由共享 WorkBoundary 获取 SDK 唯一准入；实际执行、forwarder 和持久化排空后，先结清存在的直接发起会话终态 ACK，再结束准入。生命周期缺失或结清未知不降级为内存执行。
- 并发不变量：start/resume 共用 `WorkflowTool::start_run`，先取得实例独立的 execution owner、`WorkflowTaskRegistry::reserve` 并登记取消通道，再经 `TaskManager::spawn_owned` 启动；MCP 启动成功返回前等待 Host 的 `WorkflowTaskObserved`，拒绝路径不得产生 detached runner。
- 交付不变量：engine `completed` 只表示 execution completed；acceptance、post-processing、delivery 独立投影。`acceptance_status: unknown` 且 execution/post-processing 成功时，delivery 保持 `unknown`；明确执行/验收/Git postcondition 失败才为 `blocked`。Git postcondition 只比较 Workflow 前后状态发生变化的路径，已有且未变化的无关 dirty path 不阻塞；异常只报告并 blocked，不执行 add/commit/stash/reset/restore/clean。内核不产出 `deliverable`（该判定属引擎之外的验收层；历史 state.json 里的该值按 `unknown` 读入）。

## 速查表

| 我想做什么 | 主文件 | 稳定入口/关键逻辑 |
| --- | --- | --- |
| 改 session owner、run/resume 与关闭后准入 | `peri-workflow/src/{runtime.rs,tool.rs,completion.rs}` + `peri-acp/src/host/workflow_agent.rs` | `WorkflowRuntime::{start_run,kill_run,resume}`、`ExecutionOwner`、`RunCompletion::spawn`、`create_session_workflow_chain`（:235）；关闭先封准入（`CloseGate`）；真实 wire 回归在 `mcp-packages/workflow/src/server_test.rs` 与 `peri-acp/src/host/workflow_consumer_test.rs` |
| 改 run 内 Agent 的取消排空 | `peri-workflow/src/runner/scope.rs` | `RunScope::spawn/drain`；取消必须等待所有子 future 结束，runner 还须等待 JS host/reader 退出，无法证明排空返回 `CleanupFailed` |
| 改通用 JS RPC 传输/进程生命周期 | `peri-js-runtime/src/{rpc,host}.rs` | `peri_js_runtime::RpcChannel::send_request`、`JsExecutionHost::spawn/kill/wait`；pending 先登记后写，stdout/exit/cancel drain pending，stderr 并行消费 |
| 改 Workflow 自有 Node、npm 与 Git 子进程 | `peri-workflow/src/process.rs` + `src/{cli,preflight,runner/artifact,journal/git}.rs` | 自有 spawn 统一经 `ProcessTree` 接入 SDK 进程监督并剥离 broker bearer；超时和取消必须等待组结算，不能仅靠 command child 退出 |
| 改 Agent 执行观察与结果投影 | `peri-agent/src/agent/workflow/agent.rs` + `agent/{observation,result}.rs` | `WorkflowAgentExecutor::execute` 保留装配与 loop → close bus → join forwarder → stats/result → terminal → 子会话作用域结清（`close_session_scope`，独立取消令牌不随父级联）；`WorkflowObservation` 单 owner 维护统计并先发 progress 后发 Langfuse，`project_run_result` 维持 schema/字符串 wire/终态语义 |
| 改结构化 Agent 结果校验 | `peri-agent/src/agent/workflow/agent/result.rs` + `result_test.rs` | `completed_result → validate_json_schema`；有限子集递归校验 type/required/properties/items，RawValue 保留数字原文精确判断 integer，number 仍接受整数；不宣称完整 JSON Schema |
| 改 Workflow agent 挂起/kill | `peri-workflow/src/rpc.rs` | `register_agent`、`deregister_agent`、`kill_agent`（ownership token 防 stale deregister）；P6 后引擎侧无登记方（`register_agent` 仅测试引用），单 agent kill 由任务表栅栏承担（`runtime.rs::kill_agent` → `tasks.cancel_agent` + 引擎 kill 兜底） |
| 改启动、host 所有权与取消收敛 | `peri-workflow/src/runner.rs` | `WorkflowRunner::run`；拥有 message task 的 spawn/abort/join，启动失败先移除 active channel，kill 分支回收进程并等待 message task 后发布 killed |
| 改 runtime artifact 发现/校验/命令准备 | `peri-workflow/src/runner/artifact.rs` | 零嵌入（ARC-WORKFLOW-DIST-001）：`ArtifactSources` / `resolve_artifact` 按「`PERI_WORKFLOW_ARTIFACT_DIR` → 缓存 `~/.peri/workflow/<version>/<内容身份>/` → 随包 sidecar `<exe 目录>/peri-resources/workflow/<内容身份>/`」解析；`validate_workflow_package` 校验包身份（name/version/main/协议/buildId）、入口不越界与入口 sha256 == `WORKFLOW_ENTRY_SHA256`（只存摘要，不存 JS）；`publish_to_cache_in` 以 staging + rename 只写内容身份目录；`prepare_workflow_command_with` 默认离线 fail closed，仅 `PERI_WORKFLOW_ALLOW_NPX_FALLBACK=1` 走一次「获取后仍需同一摘要校验」的 `fetch_and_publish_from_registry`；HOME 优先非空环境变量，再取平台 home |
| 改 Workflow CLI 入口 | `peri-workflow/src/cli.rs` + `peri-acp/src/lib.rs` + `peri-tui/src/cli_workflow.rs` | `cli::run`（生产解析）、`run_prepared`（测试注入已准备命令）、`validate_args` 白名单；`peri workflow` 在配置/会话初始化前经 `prepare_workflow_command` 使用已校验 artifact 运行 CLI；引擎相关测试经 `PERI_WORKFLOW_ARTIFACT_DIR` 注入构建产出的 bundle（`#[serial]`；dist 不入库，缺失即显式失败）或 `run_prepared` 直跑，缺 bundle 不再跳过；本 crate 的引擎用例只保留不依赖消费者的路径（kill / 自然崩溃），需要 agent 结果的完成与取消生命周期覆盖在宿主层 `peri-acp/src/host/workflow_consumer_test.rs`（P6：执行权在 `WorkflowTaskConsumer`） |
| 改 ADLC 文件边界证据 | `npm-packages/@peri-workflow/src/boundary.ts` | `snapshotBoundary`、`compareBoundary`；有界扫描、独立基线摘要、字面路径 allowlist、ignored 变化及生成目录身份；不是权限沙箱或写入归属证明 |
| 改 ADLC 阶段检查与恢复建议 | `npm-packages/@peri-workflow/src/adlc.ts` | `checkAdlcStage`、`planAdlcRecovery`；校验必需产物和 Main 提供的身份/依赖事实，不修改通用引擎终态或代替语义验收；策略由内置 `ultra-adlc/SKILL.md` 维护 |
| 改 run-scoped RPC 校验/启动握手 | `peri-workflow/src/runner/run_protocol.rs` + `peri-workflow/src/protocol.rs` | `parse_run_scoped`、`parse_agent_task_params`、`workflow_start_params`、`validate_start_ack`；请求匹配 active run_id，wire DTO 仍由 `protocol.rs` 定义或 re-export |
| 改 Node 消息分派 | `peri-workflow/src/runner/message_loop.rs` | `MessageLoop::run`；分派 domain method（P6：`agent/task` 通知 → `AgentTaskSink` 发布任务表，未接线/参数非法/关闭时就地回投 `agent/result`），参数错误继续接收，限额/协议错误终止循环，终态持久化与进度投影完成后发送 done |
| 改 Workflow 消费者装配与连接竞态 | `peri-acp/src/host/workspace_workflow.rs` + `host/requests/session_lifecycle.rs` | `start_workflow_consumer` 在 MCP 初始化中等待实例注册，初始化完成且无实例才跳过；失败/超时由 `attach_session_workflow` 传播到 new/load/resume/fork 的清理路径。回归：`host::workspace_seam::workflow_consumer`（延迟连接后的真实 MCP claim/回填、bare 缺席） |
| 改 Workflow agent task 门控 | `peri-workflow/src/runtime/tasks.rs` + `mcp-packages/workflow/src/consumer.rs` | `WorkflowTaskTable::{publish,claim_for_consumer,settle}`；claimId 重试幂等，状态与容量在同一锁内裁决；ready/claim/backfill 均经带消费者令牌的 MCP 工具 |
| 改任务计数与事件日志分页 | `peri-workflow/src/runtime/tasks.rs` + `runtime/event_log.rs` | `WorkflowTaskTable::{active_count_for_run, executing_count_for_run}`（持锁借用遍历 `order` + `tasks.get`，只计数不克隆记录；语义等价 `for_run(...).iter().filter(...)`；消费点 `runtime.rs` 的 `maxAgents` 门限与 `peri-acp/src/host/workflow_consumer.rs` 的 `maxConcurrency` 容量）；`BoundedEventLog::page_where` 用 `partition_point` + `VecDeque::range` 跳过游标前已读前缀（依赖 entries 内 sequence 严格递增），保留 `start = max(cursor+1, truncated_before)` 与 head_sequence/truncated_before 语义 |
| 改自然终态/Git 交付投影 | `peri-workflow/src/runner/terminal.rs` | `finalize_workflow`、`project_postcondition`、`send_failure`；Git postcondition → state.json → progress，持久化失败使对外结果降级为 failed/blocked |
| 改 Workflow 工具/preflight | `peri-workflow/src/tool.rs` + `src/preflight.rs` + `runtime.rs` | `WorkflowTool`（`new(runtime)` 薄壳与描述/参数契约）；执行与参数校验核心在 `WorkflowRuntime::start_run`；`preflight_validate_script`、`resolve_script_path`；`script` description 是 AsyncFunction body grammar（唯一 `export const meta`、顶层 primitives、顶层 `return` 建议、禁止 import/其他 export）的模型契约；run_id 前校验脚本、cwd/repo、writeIntent、JS-safe limits，并捕获 GitBaseline；preflight 持有 TempDir 与 kill-on-drop 子进程 |
| 改调用取消与运行完成发布 | `peri-workflow/src/completion.rs` | `RunCompletion::spawn/project`；单任务等待真实 runner 清理后完成 registry 与四维投影；快速窗口只观察相同结果，调用者取消不丢完成通知，kill 保持 Killed |
| 改 Git ownership/postcondition | `peri-workflow/src/journal/git.rs`（journal 根 re-export） | `GitBaseline::capture`、`validate_write_intent`、`verify_postcondition`；`GIT_OPTIONAL_LOCKS=0`，canonical repo/cwd、allowlist、HEAD/commit paths fail-safe 对账 |
| 改 state/journal/resume | `peri-workflow/src/journal.rs` + `runner/run_protocol.rs` + `npm-packages/@peri-workflow/src/server.ts` | `WorkflowJournalStore::{init_run,append,read_all_strict,write_state}`；恢复读取失败可见，只复用从 seq=0 开始的连续 ok 前缀；state 保存 args/max_concurrency；legacy attempt identity 不得用 journal seq 伪造 |
| 改长输出提取 | `peri-workflow/src/journal/output.rs`（journal 根 re-export） | `extract_long_texts`；独立文件写入成功后才提交 JSON 引用，失败保留正文，返回值只列成功标签 |
| 改 runtime limits | `peri-workflow/src/runner/{limits,message_loop}.rs` + `runtime.rs`（预算表） | tool 调用计数改由任务终态结果观察（`RunLimitState::observe_tool_calls`），breach 由 `workflow/done` 投影为 failed；`RunExecutionBudget`（maxConcurrency/maxAgents）是宿主消费者与 wire 准入的同一权威，预算缺失时容量门跳过（liveness 优先）；配置事实源为 `protocol.rs::WorkflowLimits`，cache-hit 不重复计数 |
| 改并发限制/完成通知 | `peri-workflow/src/registry.rs` + `peri-acp/src/host/workflow_projection.rs` | `complete` 保留未确认结果；Host 通过 MCP 重建独立任务投影并投递完成结果，成功后才调用 `WorkflowCompletionAck`；`WorkflowClose` 排空实例所有的执行 |
| 改进度/ACP snapshot | `peri-workflow/src/progress.rs` + `runtime.rs` | `WorkflowProgressStore::{apply_event,set_terminal_projection,get_all_runs_snapshot}`、`WorkflowRuntime::{runs_snapshot,run_snapshot,event_watermark}`；私有 TrackedRun 同时持有公开投影与本次实际执行标记，单一提交器（`commit` 串行锁）保证事件顺序，观察者回调不得重入提交；完成保留期内仍可取正确 phase 摘要，到期共同回收，cache-hit 历史 token/duration 不计入本次摘要；相位耗时是 `PhaseStarted`→`PhaseDone` 的墙钟，无相位边界事件为 `None`（不回落成 agent 时长求和） |
| 改 TUI Workflow 面板 | `peri-tui/src/kit/{workflow_snapshot.rs,panels/workflow.rs}` | `TuiRunProgress` legacy 四维默认 unknown；panel footer 只展示快捷键，不渲染 execution/acceptance/post-processing/delivery |
| 改 Node adapter/attempt bridge | `npm-packages/@peri-workflow/src/{adapter,rpc,server,types}.ts` | `rpcAdapter.run` 发布 `agent/task`（taskId = `<runId>:<seq>`）并透传真实 `ctx.agentId`；`agent/result` 按 taskId 幂等结算等待表（`registerTask`/`handleTaskResult`/`rejectPendingTasks`）；journal callback 无 identity 时省略 optional agentId，不合成确定值 |

## 状态与持久化

- `RunProgress.status` 是 legacy/导航状态；`execution_status`、`acceptance_status`、`post_processing_status`、`delivery_status` 是 canonical 终态投影。
- `RunState` 是磁盘权威状态；写入失败必须使对外结果降级为 failed/blocked，不能继续通知 completed。
- 新 state 保留启动 args 与并发数，ACP resume 原样恢复；旧 state 缺失时仅使用兼容默认，不能重建丢失的参数。契约变化时由 Workflow tool 显式传入完整新 args。
- run 终态经 `registry::complete` 保存在实例内 outbox，Host 只能通过 MCP 读取并确认；不共享 TaskManager 或完成广播。任务级终态经 `workflow://tasks` 资源与 `agent/result` 通知投递。outbox 只保证实例存活期间的重试，不承诺进程重启恢复。
- journal 保留 legacy `key/seq/result`；`attempt.agentId` 只有在来源真实可证明时存在，`journalSeq` 只表示日志顺序。

## 目标验证

```bash
cargo test -p peri-workflow --lib
cargo test -p peri-mcp-workflow --lib
cargo test -p peri-acp --lib workflow_consumer
cargo test -p peri-middlewares --lib ultra_adlc
cargo test -p peri-tui --bin peri cli_workflow
cargo test -p peri-workflow --doc
cargo test -p peri-tui --lib workflow_snapshot
cargo clippy -p peri-workflow -p peri-acp-types -p peri-middlewares -p peri-tui --all-targets -- -D warnings
cd npm-packages/@peri-workflow && bun test && bun run typecheck && bun run build
```

## 跨模块契约

- `ARC-TOOLS-001`：`Workflow`（实例工具，`mcp__workflow__Workflow`）是 deferred tool，包装层不得改变可见性；`WorkflowAgentResult` 是 host-only（`model_visible: false`）。
- `ARC-EVENT-001`：run 完成通知经 bg task event 与 Defer 双路径投递；任务/进度变化经实例资源面 `notifications/resources/updated` 唤醒订阅者（先可见后通知）。
- `ARC-FROZEN-001`：workflow agent 继承 session frozen data；执行体在 `peri-agent/src/agent/workflow/agent.rs`，装配经 `WorkflowAgentFactory` 注入（`peri-acp/src/host/workflow_agent.rs`）。
- `ARC-CAPABILITY-CLOSURE-001`：关闭键 `"WorkflowMiddleware"` 命中即不装配实例与消费者（唯一物理关闭特例）。
- `ARC-WORKFLOW-RPC-001`：通用 transport 属于 `peri-js-runtime`，Workflow Adapter 只处理 domain method 与 agent 任务发布/结果结算（`agent/task` / `agent/result`）。
