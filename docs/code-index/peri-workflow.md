# peri-workflow 代码索引

> 速查表：把「我想做什么」映射到稳定符号；细节以代码为准。更新：2026-09-13（ADLC 检查与恢复）
> 依据：`docs/design/workflow.md`、`docs/standards/architecture-contracts.md`、源码（无 crate 级 CLAUDE.md）

## 架构速览

- 定位：多 Agent 编排子系统。用户 JS AsyncFunction body（剥离唯一 `export const meta` 后执行）在独立 Node.js 进程运行，经 stdio NDJSON 与 Rust host 双向 JSON-RPC；agent 回调复用 v2 `run_react_loop`。
- 主链：`WorkflowTool::invoke → preflight/GitBaseline → registry.reserve → RunCompletion::spawn → WorkflowRunner::run → peri-js-runtime → Node engine → agent/run → AgentExecutor → Git postcondition/state.json → done_tx → RunCompletion::project → registry.complete → session consumer → TUI/Defer`。
- 入口：`peri-workflow/src/tool.rs::WorkflowTool::invoke`；执行与终态：`peri-workflow/src/runner.rs::WorkflowRunner::run`；通用进程 host 与 NDJSON framing/pending：`peri-js-runtime/src/{host,rpc}.rs`；Workflow agent ownership/kill：`peri-workflow/src/rpc.rs`。
- 契约事实源：`peri-acp-types/src/workflow.rs` 的 `AgentExecutor`、`AgentRunParams`、`AgentRunResult`、`ProgressEvent`、四维状态、`WorkflowAttempt`、`WorkflowTaskResult`。wire 变更须同步 `npm-packages/@peri-workflow/src/types.ts`。
- 并发不变量：start/resume 共用 `WorkflowTool::start_run`，先取得 session execution owner、`WorkflowTaskRegistry::reserve` 并登记取消通道，再经 `TaskManager::spawn_owned` 启动，拒绝路径不得产生 detached runner。
- 交付不变量：engine `completed` 只表示 execution completed；acceptance、post-processing、delivery 独立投影。`acceptance_status: unknown` 且 execution/post-processing 成功时，delivery 保持 `unknown`；明确执行/验收/Git postcondition 失败才为 `blocked`。Git postcondition 只比较 Workflow 前后状态发生变化的路径，已有且未变化的无关 dirty path 不阻塞；异常只报告并 blocked，不执行 add/commit/stash/reset/restore/clean。

## 速查表

| 我想做什么 | 主文件 | 稳定入口/关键逻辑 |
| --- | --- | --- |
| 改 session owner、run/resume 与关闭后准入 | `peri-workflow/src/tool.rs` + `tool/completion.rs` + `peri-middlewares/src/workflow/mod.rs` | `WorkflowTool::start_run`、`ExecutionOwner`、`RunCompletion::spawn`、`resume_workflow`；实际外部执行结算与 Defer/UI 完成分别维护；真实 Node 回归在 `peri-middlewares/src/workflow/lifecycle_test.rs` |
| 改 run 内 Agent 的取消排空 | `peri-workflow/src/runner/scope.rs` + `runner/agent_dispatch.rs` | `RunScope::spawn/drain`；取消必须等待所有子 future 结束，runner 还须等待 JS host/reader 退出，无法证明排空返回 `CleanupFailed` |
| 改通用 JS RPC 传输/进程生命周期 | `peri-js-runtime/src/{rpc,host}.rs` | `peri_js_runtime::RpcChannel::send_request`、`JsExecutionHost::spawn/kill/wait`；pending 先登记后写，stdout/exit/cancel drain pending，stderr 并行消费 |
| 改 Agent 执行观察与结果投影 | `peri-agent/src/agent/workflow/agent.rs` + `agent/{observation,result}.rs` | `WorkflowAgentExecutor::execute` 保留装配与 loop → close bus → join forwarder → stats/result → terminal；`WorkflowObservation` 单 owner 维护统计并先发 progress 后发 Langfuse，`project_run_result` 维持 schema/字符串 wire/终态语义 |
| 改结构化 Agent 结果校验 | `peri-agent/src/agent/workflow/agent/result.rs` + `result_test.rs` | `completed_result → validate_json_schema`；有限子集递归校验 type/required/properties/items，RawValue 保留数字原文精确判断 integer，number 仍接受整数；不宣称完整 JSON Schema |
| 改 Workflow agent 挂起/kill | `peri-workflow/src/rpc.rs` | `register_agent`、`deregister_agent`、`kill_agent`；ownership token 防 stale deregister，kill 同时响应 RPC error 与 cancel |
| 改启动、host 所有权与取消收敛 | `peri-workflow/src/runner.rs` | `WorkflowRunner::run`；拥有 message task 的 spawn/abort/join，启动失败先移除 active channel，kill 分支回收进程并等待 message task 后发布 killed |
| 改 runtime artifact/安装/命令准备 | `peri-workflow/src/runner/artifact.rs` | `prepare_workflow_command`、`validate_workflow_artifact`；固定 bundle 身份/字节校验，staging 原子发布与显式网络 fallback；安装路径优先非空 HOME，再取平台 home；内嵌 runner 需要 node，npm/npx 仅用于显式网络 fallback；`runner::WORKFLOW_ARTIFACT_BYTES` 仅为 preflight 兼容 re-export |
| 改内嵌 Workflow CLI 入口 | `peri-workflow/src/cli.rs` + `peri-acp/src/lib.rs` + `peri-tui/src/cli_workflow.rs` | `cli::run`、`argv_requests_workflow`；`peri workflow` 在配置/会话初始化前使用当前内嵌 artifact 运行 CLI，临时文件保留到 Node 退出，不查找网络版本 |
| 改 ADLC 文件边界证据 | `npm-packages/@peri-workflow/src/boundary.ts` | `snapshotBoundary`、`compareBoundary`；有界扫描、独立基线摘要、字面路径 allowlist、ignored 变化及生成目录身份；不是权限沙箱或写入归属证明 |
| 改 ADLC 阶段检查与恢复建议 | `npm-packages/@peri-workflow/src/adlc.ts` | `checkAdlcStage`、`planAdlcRecovery`；校验必需产物和 Main 提供的身份/依赖事实，不修改通用引擎终态或代替语义验收；策略由内置 `ultra-adlc/SKILL.md` 维护 |
| 改 run-scoped RPC 校验/启动握手 | `peri-workflow/src/runner/run_protocol.rs` + `peri-workflow/src/protocol.rs` | `parse_run_scoped`、`parse_agent_run_params`、`workflow_start_params`、`validate_start_ack`；请求匹配 active run_id，wire DTO 仍由 `protocol.rs` 定义或 re-export |
| 改 Node 消息分派 | `peri-workflow/src/runner/message_loop.rs` | `MessageLoop::run`；分派 domain method，参数错误继续接收，限额/协议错误终止循环，终态持久化与进度投影完成后发送 done |
| 改 Workflow agent task/响应门控 | `peri-workflow/src/runner/agent_dispatch.rs` | `AgentDispatcher::dispatch`；先注册再 spawn，task 持有 permit，token 决定响应所有权，duplicate/kill 不产生重复响应 |
| 改自然终态/Git 交付投影 | `peri-workflow/src/runner/terminal.rs` | `finalize_workflow`、`project_postcondition`、`send_failure`；Git postcondition → state.json → progress，持久化失败使对外结果降级为 failed/blocked |
| 改 Workflow 工具/preflight | `peri-workflow/src/tool.rs` + `tool/preflight.rs` | `WorkflowTool::invoke`、`preflight_validate_script`、`resolve_script_path`；`script` description 是 AsyncFunction body grammar（唯一 `export const meta`、顶层 primitives、顶层 `return` 建议、禁止 import/其他 export）的模型契约；run_id 前校验脚本、cwd/repo、writeIntent、JS-safe limits，并捕获 GitBaseline；preflight 持有 TempDir 与 kill-on-drop 子进程 |
| 改调用取消与运行完成发布 | `peri-workflow/src/tool/completion.rs` | `RunCompletion::spawn/project`；单任务等待真实 runner 清理后完成 registry 与四维投影；快速窗口只观察相同结果，调用者取消不丢完成通知，kill 保持 Killed |
| 改 Git ownership/postcondition | `peri-workflow/src/journal/git.rs`（journal 根 re-export） | `GitBaseline::capture`、`validate_write_intent`、`verify_postcondition`；`GIT_OPTIONAL_LOCKS=0`，canonical repo/cwd、allowlist、HEAD/commit paths fail-safe 对账 |
| 改 state/journal/resume | `peri-workflow/src/journal.rs` + `runner/run_protocol.rs` + `npm-packages/@peri-workflow/src/server.ts` | `WorkflowJournalStore::{init_run,append,read_all_strict,write_state}`；恢复读取失败可见，只复用从 seq=0 开始的连续 ok 前缀；state 保存 args/max_concurrency；legacy attempt identity 不得用 journal seq 伪造 |
| 改长输出提取 | `peri-workflow/src/journal/output.rs`（journal 根 re-export） | `extract_long_texts`；独立文件写入成功后才提交 JSON 引用，失败保留正文，返回值只列成功标签 |
| 改 runtime limits | `peri-workflow/src/runner/{limits,agent_dispatch,message_loop}.rs` | `try_reserve_live_attempt` 与 `LiveAttemptPermit::drop` 管 live 配额；`AgentDispatcher::dispatch` 检查 agent/tool 门限，`MessageLoop::run` 检查等待 deadline；配置事实源为 `protocol.rs::WorkflowLimits`，cache-hit 不重复计数 |
| 改并发限制/完成通知 | `peri-workflow/src/registry.rs`、`peri-middlewares/src/workflow/mod.rs` | `reserve`、`attach_child`、`complete`、`kill`、`resume_workflow`；complete 保留历史并广播，kill 清理由 runner 收敛 |
| 改进度/ACP snapshot | `peri-workflow/src/progress.rs`、`peri-middlewares/src/workflow/mod.rs` | `WorkflowProgressStore::apply_event/set_terminal_projection/get_all_runs_snapshot`、`WorkflowMiddlewarePort::runs_snapshot`；私有 TrackedRun 同时持有公开投影与本次实际执行标记，单一注册表锁管理；完成保留期内仍可取正确 phase 摘要，到期共同回收，cache-hit 历史 token/duration 不计入本次摘要 |
| 改 TUI Workflow 面板 | `peri-tui/src/kit/{workflow_snapshot.rs,panels/workflow.rs}` | `TuiRunProgress` legacy 四维默认 unknown；panel footer 只展示快捷键，不渲染 execution/acceptance/post-processing/delivery |
| 改 Node adapter/attempt bridge | `npm-packages/@peri-workflow/src/{adapter,server,types}.ts` | `rpcAdapter.run` 透传真实 `ctx.agentId` 到 agent/run；journal callback 无 identity 时省略 optional agentId，不合成确定值 |

## 状态与持久化

- `RunProgress.status` 是 legacy/导航状态；`execution_status`、`acceptance_status`、`post_processing_status`、`delivery_status` 是 canonical 终态投影。
- `RunState` 是磁盘权威状态；写入失败必须使对外结果降级为 failed/blocked，不能继续通知 completed。
- 新 state 保留启动 args 与并发数，ACP resume 原样恢复；旧 state 缺失时仅使用兼容默认，不能重建丢失的参数。契约变化时由 Workflow tool 显式传入完整新 args。
- `WorkflowTaskResult` 进入 broadcast 后由 session 级 consumer 唯一消费，分别驱动 bg-task completion 与 Defer。
- journal 保留 legacy `key/seq/result`；`attempt.agentId` 只有在来源真实可证明时存在，`journalSeq` 只表示日志顺序。

## 目标验证

```bash
cargo test -p peri-workflow --lib
cargo test -p peri-middlewares --lib workflow
cargo test -p peri-middlewares --lib ultra_adlc
cargo test -p peri-tui --bin peri cli_workflow
cargo test -p peri-workflow --doc
cargo test -p peri-tui --lib workflow_snapshot
cargo clippy -p peri-workflow -p peri-acp-types -p peri-middlewares -p peri-tui --all-targets -- -D warnings
cd npm-packages/@peri-workflow && bun test && bun run typecheck && bun run build
```

## 跨模块契约

- `ARC-TOOLS-001`：`WorkflowTool` 是 deferred tool，包装层不得改变可见性。
- `ARC-EVENT-001`：完成通知经 bg task event 与 Defer 双路径投递，session consumer 唯一消费。
- `ARC-FROZEN-001`：workflow agent 继承 session frozen data；执行体在 `peri-agent/src/agent/workflow/agent.rs`，装配经 `WorkflowMiddlewarePort` 注入。
- `ARC-WORKFLOW-RPC-001`：通用 transport 属于 `peri-js-runtime`，Workflow Adapter 只处理 domain method 与 agent ownership。
