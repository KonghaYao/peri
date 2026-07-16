# @peri-code/workflow

> Workflow runner for the Perihelion agent — JSON-RPC 2.0 stdio bridge.
> **Host-language-agnostic**: any language that can spawn a Node child process and speak JSON-RPC over stdin/stdout can integrate.

## 这是什么？

`@peri-code/workflow` 是一个独立的 Node.js 进程，负责**工作流编排**——解析用户编写的 workflow 脚本，按 DAG 逻辑调度 agent 调用（`agent()`、`parallel()`、`pipeline()`、`phase()`），并通过 JSON-RPC 2.0 协议与宿主进程通信。

宿主进程（Rust、Go、Python 等）负责**执行 agent**——管理 LLM API 密钥、运行 ReAct 循环、执行工具调用。

```
用户编写 workflow 脚本
        │
        ▼
┌───────────────────┐   JSON-RPC 2.0   ┌─────────────────────┐
│  peri-workflow    │◄────────────────►│  宿主进程 (Rust)     │
│  (此包, Node.js)  │   stdin/stdout   │                     │
│                   │                  │  执行 agent()       │
│  编排 agent()     │──agent/run──────►│  管理 LLM 密钥       │
│  parallel()       │                  │  运行工具            │
│  pipeline()       │◄──AgentRunResult─│                     │
│                   │                  │                     │
└───────────────────┘                  └─────────────────────┘
```

## 安装

```bash
npm install -g @peri-code/workflow
```

或通过 npx / bunx 自动下载（无需全局安装）：`npx -y @peri-code/workflow` / `bunx @peri-code/workflow`

## 快速开始

### 1. 写一个 workflow 脚本

```javascript
// workflow-demo.js
export const meta = {
  name: 'demo-workflow',
  description: 'A simple demo',
}

export default async function(workflow) {
  const research = workflow.agent('Research quantum computing', {
    agentType: 'web-researcher'
  })

  const summary = workflow.agent('Summarize the findings', {
    model: 'claude-sonnet-4-20250514'
  })

  workflow.log(`Research: ${research}`)
  return summary
}
```

### 2. 宿主启动 workflow

宿主需要实现两个能力：

**A) spawn Node 子进程**：

```rust
// 伪代码 (Rust) — bun 环境优先 bunx，否则 npx
let cmd = if has_bun() { ("bunx", &["@peri-code/workflow"]) } else { ("npx", &["-y", "@peri-code/workflow"]) };
let child = Command::new(cmd.0)
    .args(cmd.1)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .spawn()?;

// 发送 workflow/start
send_json(&mut child.stdin, json!({
    "jsonrpc": "2.0", "id": 1,
    "method": "workflow/start",
    "params": {
        "runId": "<uuid>",
        "cwd": "/home/user/project",
        "budgetTotal": 200_000,
        "script": fs::read_to_string("workflow-demo.js")?,
    }
}));
```

**B) 响应 agent/run 请求**：

```rust
// 读取 stdout，解析 agent/run
if method == "agent/run" {
    let prompt = params["prompt"].as_str().unwrap();
    let result = execute_llm_agent(prompt, &api_key).await?;
    send_json(&mut child.stdin, json!({
        "jsonrpc": "2.0", "id": request.id,
        "result": { "kind": "ok", "output": result, "usage": {"outputTokens": 500} }
    }));
}
```

### 3. 接收结果

宿主监听 `workflow/done` 通知：

```rust
// stdout 中收到
{ "jsonrpc": "2.0", "method": "workflow/done", "params": {
    "runId": "<uuid>",
    "status": "completed",
    "returnValue": "..."
}}
```

## 协议

完整的 JSON-RPC 2.0 协议规范见 [DESIGN.md](./DESIGN.md)。

### 宿主 → Runner

| 方法 | 描述 |
|------|------|
| `workflow/start` | 启动一个 workflow run |
| `workflow/kill` | 中止当前 workflow |

### Runner → 宿主

| 方法 | 类型 | 描述 |
|------|------|------|
| `agent/run` | Request | 执行一个 agent 调用（宿主必须响应） |
| `progress/event` | Notification | 进度更新 |
| `journal/append` | Notification | 持久化 journal |
| `workflow/done` | Notification | workflow 完成 |

## 宿主实现检查清单

想对接 `@peri-code/workflow`，你的宿主需要实现：

- [ ] spawn 子进程（`npx -y @peri-code/workflow` 或 `bunx @peri-code/workflow`）
- [ ] 在 `stdin` 上写 newline-delimited JSON，在 `stdout` 上读 newline-delimited JSON
- [ ] 发送 `workflow/start` 请求（含脚本源码）
- [ ] 处理 `agent/run` 请求：执行 LLM agent 并返回 `AgentRunResult`
- [ ] 处理 `progress/event` 通知（更新 UI progress panel）
- [ ] 处理 `journal/append` 通知（持久化到磁盘）
- [ ] 处理 `workflow/done` 通知（清理资源、通知用户）
- [ ] 实现 `workflow/kill` 发送（用户中止 workflow 时）
- [ ] **安全：子进程 env_clear()，禁止注入 API 密钥**

## 构建（参与开发）

```bash
# 打包为单文件（零外部依赖）
bun build runner.ts \
  --outfile=dist/peri-workflow.js \
  --target=node \
  --format=esm \
  --banner:js='#!/usr/bin/env node'

# 验证
node dist/peri-workflow.js < /dev/null && echo "OK"
```

输出是一个完整的自包含 Node.js 脚本（~25KB），无需 `node_modules`。

## 技术栈

- **TypeScript** (`.ts`) → `bun build` → 单文件 JS
- **运行时**：Node.js ≥ 18
- **内部引擎**：`@claude-code-best/workflow-engine`（构建时内嵌）
- **无生产依赖**：打包后为单文件，零运行时依赖

## 架构

详见 [DESIGN.md](./DESIGN.md)。核心原则：

- **宿主拥有 agent 执行权**（模型选择、API 密钥、工具、安全）
- **此包拥有 workflow 编排权**（DAG 调度、并行、重试、journal 回放）
- **边界在 `agent/run`**：宿主决定"怎么执行 agent"，此包决定"agent 之间怎么编排"

## 许可

Apache-2.0
