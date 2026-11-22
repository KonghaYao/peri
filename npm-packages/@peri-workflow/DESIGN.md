# @peri-code/workflow — Design & Integration Contract

## 定位

`@peri-code/workflow` 是一个 **JSON-RPC 2.0 stdio 桥梁**，连接宿主进程与 workflow engine。它不绑定任何宿主语言——任何能 spawn Node.js 子进程、且能读写 JSON-RPC 的语言都可以对接。

```
 ┌──────────────────┐              ┌─────────────────────────┐
 │  宿主 (Rust etc.) │              │  @peri-code/workflow (Node)   │
 │                  │  JSON-RPC    │                         │
 │  • 会话管理       │◄────────────►│  • RPC 传输层            │
 │  • LLM API 密钥   │  stdin/stdout │  • AgentAdapter         │
 │  • 工具执行       │              │  • WorkflowPorts 工厂    │
 │  • 文件系统权限    │              │  ┌─────────────────────┐│
 │  • 审批/安全      │              │  │ workflow-engine      ││
 │  • UI 渲染        │              │  │ (DAG 执行/沙箱/日志) ││
 │                  │              │  └─────────────────────┘│
 └──────────────────┘              └─────────────────────────┘
```

**核心原则**：宿主拥有 agent 执行权，本进程拥有 workflow 编排权。边界在 **`agent/task`（发布）+ `agent/result`（回填）通知对**（P6；旧链为 `agent/run` 请求/响应）——宿主决定如何执行 LLM agent（选择模型、管理 API 密钥、运行工具），本进程决定 agent 之间如何编排（并行、流水线、重试）。

---

## 协议规范

### 传输层

- **通道**：stdin（宿主→runner）+ stdout（runner→宿主）
- **帧格式**：newline-delimited JSON（每行一个完整的 JSON 对象）
- **编码**：UTF-8
- **协议**：JSON-RPC 2.0

### 消息类型

| 类型 | jsonrpc | id | method | params | result/error |
|------|---------|-----|--------|--------|-------------|
| Request | "2.0" | 有 | 有 | 有 | 无 |
| Response | "2.0" | 有 | 无 | 无 | 有（result 或 error） |
| Notification | "2.0" | 无 | 有 | 有 | 无 |

### 宿主 → Runner（入站请求）

#### `workflow/start`

启动一个新的 workflow run。

```typescript
// 请求
{ jsonrpc: "2.0", id: 1, method: "workflow/start", params: {
  runId:    string,        // UUID，用于关联 progress/journal
  cwd:      string,        // 工作目录
  budgetTotal: number|null, // token 预算上限，null 为无限制
  resume?:  JournalEntry[], // 续跑时的历史 journal
  resumeTools?: ToolJournalEntry[] | null, // 续跑时的脚本工具调用记录；省略/null = 不回放
  script:   string,        // workflow ESM 脚本源码
  args?:    unknown,       // 用户传入的参数（args 变量）
  maxConcurrency?: number, // 最大并发 agent 数
} }
// 响应
{ jsonrpc: "2.0", id: 1, result: { ok: true } }
```

**时序**：响应立即返回（验证 stage），workflow 自体异步执行。宿主必须在收到响应后随时准备好处理后续的 `agent/task` 通知与 `progress/event` 通知。

#### `workflow/kill`

中止当前运行的 workflow。

```typescript
// 请求
{ jsonrpc: "2.0", id: 2, method: "workflow/kill" }
// 响应
{ jsonrpc: "2.0", id: 2, result: { ok: true } }
```

**效果**：设置 `AbortController.abort()`。未完成任务的等待项以取消编码 `dead { reason: "killed" }` 结算（adapter 抛 `WorkflowAbortedError`）。workflow 停止推进并发送 `workflow/done` 通知（status: "killed"）。

### Runner → 宿主（出站通知）

#### `agent/task`

发布一个 agent 任务（**通知**，无 `id`、无响应）。宿主登记 `taskId` → 认领执行 → 以 `agent/result` 通知回投结果（按 `taskId` 幂等）。P6 任务信道；旧链 `agent/run` 请求/响应已删除。

```typescript
// 发布（通知；无 id）
{ jsonrpc: "2.0", method: "agent/task", params: {
  taskId:   string,        // 单次 run 内唯一（`<runId>:<seq>`）；回填按它去重
  runId:    string,
  agentId:  number,        // 引擎层序列号（不是核心 AgentId）
  prompt:   string,
  schema?:   object,       // JSON Schema，要求结构化输出
  model?:   string,        // 模型选择
  maxTokens?: number,
  agentType?: string,      // 子 agent 类型
  isolation?: "worktree",  // 是否隔离工作区
  allowedTools?: string[], // 工具白名单
  label?:    string,       // 展示名称
  phase?:    string,       // 所属阶段
} }
```

#### `agent/result`

宿主回投任务结果（**通知**，无 `id`）。

```typescript
// 回填（宿主 → runner，通知；无 id）
{ jsonrpc: "2.0", method: "agent/result", params: {
  runId:  string,
  taskId: string,   // 与发布一致；未知/重复 taskId 忽略（幂等结算等待表）
  result: AgentRunResult,
} }
```

`AgentRunResult` 判别联合：
- `{ kind: "ok", output: string|object, usage: { outputTokens: number }, model?, toolCount?, tokenCount? }`
- `{ kind: "skipped" }`
- `{ kind: "dead", reason?: "no-structured-output"|"runagent-threw"|"worktree-failed"|"killed"|"unknown", detail?: string }` —— `killed` 为取消编码，adapter 据此抛 `WorkflowAbortedError`。

**等待表与幂等**：adapter 发布前登记 `taskId → pending`（`registerTask`）；`agent/result` 按 `taskId` 结算（`handleTaskResult`），未知或重复投递直接忽略；run 结束/kill 收口时以 `dead { reason: "killed" }` 结算全部未完成项（`rejectPendingTasks`），不残留悬挂 promise。

**宿主职责**：
1. 加载正确的 LLM 模型（使用 `model` 参数）
2. 执行 ReAct 循环（prompt → LLM → tool calls → results → LLM → ...）
3. 管理 API 密钥（只待在宿主内存中，绝不影响 runner 环境变量）
4. 应用 `allowedTools` 过滤
5. 返回结果时包含 `usage.outputTokens`
6. 执行完成后以 `agent/result` 通知回投（`taskId` 幂等；取消/中止编码为 `dead { reason: "killed" }`）

**任务结算编码（P6）**：任务结果不再经请求/错误响应，统一在 `agent/result` 中编码：
- 取消/中止：`dead { reason: "killed" }` → adapter 抛 `WorkflowAbortedError`（旧链等价 `-32000` 错误响应）；
- 宿主内部失败：`dead { reason: "runagent-threw", detail }`（adapter 对空投递兜底同一 reason）；
- `-32603` 等 generic error 仅适用于仍有响应的宿主 → runner 请求（`workflow/start`、`workflow/kill`）。

### 脚本工具调用信道（issue #407）

脚本 `tool()` 的三条通知：`tool/task`（runner → 宿主，发布）、`tool/result`（宿主 → runner，回投）与 `tool/replayed`（runner → 宿主，续跑回放）。与 agent 任务共用回填幂等约定，但使用独立等待表，不进入 agent 任务表、并发槽与 token/工具计数。每次调用的持久记录（`tools.jsonl`）由 **Rust 宿主**写入，Node 侧只消费 `resumeTools` 前缀。

#### `tool/task`

发布一次脚本工具调用（**通知**，无 `id`）。宿主在会话可见视图内按 effective 名解析工具、走既有审批与 canonical 调用管线，并以 `tool/result` 通知回投终态。`taskId` 不进入 agent 任务表，也不约束于 `maxConcurrency`。

```typescript
// 发布（通知；无 id）
{ jsonrpc: "2.0", method: "tool/task", params: {
  taskId:   string,   // 单次 run 内唯一（`tool-<seq>`）；回填按它去重
  runId:    string,
  toolName: string,   // 只接受 effective 名；宿主解析，本进程不做前缀归一
  arguments: object,  // 省略 = 空对象；数组/null/原始值在 Node 侧即拒绝
  toolSeq:  number,   // 脚本工具调用在 run 内的连续序号（从 0 起，回放与 live 共用）
  agentSeq: number,   // 该调用前已完成的 agent() 数（= 已观察到的 agent_done 数）
} }
```

`taskId` 的 `tool-<n>` 序号与 `toolSeq` 是两套编号：前者只用于 `tool/result` 回填去重，后者是 `tools.jsonl` 的回放位置。

#### `tool/result`

宿主回投脚本工具调用终态（**通知**，无 `id`）。与 `agent/result` 同一条投递契约：任务表提交先于投递，Node 侧按 `taskId` 去重；未知 `taskId` 只记 stderr 警告，不影响其他未决调用。

```typescript
// 回填（宿主 → runner，通知；无 id）
{ jsonrpc: "2.0", method: "tool/result", params: {
  runId:  string,
  taskId: string,   // 与 tool/task 发布一致
  outcome:
    | { kind: "ok", content: string, structuredContent?: unknown }
    | { kind: "error", code: string, tool: string, message: string, details?: unknown },
} }
```

- `kind` 只有 `"ok"` / `"error"`；`content` 恒为字符串，`structuredContent` / `details` 无值时字段省略。
- `code` 取值为 `tool_not_found` | `invalid_arguments` | `tool_unavailable` | `policy_denied` | `approval_unavailable` | `not_executed` | `tool_error` | `protocol_error` | `task_accepted` | `outcome_unknown` | `cancelled`。
- run 结束/kill 时未决调用由 `rejectPendingToolCalls` 以 `cancelled` 结算（不残留悬挂 promise）。

#### `tool/replayed`

续跑回放一条已记录的工具调用（**通知**，无 `id`）。每回放一条在把记录值返回脚本**之前**发送一次；宿主据此把该条目补记进本 run 的 `tools.jsonl`。回放不发布 `tool/task`，不触发审批或真实执行。

```typescript
// 回放（runner → 宿主，通知；无 id）
{ jsonrpc: "2.0", method: "tool/replayed", params: {
  runId: string,
  entry: ToolJournalEntry, // { toolSeq, agentSeq, taskId, toolName, arguments, outcome }
} }
```

### Runner → 宿主（出站通知）

#### `progress/event`

工作流进度事件。宿主通常用于更新 UI panel。

```typescript
{ jsonrpc: "2.0", method: "progress/event", params: {
  type: "run_started"|"phase_started"|"phase_done"|"agent_started"|"agent_progress"|"agent_done"|"log"|"run_done",
  runId: string,
  // 每类事件特有的字段，见 ProgressEvent 类型
} }
```

#### `journal/append`

持久化一条 journal 记录。宿主应将条目追加到磁盘。

```typescript
{ jsonrpc: "2.0", method: "journal/append", params: {
  runId: string,
  entry: { key: string; seq: number; result: AgentRunResult }
} }
```

#### `journal/truncate`

清空当前 run 的 journal。

```typescript
{ jsonrpc: "2.0", method: "journal/truncate", params: { runId: string } }
```

#### `log`

引擎日志消息。

```typescript
{ jsonrpc: "2.0", method: "log", params: {
  level: "debug"|"event"|"warn"|"error",
  message: string,
  meta?: Record<string, unknown>
} }
```

#### `workflow/done`

workflow 执行完毕（不管成功/失败/中止）。

```typescript
{ jsonrpc: "2.0", method: "workflow/done", params: {
  runId: string,
  status: "completed"|"failed"|"killed",
  returnValue?: unknown,
  error?: string,
} }
```

这是**终态通知**——此通知后 runner 进程退出。宿主应清理资源、统计 token 用量、通知用户结果。

---

## 脚本 API：`tool()`（issue #407）

run 期间本进程把 `globalThis.tool` 安装到当前 run（`installToolGlobal`，run 结束时在
`finally` 恢复宿主旧值）。脚本可把确定性 MCP 工具调用交给宿主：

```js
const result = await tool('Read', { file_path: '/abs/path' })
// structuredContent 存在（非 null）→ 返回它；否则返回文本字符串
// 失败 → throw WorkflowToolError { code, tool, message, details? }
```

- `effectiveName` 必须是非空字符串；`arguments` 必须是普通对象或省略。数组 / `null` /
  原始值在本地以 `invalid_arguments` 拒绝，不发布 `tool/task`。
- `tool()` 发布 `tool/task`、等待 `tool/result`，按 `taskId` 结算；不注册 agent 任务，
  不占用 `maxConcurrency`，也不计入 `maxToolCalls` / token。
- 错误抛类型化 `WorkflowToolError`，脚本可 `try/catch`；run 结束/kill 时未决调用以
  `cancelled` 结算。回放记录中的 error 同样抛 `WorkflowToolError`（保留 `code`）。
- **结果不截断**、不做 schema 校验：schema 归 MCP 服务端，本进程只做 wire 形状检查。
- **不是安全边界**：脚本仍可使用 Node 原生能力（`process`、`require` 等）；`tool()`
  只把 MCP 调用交给宿主审批与执行，不提供沙箱或权限提升。
- 嵌套 `workflow()` 在同一进程、同一 run 内串行执行，共用同一 hook，不另建 run。
  宿主未接入工具通道的部署形态（如非会话路径）以 `tool_unavailable` 失败。

### 确定性回放（阶段 2）

宿主把每次脚本工具调用写入本 run 的 `tools.jsonl`（`ToolJournalEntry`，字段：
`toolSeq` / `agentSeq` / `taskId` / `toolName` / `arguments` / `outcome`）。续跑时宿主经
`workflow/start.resumeTools` 回传该文件，Node 侧按下述规则前缀回放：

- **启用条件**：只有 `journalStore.read()` 真正执行（续跑且脚本未变）后
  `configureToolReplay(entries, reusableAgentPrefix)` 才启用回放；fresh run 与
  `scriptChanged` 不读 journal，回放保持禁用。
- **匹配条件**（全部满足才回放）：`entry.toolSeq === cursor`（当前脚本工具序号）、
  `entry.agentSeq <= reusableAgentPrefix`、`toolName` 严格相等、`arguments` 与
  `args ?? {}` JSON 深度相等。
- **命中**：`cursor += 1`，发 `tool/replayed`，按记录 `outcome` 返回
  （`structuredContent ?? content`）或抛错；**不发布 `tool/task`**。
- **首个不匹配转 live**：本次及之后全部走 live，不再尝试回放；`cursor` 仍随每次调用
  递增，并以当前 `cursor` 作为 live `tool/task.toolSeq`。
- 引擎判定 journal 分叉并 `truncate` 时调用 `noteJournalReset()`：只关闭回放，
  **不复位 `cursor`**——本 run 的 `tools.jsonl` 是 append-only，已回放条目继续占位。
- 每次 `agent_done` 进度事件使已完成 agent 计数 +1（含 journal 命中的 agent），
  live `tool/task.agentSeq` 取该计数。
- 非安全边界不变：回放只复用宿主记录的结果，不校验 MCP schema、不截断结果。

---

## 生命周期

```
[1] 宿主 spawn Node 子进程
[2] 宿主 → runner: workflow/start（含脚本源码）
[3] runner 解析脚本，返回 { ok: true }
[4] runner → 宿主: progress/event(run_started)
[5] 循环 {
[6]   runner → 宿主: progress/event(agent_started)
[7]   runner → 宿主: agent/task（通知；宿主登记任务并执行 LLM agent）
[8]   宿主 → runner: agent/result（通知；按 taskId 结算等待表）
[9]   runner → 宿主: progress/event(agent_done)
[10]  runner → 宿主: journal/append
[11] }
[12] runner → 宿主: progress/event(run_done)
[13] runner → 宿主: workflow/done
[14] runner 进程退出（exit 0/1）
[15] 宿主清理（kill 子进程如未退出）
```

### Kill 流程

```
[1] 宿主 → runner: workflow/kill
[2] runner → 宿主: { ok: true }
[3] runner 内部 abort；未完成任务的等待项以 `dead { reason: "killed" }` 结算（`rejectPendingTasks`）
[4] 宿主亦可主动以 `agent/result` 回投取消编码；adapter 统一抛 `WorkflowAbortedError`（旧链等价 `-32000` 响应）
[5] runner → 宿主: workflow/done { status: "killed" }
[6] runner 进程退出
```

### Resume 流程

```
[1] 宿主严格读取历史 journal 与 tools.jsonl；缺失、IO 错误和坏 JSON 行必须失败
[2] 宿主 → runner: workflow/start { resume: [...连续成功前缀], resumeTools: [...工具记录前缀] }
[3] runner 仅复用从 seq=0 开始、key 匹配的连续 ok 调用；工具调用按 toolSeq 从前缀回放，首个不匹配转 live
[4] 首个 dead/skipped 及后缀重新执行，保留 recovered/produced 身份；序号缺口/重复由宿主 strict read 拒绝，需依据工作包 checkpoint 显式重规划
```

---

## 运行结果落盘格式

宿主（Rust 侧 `peri-workflow` crate）在每次运行时把结果落到 `.claude/workflow-runs/<runId>/` 目录，npm 侧 `src/reader.ts`（`read` / `list` 子命令）读取同一格式。**本节是跨侧契约的单一事实源**：Rust 侧写入（`peri-workflow/src/journal.rs`，宿主仓库）↔ npm 侧读取（`src/reader.ts`）双向对齐，任何字段变更必须两侧同步修改。

```
.claude/workflow-runs/<runId>/
├── script.js       # workflow 脚本源码副本（宿主 init_run 写入；reader 不消费）
├── state.json      # 最终状态快照（run_done 时原子写入：先写 .tmp 再 rename）
├── journal.jsonl   # append-only，每行一个 JSON 对象（一个 agent() 调用结果）
└── outputs/        # 超长文本提取文件（可选；return_value 无长文本时不存在）
    └── <label>.txt
```

### state.json 字段

snake_case 命名（`serde` 未做 rename，Rust `RunState` 直出）：

| 字段 | 类型 | 说明 |
|------|------|------|
| `run_id` | string | 运行 UUID（与目录名一致） |
| `workflow_name` | string | 脚本 `meta.name` |
| `status` | string | `"completed"` \| `"failed"` \| `"killed"` |
| `outcome` | string \| 省略 | 引擎级 `"completed"` \| `"blocked"` \| `"failed"` \| `"killed"` \| `"unknown"`；`completed` 不代表验收通过，旧记录缺省为 unknown |
| `attempts` | array \| 省略 | journal attempt 记录；`attemptIndex` 为宿主观测到的执行尝试序号（引擎重试递增，cache 命中保留原值） |
| `return_value` | unknown | 脚本顶层 return（超长字符串已外置为占位符，见下） |
| `script` | string | 脚本源码副本 |
| `args` | unknown \| 省略 | 新运行保存的启动参数，ACP resume 原样恢复；旧记录缺失时不可重建 |
| `max_concurrency` | number \| 省略 | 启动并发数；旧记录缺失时 Rust 兼容默认 3 |
| `started_at` | string | 运行启动 ISO 时间戳 |
| `finished_at` | string \| 省略 | 完成时间戳 |
| `error` | string \| 省略 | 失败原因（非失败时省略） |

### journal.jsonl 字段

每行一条 `JournalEntry`（`key` / `seq` / `result` 为 snake_case 平铺字段）：

| 字段 | 类型 | 说明 |
|------|------|------|
| `key` | string | agent 参数摘要（SHA256 哈希，用于 resume cache-hit） |
| `seq` | number | agent() 调用顺序号（跨子 workflow 单调递增；reader 按它升序） |
| `result` | object | `AgentRunResult`（camelCase wire 命名，见下） |

`result` 为 `kind` 判别联合：

- `{ "kind": "ok", "output": string\|object, "usage": { "outputTokens": number }, "model"?, "toolCount"?, "tokenCount"?, "phase"?, "durationMs"? }`
- `{ "kind": "skipped" }`
- `{ "kind": "dead", "reason"?, "detail"?, "durationMs"?, "tokenCount"? }`

其中 `toolCount` / `tokenCount` / `durationMs` / `phase` / `model` / `reason` / `detail` 均为可选，宿主未提供时省略；reader 读取时 `tokens` 取 `tokenCount`，缺失时回退 `usage.outputTokens`。dead 的 `durationMs`/`tokenCount` 缺省表示宿主未测量，不得当作 0。

### outputs/*.txt 与 `${label}` 占位符

`return_value` 中超过阈值（200 字节，宿主常量 `LONGTEXT_EXTRACT_THRESHOLD_BYTES`）的字符串由宿主 `extract_long_texts`（`journal.rs`）递归提取：

- **label 规则**：字符串在 JSON 中的字段路径即文件名——对象字段按点路径拼接（`a.b` → `outputs/a.b.txt`），数组元素按下标 `[i]`（`items[0]` → `outputs/items[0].txt`）；路径分隔符与控制字符替换为 `_`，连续点折叠为 `._`，空 label 回落 `unnamed`，同一 run 内归一到同名时追加 `-2`、`-3`…（宿主 `output_label` 是唯一权威）。占位符与文件名取同一 label，因此读取侧按 label 解析必然命中。
- 原位置替换为 `${label}` 占位符（如 `"${a.b}"`）；reader 用 `/^\$\{([^}]+)\}$/` **精确匹配整个字符串**，命中且存在对应文件则替换回原文，未命中（无对应文件 / 非整串占位符）时原样保留

---

## 部署模型

### 构建

构建入口为 `src/index.ts`（与 package.json 的 `build` script 一致）：

```bash
bun build src/index.ts \
  --outfile=dist/peri-workflow.js \
  --target=node \
  --format=esm \
  --banner:js='#!/usr/bin/env node'
```

产出：`dist/peri-workflow.js`（单文件，engine 一并打包，零运行时依赖）。

`dist/` 为**构建产物**（`bun run build` 可复现字节）、**不入库**：发布链在打包前构建并随发布包携带；Rust 侧只保存它的 sha256 摘要
（`peri-workflow/src/runner/artifact.rs::WORKFLOW_ENTRY_SHA256`），不把 JS 嵌入二进制。
重建 dist 后必须同步该摘要（`repository_bundle_matches_pinned_digest` 测试守卫）。

### 安装（npm 渠道，非 Peri 宿主）

Peri 运行时不使用该渠道；以下仅用于非 Peri 宿主或手工调试。

```bash
npm install -g @peri-code/workflow
# bin: peri-workflow → dist/peri-workflow.js
```

或通过 npx / bunx 使用（无需全局安装；手工/调试口径）：

```bash
# 必须带显式版本：npx 在全局已有同名 bin 时会静默复用旧版（旧版无 CLI 子命令，
# 命令无输出且 exit 0），`@<version>` 才能强制使用 registry 上的目标版本。
npx -y @peri-code/workflow@0.3.0   # Node.js 环境
bunx @peri-code/workflow@0.3.0     # Bun 环境
```

### 宿主消费（Peri 的非嵌入契约）

Peri 不把 JS 产物嵌入二进制；发行归档在可执行文件旁携带 sidecar：

```text
peri
peri-resources/workflow/<内容身份>/     # 内容身份 = dist/peri-workflow.js 的 sha256 前 12 位
    package.json
    dist/peri-workflow.js
```

`peri-workflow/src/runner/artifact.rs` 的解析顺序：

1. `PERI_WORKFLOW_ARTIFACT_DIR` 显式注入的包目录（部署或测试）；
2. 本机缓存 `~/.peri/workflow/<version>/<内容身份>/`；
3. 可执行文件目录旁的随包 sidecar（命中后 best-effort 发布到缓存以便复用）。

每步都校验包身份（name / version / main / `periProtocolVersion` / `periBuildId`）、入口路径
不越界，以及入口字节的 sha256 等于编译期 `WORKFLOW_ENTRY_SHA256`。
运行时不访问 npm/registry：按顺序尝试上述来源，仅当没有任何来源通过身份与摘要校验时 fail closed。
启动方式为 `node <已校验入口>`；`peri workflow` CLI、preflight 与 workflow 工具共用同一解析路径。

**安全要点**：
- 子进程默认 `env_clear()`，仅注入显式声明的环境变量（`peri-js-runtime` 的 `JsExecutionHost`）
- 禁止注入 `*_API_KEY`、`*_SECRET`、`LANGFUSE_*` 等密钥

---

## 安全性

### 沙箱边界

`@peri-code/workflow` 本身不执行任何形式的沙箱。脚本执行由内部的 workflow-engine 负责：

1. 脚本通过 `Function()` 构造器解析（不是 `eval` 或 `import`）
2. 全局对象受限（禁止 `Date.now()`、`Math.random` 等非确定性 API）
3. 所有对外交互（agent、日志、文件）都通过声明的 hooks（`WorkflowHooks`），不存在直接的系统调用

**安全关键**：`@claude-code-best/workflow-engine` 是私有 npm 包，定义沙箱边界。JS 打包为单文件 bundle 后，沙箱代码随 bundle 一同分发且受摘要 pin 保护，完全可审计（Rust 侧不嵌入该产物；bundle 由发布链构建、不入库）。

### 传输安全

- JSON-RPC 帧**无**认证——默认信任 stdin/stdout 通道
- 如果宿主进程被攻破，攻击者可发送任意 RPC 消息
- 宿主必须验证 `scriptPath` 不超出工作目录范围
- 宿主必须验证 `resumeFromRunId` 为 UUID 格式（防止路径遍历）

---

## 协议版本

发布版本以 package.json 的 `version` 字段为准（本表只记录影响 RPC wire 的变更）。

| 版本 | 变更 |
|------|------|
| 0.1.0 | 初始版本，完整的 JSON-RPC 2.0 协议 |
| 0.2.0 | 新增 CLI 子命令 read/list/validate 与运行结果落盘格式（见上节）；CLI 子命令不参与 JSON-RPC 协议面，**不影响 RPC 兼容性** |
| 0.3.0 | Node wire 改为 `agent/task`（发布通知）+ `agent/result`（回投通知），删除 `agent/run` 请求/响应；`periProtocolVersion` 升为 2 并纳入启动握手校验。**破坏性变更：0.3.0 与 0.2.0 的宿主/引擎不得混用** |

**0.x 阶段的兼容策略**：
- 0.x 的破坏性 wire 变更随 **minor** 发布（0.2 → 0.3 即为此例）：0.3.0 不兼容 0.2.0 的 wire，也不宣称兼容；新增 RPC 方法、参数新增可选字段与新增通知类型仍视为兼容变更。
- `periProtocolVersion` 是 wire 兼容的**唯一权威判定**：宿主的启动握手（`validate_start_ack`）比对协议版本与 `periBuildId`，不匹配即拒绝启动；宿主与引擎必须成对升级。
- 宿主本地 artifact 另有内容 SHA-256 pin（见「宿主消费」节），旧 bundle 因摘要不符不会被采用。
- CLI 子命令（read/list/validate）为独立 argv 入口，仍不改变任何 RPC 方法、参数或错误码。

---

## 参考实现

- 完整宿主实现（Rust）：`peri-workflow` crate（本仓库）
- 协议测试：`peri-workflow/src/protocol_test.rs`
