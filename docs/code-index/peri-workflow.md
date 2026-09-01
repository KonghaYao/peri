# peri-workflow 代码索引

> 速查表：把「我想做什么」映射到稳定符号；细节以代码为准。更新：2026-09-01
> 依据：`docs/design/workflow-system.md`、`docs/standards/architecture-contracts.md`、源码（无 crate 级 CLAUDE.md）

## 架构速览

- 定位：多 Agent 编排子系统。用户 JS ESM 脚本在独立 Node.js 进程运行，经 stdio NDJSON 与 Rust host 双向 JSON-RPC；agent 回调复用 v2 `run_react_loop`。
- 主链：`WorkflowTool::invoke → preflight/GitBaseline → registry.reserve → WorkflowRunner::run → peri-js-runtime → Node engine → agent/run → AgentExecutor → Git postcondition/state.json → done_tx → registry.complete → session consumer → TUI/Defer`。
- 入口：`peri-workflow/src/tool.rs::WorkflowTool::invoke`；执行与终态：`peri-workflow/src/runner.rs::WorkflowRunner::run`；通用进程/RPC host：`peri-js-runtime/src/{host,rpc}.rs`。
- 契约事实源：`peri-acp-types/src/workflow.rs` 的 `AgentExecutor`、`AgentRunParams`、`AgentRunResult`、`ProgressEvent`、四维状态、`WorkflowAttempt`、`WorkflowTaskResult`。wire 变更须同步 `npm-packages/@peri-workflow/src/types.ts`。
- 并发不变量：start/resume 都先 `WorkflowTaskRegistry::reserve`，成功后才 spawn，并用 `attach_child` 绑定 task，拒绝路径不得产生 detached runner。
- 交付不变量：engine `completed` 只表示 execution completed；acceptance、post-processing、delivery 独立投影。Git postcondition 异常只报告并 blocked，不执行 add/commit/stash/reset/restore/clean。

## 速查表

| 我想做什么 | 主文件 | 稳定入口/关键逻辑 |
| --- | --- | --- |
| 改通用 JS RPC 传输/进程生命周期 | `peri-js-runtime/src/{rpc,host}.rs` | `RpcChannel::send_request`、`JsExecutionHost::spawn/kill/wait`；pending 先登记后写，stdout/exit/cancel drain pending，stderr 并行消费 |
| 改 Workflow agent 挂起/kill | `peri-workflow/src/rpc.rs` | `register_agent`、`deregister_agent`、`kill_agent`；ownership token 防 stale deregister，kill 同时响应 RPC error 与 cancel |
| 改脚本执行/Node 消息循环 | `peri-workflow/src/{runner,protocol}.rs` | `WorkflowRunner::run`、`workflow_start_params`、`parse_agent_run_params`；所有 run-scoped RPC 必须匹配 active run_id |
| 改 Workflow 工具/preflight | `peri-workflow/src/tool.rs` | `WorkflowTool::invoke`、`preflight_validate_script`、`resolve_script_path`；run_id 前校验脚本、cwd/repo、writeIntent、JS-safe limits，并捕获 GitBaseline |
| 改 Git ownership/postcondition | `peri-workflow/src/journal.rs` | `GitBaseline::capture`、`validate_write_intent`、`verify_postcondition`；`GIT_OPTIONAL_LOCKS=0`，canonical repo/cwd、allowlist、HEAD/commit paths fail-safe 对账 |
| 改 state/journal/resume | `peri-workflow/src/journal.rs` + `npm-packages/@peri-workflow/src/server.ts` | `WorkflowJournalStore::{init_run,append,read_all,write_state}`；state 原子写；legacy attempt identity 不得用 journal seq 伪造 |
| 改 runtime limits | `peri-workflow/src/runner.rs` | `WorkflowLimits`、agent/run 分支、elapsed/tool count 门限；live agent 才计 attempt，cache-hit 不重复计数 |
| 改并发限制/完成通知 | `peri-workflow/src/registry.rs`、`peri-middlewares/src/workflow/mod.rs` | `reserve`、`attach_child`、`complete`、`kill`、`resume_workflow`；complete 保留历史并广播，kill 清理由 runner 收敛 |
| 改进度/ACP snapshot | `peri-workflow/src/progress.rs`、`peri-middlewares/src/workflow/mod.rs` | `WorkflowProgressStore::apply_event/set_terminal_projection/get_all_runs_snapshot`、`WorkflowMiddlewarePort::runs_snapshot`；四维状态随 snapshot 序列化 |
| 改 TUI Workflow 面板 | `peri-tui/src/kit/{workflow_snapshot.rs,panels/workflow.rs}` | `TuiRunProgress` legacy 四维默认 unknown；panel footer 分别展示 execution/acceptance/post-processing/delivery |
| 改 Node adapter/attempt bridge | `npm-packages/@peri-workflow/src/{adapter,server,types}.ts` | `rpcAdapter.run` 透传真实 `ctx.agentId` 到 agent/run；journal callback 无 identity 时省略 optional agentId，不合成确定值 |

## 状态与持久化

- `RunProgress.status` 是 legacy/导航状态；`execution_status`、`acceptance_status`、`post_processing_status`、`delivery_status` 是 canonical 终态投影。
- `RunState` 是磁盘权威状态；写入失败必须使对外结果降级为 failed/blocked，不能继续通知 completed。
- `WorkflowTaskResult` 进入 broadcast 后由 session 级 consumer 唯一消费，分别驱动 bg-task completion 与 Defer。
- journal 保留 legacy `key/seq/result`；`attempt.agentId` 只有在来源真实可证明时存在，`journalSeq` 只表示日志顺序。

## 目标验证

```bash
cargo test -p peri-workflow --lib
cargo test -p peri-middlewares --lib workflow
cargo test -p peri-tui --lib workflow_snapshot
cargo clippy -p peri-workflow -p peri-acp-types -p peri-middlewares -p peri-tui --all-targets -- -D warnings
cd npm-packages/@peri-workflow && bun test && bun run typecheck && bun run build
```

## 跨模块契约

- `ARC-TOOLS-001`：`WorkflowTool` 是 deferred tool，包装层不得改变可见性。
- `ARC-EVENT-001`：完成通知经 bg task event 与 Defer 双路径投递，session consumer 唯一消费。
- `ARC-FROZEN-001`：workflow agent 继承 session frozen data；执行体在 `peri-agent/src/agent/workflow/agent.rs`，装配经 `WorkflowMiddlewarePort` 注入。
- `ARC-WORKFLOW-RPC-001`：通用 transport 属于 `peri-js-runtime`，Workflow Adapter 只处理 domain method 与 agent ownership。
