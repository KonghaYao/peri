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

**核心原则**：宿主拥有 agent 执行权，本进程拥有 workflow 编排权。边界在 `agent/run` RPC 调用——宿主决定如何执行 LLM agent（选择模型、管理 API 密钥、运行工具），本进程决定 agent 之间如何编排（并行、流水线、重试）。

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
  script:   string,        // workflow ESM 脚本源码
  args?:    unknown,       // 用户传入的参数（args 变量）
  maxConcurrency?: number, // 最大并发 agent 数
} }
// 响应
{ jsonrpc: "2.0", id: 1, result: { ok: true } }
```

**时序**：响应立即返回（验证 stage），workflow 自体异步执行。宿主必须在收到响应后随时准备好处理后续的 `agent/run` 请求和 `progress/event` 通知。

#### `workflow/kill`

中止当前运行的 workflow。

```typescript
// 请求
{ jsonrpc: "2.0", id: 2, method: "workflow/kill" }
// 响应
{ jsonrpc: "2.0", id: 2, result: { ok: true } }
```

**效果**：设置 `AbortController.abort()`。正在运行的 agent 会收到 signal 并返回 `{ kind: "dead" }`。workflow 停止推进并发送 `workflow/done` 通知（status: "killed"）。

### Runner → 宿主（出站请求）

#### `agent/run`

执行一个 agent 调用。这是 **唯一的双向请求**——runner 请求宿主执行 LLM agent，宿主必须响应。

```typescript
// 请求
{ jsonrpc: "2.0", id: 100, method: "agent/run", params: {
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
// 成功响应
{ jsonrpc: "2.0", id: 100, result: {
  kind: "ok",
  output:     string|object,
  usage:      { outputTokens: number },
  model?:     string,
  toolCount?: number,
  tokenCount?: number,
} }
// 跳过响应
{ jsonrpc: "2.0", id: 100, result: { kind: "skipped" } }
// 失败响应
{ jsonrpc: "2.0", id: 100, result: {
  kind: "dead",
  reason?: "no-structured-output"|"runagent-threw"|"worktree-failed"|"unknown",
  detail?: string,
} }
// 中止（特殊错误码）
{ jsonrpc: "2.0", id: 100, error: { code: -32000, message: "aborted" } }
```

**宿主职责**：
1. 加载正确的 LLM 模型（使用 `model` 参数）
2. 执行 ReAct 循环（prompt → LLM → tool calls → results → LLM → ...）
3. 管理 API 密钥（只待在宿主内存中，绝不影响 runner 环境变量）
4. 应用 `allowedTools` 过滤
5. 返回结果时包含 `usage.outputTokens`

**错误码约定**：
- `-32000`：aborted（workflow 被 kill，agent 中止）— runner 会转为 `WorkflowAbortedError`
- `-32603`：内部错误 — runner 记录为 `{ kind: "dead", reason: "runagent-threw" }`

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

## 生命周期

```
[1] 宿主 spawn Node 子进程
[2] 宿主 → runner: workflow/start（含脚本源码）
[3] runner 解析脚本，返回 { ok: true }
[4] runner → 宿主: progress/event(run_started)
[5] 循环 {
[6]   runner → 宿主: progress/event(agent_started)
[7]   runner → 宿主: agent/run（宿主执行 LLM agent）
[8]   宿主 → runner: AgentRunResult
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
[3] runner 内部 abort
[4] 正在执行的 agent → 宿主: agent/run RPC 的响应应为 { error: { code: -32000 } }
[5] runner → 宿主: workflow/done { status: "killed" }
[6] runner 进程退出
```

### Resume 流程

```
[1] 宿主从磁盘加载历史 journal
[2] 宿主 → runner: workflow/start { resume: [...历史entries] }
[3] runner 重放 journal，跳过已命中的 agent 调用
[4] 仅执行新任务
```

---

## 部署模型

### 构建

```bash
bun build runner.ts \
  --outfile=dist/peri-workflow.js \
  --target=node \
  --format=esm \
  --banner:js='#!/usr/bin/env node'
```

产出：`dist/peri-workflow.js`（单文件，零外部依赖，~25KB）

### 安装

```bash
npm install -g @peri-code/workflow
# bin: peri-workflow → dist/peri-workflow.js
```

或直接使用构建产物：

```bash
node dist/peri-workflow.js
```

### 宿主发现

宿主的 `resolve_binary()` 逻辑：

1. 在 PATH 中搜索 `peri-workflow`
2. 判断 `$HOME/.peri/peri-workflow` 是否存在

找到后可执行文件后：

```rust
Command::new(binary)
    .env_clear()
    .envs(safe_child_env())  // 仅注入 HOME/PATH/USER 等基本变量
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
```

**安全要点**：
- 必须 `env_clear()` 清除父进程环境变量
- 禁止注入 `*_API_KEY`、`*_SECRET`、`LANGFUSE_*` 等密钥
- 仅注入 `HOME`、`PATH`、`USER` 等基本变量及 `PERI_*` 相关业务变量

---

## 安全性

### 沙箱边界

`@peri-code/workflow` 本身不执行任何形式的沙箱。脚本执行由内部的 workflow-engine 负责：

1. 脚本通过 `Function()` 构造器解析（不是 `eval` 或 `import`）
2. 全局对象受限（禁止 `Date.now()`、`Math.random` 等非确定性 API）
3. 所有对外交互（agent、日志、文件）都通过声明的 hooks（`WorkflowHooks`），不存在直接的系统调用

**安全关键**：`@claude-code-best/workflow-engine` 是私有 npm 包，定义沙箱边界。JS 打包为单文件 bundle 后，沙箱代码内嵌为仓库自有代码，完全可审计。

### 传输安全

- JSON-RPC 帧**无**认证——默认信任 stdin/stdout 通道
- 如果宿主进程被攻破，攻击者可发送任意 RPC 消息
- 宿主必须验证 `scriptPath` 不超出工作目录范围
- 宿主必须验证 `resumeFromRunId` 为 UUID 格式（防止路径遍历）

---

## 协议版本

| 版本 | 变更 |
|------|------|
| 0.1.0 | 初始版本，完整的 JSON-RPC 2.0 协议 |

**向前兼容承诺**：
- 新增 RPC 方法不会破坏现有宿主
- 现有方法的参数新增可选字段不会破坏现有宿主
- 通知的新类型不会破坏现有宿主
- 破坏性变更（删除方法、改变参数语义）会触发主版本号升级

---

## 参考实现

- 完整宿主实现（Rust）：`peri-workflow` crate（本仓库）
- 协议测试：`peri-workflow/src/protocol_test.rs`
