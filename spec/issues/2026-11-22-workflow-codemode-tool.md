# Workflow 脚本确定性工具调用（`tool()`）阶段 1+2

**状态**：已实现并完成本地验证；未提交。
**优先级**：P1（issue #407 提案；用户 2026-11-22 采访定稿）。
**类型**：能力新增 / Workflow 脚本原语 / MCP 调用通道。
**创建日期**：2026-11-22。
**来源**：用户 `/interview https://github.com/KonghaYao/peri/issues/407`，7 轮采访定稿。
**基线**：分支 `feat/peri-swarm-production` @ `26d48ec98`；工作树仅 `.mcp.json` 有既存未提交改动（本任务不触碰）。
**交付范围**：阶段 1+2 最小闭环——`tool()` 原语 + 工具调用通道 + 结果投影 + resume/可观测 + 文档与验收。

## 用户决议

| 维度 | 决议 |
| --- | --- |
| 交付范围 | 阶段 1+2 最小闭环 |
| 生命周期 | 始终后台 Workflow run（无同步内联模式），复用 run_id/journal/取消/TUI 面板 |
| 工具面 | 会话模型可见目录中的全部 MCP 工具（workspace + 外部 MCP + deferred）；排除 host-only meta 工具（Agent / AskUserQuestion / TodoWrite / SkillTool / ToolSearch / ToolInvoke）与 Workflow 自身实例 |
| 命名 | 只接受 effective 名；deferred 工具由模型先 ToolSearch 发现，宿主解析与模型目录同源 |
| 参数校验 | 归 MCP 服务端；宿主仅做对象形状检查 + 复用 `normalize_params` 做 alias 归一 |
| 执行路径 | MCP 执行后回 Peri 层，经 canonical 调用管线执行 |
| 返回形态 | `structuredContent` 优先、文本兜底 |
| 错误语义 | 抛类型化 Error（code/tool/message/details），脚本可 try/catch |
| 限额 | 脚本工具调用不计入 `maxToolCalls` |
| 并发 | 工具不设并发上限（不受 `maxConcurrency` 约束） |
| 审批 | 遵守 Peri 现有审批面；后台 run 等待审批到超时，超时 fail closed |
| writeIntent | 保持 Git 后置条件语义，不用它限制工具 |
| resume | 记录并重放工具结果；新增独立 `tools.jsonl`，不改现有 agent journal 的 key/seq 与前缀匹配 |
| 崩溃恢复 | 执行中副作用崩溃：抛错给脚本；run 终态标记「存在未知副作用」 |
| 结果大小 | 工具结果不截断；`return` 值扩展现有 `outputs/` 外置（纳入顶层数组与裸字符串） |
| 可观测性 | 复用现有 `log` 进度事件（不改 8 种 `ProgressEvent` 契约） |
| 隔离边界 | 文档明确声明不是安全边界（脚本 `process`/`require` 等原生能力不受约束） |
| 引擎改动 | 不升级引擎；在 `@peri-workflow` 侧注入 `globalThis.tool` |
| 使用引导 | 新增 `codemode` skill，并与 ultracode 配合说明边界 |
| 承载通道 | 复用 workflow 任务表，新增 tool 种类（claim/回填/取消同路） |
| 未知副作用 | run 终态新增 `unknownSideEffects` 布尔字段（true 才出现） |
| resume 对齐 | 工具条目记自身连续 `toolSeq` + 调用前已完成 agent 调用数 `agentSeq` |
| 大结果口径 | 脚本路径不写 session outcome store；直接经 wire 返回并落 `tools.jsonl` |

## 核实证据

- `peri-acp/src/host/interaction_router.rs:748-769`：无请求作用域的 `TrustedInteractionOwner::Host` 按唯一在线 endpoint 投递；零端点 `NoOnlineEndpoint`、多端点 `OwnerUnmatched`，均 fail closed。注释明确覆盖「调度 / 工作流等 Host 入口」。
- `peri-middlewares/src/mcp/session_invocation/session_invocation_service.rs:554`：`run()` 只执行六步管线；`:694` `finish()` 才写 outcome store；`:801` 公开 `invoke()` = `run` + `finish`。
- `peri-middlewares/src/mcp/session_invocation/session_invocation_service.rs:282-290`：`session_visible_bridges()` 是会话可见 MCP bridge 的单一准入视图（排除关闭实例）。
- `peri-agent/src/tools/invocation.rs:190`：`normalize_params(input, Some(&bridge))` 可复用。
- `peri-acp-types/src/mcp_apps.rs:465-510`：`SessionInvocationRequest` / `InvocationTarget::Host { server_id, tool_name, arguments }`；`request.cancel` 仅是准入前放弃信号，不取消已 dispatch 调用。
- `peri-workflow/src/runner/artifact.rs:213-245`：显式注入目录同样校验随包 `WORKFLOW_ENTRY_SHA256`；任何 `@peri-workflow` JS 改动必须同步重建产物与 pin。
- `peri-workflow/src/journal/output.rs:139-141`：顶层字符串不参与 `extract_long_texts`；`peri-workflow/src/runner/message_loop.rs:326-345` 只对 object 形态 `return` 调用外置。
- `peri-workflow/src/runner/run_limit_state.rs`：`observed_tool_calls` 由任务终态结果观察；tool 种类任务必须排除，否则脚本工具会污染 `maxToolCalls`。
- `mcp-packages/workspace/src/resources/builtin.rs`：builtin skill 以 `include_str!` 嵌入并按宿主注册表同序；新增 skill 需同步 `peri-middlewares/src/prompt_tool_name_lock_test.rs::MANAGED_PROMPT_FILES`。

## 契约

### 脚本面

```js
const result = await tool('Read', { file_path: '/abs/path' })
// structuredContent 存在 → 返回它；否则返回文本字符串
// 失败 → throw WorkflowToolError { code, tool, message, details? }
```

- `arguments` 必须是普通对象或省略；数组/null/原始值在 Node 侧以 `invalid_arguments` 拒绝。
- 宿主只做形状检查与 alias 归一，不重复校验 schema。
- 错误码：`tool_not_found`、`invalid_arguments`、`tool_unavailable`、`policy_denied`、`approval_unavailable`、`not_executed`、`tool_error`、`protocol_error`、`task_accepted`、`outcome_unknown`、`cancelled`。
- 脚本可见集 = `session_visible_bridges()`（含 deferred、排除关闭实例）；host-only 工具不在该视图。
- 脚本工具调用不产生 `ProgressEvent::Tool`；只发 `log` 进度，不进入 `maxToolCalls` / token 计数。

### Wire

- Node → Rust：通知 `tool/task { taskId, runId, toolSeq, agentSeq, toolName, arguments }`。
- Rust → Node：通知 `tool/result { taskId, runId, outcome }`，`outcome` 为 tagged ok/error。
- Node → Rust：通知 `tool/replayed { runId, entry }`；resume 回放命中时记录到新 run 的 `tools.jsonl`，宿主不再执行工具。
- Host → runtime：host-only MCP 工具 `WorkflowToolResult`，复用 `WORKFLOW_CONSUMER_META_KEY` 鉴权与单次终态提交。
- `workflow/start` 增加可选 `resumeTools: ToolJournalEntry[]`（fresh run 省略）。
- 工作流协议版本 `3`：Rust `WORKFLOW_PROTOCOL_VERSION`、`package.json.periProtocolVersion`、Node `WORKFLOW_PROTOCOL_VERSION` 三处一致；artifact pin 按新 bundle 重建。

### 任务表

- `WorkflowTaskRecord.params` 泛化为 tagged payload：`agent` / `tool`。
- tool 种类任务沿用 claim / 回填 / 取消 / 资源订阅；不进入 agent 并发与 token/工具计数。
- `maxConcurrency` 仍只约束引擎的 `agent()` 槽位；`tool()` 调用不注册为 agent 任务。

### 持久化

`<run_dir>/tools.jsonl` 追加式条目：

```json
{"toolSeq":0,"agentSeq":2,"taskId":"...","toolName":"Read","arguments":{},"outcome":{"kind":"ok","structuredContent":{},"content":""}}
```

- 写入顺序：宿主回填到达后先追加日志，再向 Node 投递 `tool/result`。
- resume 可回放前缀：`toolSeq` 从 0 连续、`agentSeq <= 已回放 agent 前缀长度`、`toolName` 与 `arguments` 相等；首个不匹配起全部转 live。
- 新增 run 终态字段 `unknownSideEffects`（camelCase，`state.json` 与完成投影；旧文件缺省 false）。置位条件：`OutcomeUnknown`、执行中被终止、`TaskAccepted`。
- `return` 外置 gate 扩展到顶层数组与裸字符串；顶层字符串使用固定 label，避免空 label。

## 实施分解

### 阶段 1：可用闭环

1. `peri-acp-types/src/workflow.rs`：`WorkflowToolInvoker` / `WorkflowToolCall` / `WorkflowToolOutcome` / 错误码；`WorkflowTaskResult.unknown_side_effects`。
2. `peri-workflow`：`protocol.rs` 新增 wire 参数；`runtime/tasks.rs` payload 泛化；`runtime/workflow_runtime.rs` `complete_tool_call` + 未知副作用判定；`runner/message_loop.rs` 处理 `tool/task`；`runner/workflow_runner.rs` 投递 `tool/result`。
3. `mcp-packages/workflow`：新增 host-only `WorkflowToolResult`，更新任务资源投影与文档。
4. `peri-middlewares`：`resolve_visible_effective`（同一 `session_visible_bridges`）+ `invoke_unrecorded`（同一条 `run()`，跳过 `finish()`）+ 实现 invoker port。
5. `peri-acp`：生产环境保留 `SessionInvocationService` 句柄；`workflow_consumer` 按任务种类分派；`workspace_workflow` 注入 invoker。
6. `npm-packages/@peri-workflow`：`tool.ts`、`server.ts`、`rpc.ts`、`types.ts`；`globalThis.tool` 安装与 `finally` 恢复；待决工具调用在 kill 时拒绝。
7. 阶段 1 测试 + artifact 构建/pin 更新。

### 阶段 2：可恢复与可观测（已实现）

1. `tools.jsonl` 写入/读取/前缀过滤；`workflow/start` 携带 `resumeTools`；Node 侧顺序回放。实现要点：
   - `ToolJournalEntry`（`protocol.rs`）、`append_tool` / `read_tools_strict`（`journal/workflow_journal_store.rs`）；
   - 回放前缀由 Rust 选：`toolSeq` 从 0 连续且 `agentSeq <= 已回放 agent 前缀长度`（`runner/run_protocol.rs`），Node 再按 `toolSeq`/`toolName`/`arguments` 深比较逐条确认，首个不匹配起全部转 live；
   - 回放命中发 `tool/replayed`，只记录不执行；`tools.jsonl` **不随 `journal/truncate` 清空**（append-only，前缀选择容忍断点）；
   - 并发工具按完成顺序追加，读取时按 `toolSeq` 排序；`init_run` 创建空 `journal.jsonl` / `tools.jsonl`，保证零 agent run 也可 resume。
2. `unknownSideEffects` 投影到 `state.json`、`WorkflowTaskResult` 与完成摘要（`to_notification` 的未知副作用行）。
3. `return` 顶层数组/裸字符串外置（顶层字符串固定 label `return`，数组元素 `[i]`）。
4. `codemode` skill 与 ultracode 边界说明；已注册 builtin 与 prompt 锁清单。
5. 设计文档、功能目录、验收映射与验收记录同变更更新。

## 验收

1. 脚本只调用 `tool()`（无 `agent()`）即可完成真实 MCP 工具编排并返回结构化结果。
2. `Read`/`Bash` 等 workspace 工具与至少一个 deferred 外部 MCP 工具可按 effective 名调用。
3. 审批：唯一在线 endpoint 可获批准；零/多端点或拒绝/超时 → 脚本收到可 catch 的类型化错误，工具未执行。
4. 结果 >1 MiB 不截断，且不写入 session outcome store。
5. resume 不重复执行已记录副作用（标记文件只写一次）；agent 失败边界之后的工具不被错误回放。
6. kill 中断执行中的工具后，`unknownSideEffects=true` 且不虚报完成。
7. `maxToolCalls` 不被脚本工具污染；并发工具调用不被 `maxConcurrency` 拒绝。
8. 文档明确「不是安全边界」，`codemode` skill 可按名加载并通过 builtin 能力测试。

## 边界与限制

- 不是安全边界：脚本仍可使用 Node 原生能力。
- 已 dispatch 的 MCP 调用不会被 run kill 强行中断；此时按未知副作用处理。
- `TaskAccepted` 在阶段 1+2 只抛 `task_accepted`，不提供轮询/等待原语。
- 非会话路径（`peri workflow` CLI）不提供 `tool()`；调用得到 `tool_unavailable`。
- 旧 `state.json` 无 `unknownSideEffects` 字段时按 false 读取；不得据此推断本能力已存在。

## 验证状态

- 基线：`feat/peri-swarm-production` @ `26d48ec98` + 本任务未提交改动；sidecar
  `dist/workflow-sidecar/peri-resources/workflow/b7fcaf3e1cbc/`，入口 sha256
  `b7fcaf3e1cbccd2629254a071b5d41a808464ba20548346851f538762e0bdd89` 与
  `WORKFLOW_ENTRY_SHA256` 一致。
- 已运行：`./scripts/cargo-rmcp-patched.sh check --locked`（peri-workflow /
  peri-acp-types / peri-middlewares / peri-acp / peri-mcp-workflow / peri-tui）通过；
  `peri-workflow --lib` 串行 285/285；真实引擎 E2E
  `host::workflow_consumer_tests` 11/11（含 tool E2E 5/5）；`peri-middlewares`
  prompt 锁 1/1；`peri-mcp-workspace` builtin 14/14；`peri-tui` workflow 21/21；
  Node `bun test` 136/136、`bun run typecheck` 通过；`cargo fmt --check` 与
  `git diff --check` 无差异。
- 复跑判定：一次性全量 `peri-acp --lib` 并行出现 8 个失败（worktree / stdio /
  wire），串行分组全部通过，判定为并行负载下的 Git discovery 与 stdio 超时；
  `peri-workflow` 并行全量偶发 `workflow_cli_spawns_prepared_artifact_for_help`
  返回 1，`--exact` 与串行均通过，属既存测试隔离竞争。
- 未覆盖：真实 `interaction_router` 审批拒绝/超时、真实外部 deferred MCP 工具、
  TUI 渲染断言、peri-e2e #10 的固定测试仓 SHA 对账；`read` CLI 文本摘要未加该字段。
- 详细命令、证据与限制见
  [验收记录](../../docs/verification/workflow-codemode-tool-2026-11-22.md)。
