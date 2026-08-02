# @peri-code/workflow

> Workflow runner for the Perihelion agent — JSON-RPC 2.0 stdio bridge with built-in CLI subcommands.
> **Host-language-agnostic**: any language that can spawn a Node.js child process and speak JSON-RPC over stdin/stdout can integrate.

## 这是什么？

`@peri-code/workflow` 是一个独立的 Node.js 进程，承担两件事：

1. **工作流编排（JSON-RPC 模式，主路径）**——解析用户编写的 workflow 脚本，按 DAG 逻辑调度 agent 调用（`agent()`、`parallel()`、`phase()`），并通过 JSON-RPC 2.0 协议与宿主进程通信。
2. **运行结果读取（CLI 子命令模式）**——`read` / `list` 子命令读取宿主落盘的运行结果，无需再维护独立的读取脚本（合一）。

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
│  phase()          │                  │                     │
└───────────────────┘                  └─────────────────────┘
```

无参数运行时为 JSON-RPC 模式（宿主集成）；首参为 `read` / `list` / `--help` 时为 CLI 模式，互不干扰。

## 安装

```bash
npm install -g @peri-code/workflow
```

或通过 npx / bunx 自动下载（无需全局安装）：`npx -y @peri-code/workflow` / `bunx @peri-code/workflow`

**消费端无需构建**：发布的包自带 `dist/peri-workflow.js`（自包含单文件，内嵌 engine、零运行时依赖），Node.js ≥ 18 直接运行。

## 快速开始

### 1. 写一个 workflow 脚本

脚本为 ESM 格式，**只允许一个 `export const meta`**（name + description 必填）；其余代码是脚本主体，engine 注入顶层自由函数 `agent()` / `parallel()` / `phase()`，结果用**顶层 `return`** 返回（禁止 `export default`）。

```javascript
// workflow-demo.js
export const meta = {
  name: 'demo-workflow',
  description: 'A simple demo',
}

const research = await agent('Research quantum computing', {
  agentType: 'web-researcher',
})

const summary = await agent('Summarize the findings', {
  model: 'claude-sonnet-4-20250514',
})

return { research, summary }
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

完整的 JSON-RPC 2.0 协议规范（传输层、消息类型、请求/响应/错误码约定、时序）见 [DESIGN.md](./DESIGN.md)。

### 宿主 → Runner

| 方法 | 类型 | 描述 |
|------|------|------|
| `workflow/start` | Request | 启动一个 workflow run（含脚本源码，立即返回，异步执行） |
| `workflow/kill` | Request | 中止当前 workflow（发送 `workflow/done`，status: "killed"） |

### Runner → 宿主

| 方法 | 类型 | 描述 |
|------|------|------|
| `agent/run` | Request | 执行一个 agent 调用（宿主必须响应；`-32000` 表示 aborted） |
| `progress/event` | Notification | 进度更新（run_started / phase_started / phase_done ...） |
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

## 读取运行结果（CLI 子命令）

宿主（Rust 侧 `peri-workflow`）把每次运行落到 `.claude/workflow-runs/<runId>/`：
`state.json`（状态与 return value，超长文本以 `${label}` 占位符外置）、`outputs/`（外置长文本）、`journal.jsonl`（每个 agent 调用的结果）。

`peri-workflow` 二进制内置读取子命令（从仓库任意子目录运行，自动向上定位 `workflow-runs` 根；占位符原位替换回完整内容）：

```bash
peri-workflow read <runId>         # 完整报告：状态 + return value + 每个 agent 全量输出
peri-workflow read <runId> --short # 仅状态与 agent 统计表
peri-workflow read <runId> --json  # 结构化 JSON（stdout 只输出 JSON，供脚本消费）
peri-workflow list                 # 列出所有 run（按结束时间倒序）
peri-workflow list --json          # 同上，JSON 形式
peri-workflow validate <script.mjs> # 校验 workflow 脚本语法（exit 0/1）
peri-workflow validate <script.mjs> --json # 结构化校验结果
peri-workflow --help               # 用法帮助
```

无参数运行时保持 JSON-RPC 模式（宿主集成）；带子命令即 CLI 模式，互不干扰。通过 `npx` / `bunx` 也可直接用：

```bash
npx -y @peri-code/workflow list
npx -y @peri-code/workflow read 019fc025-c4d9-7d52-a30a-7409229e3148 --short
npx -y @peri-code/workflow validate my-workflow.mjs
```

### validate：agent 写脚本前的语法校验

workflow 脚本由引擎执行，但引擎的 `parseScript` 只查语法 / export / import；**常见的运行时错误**（`workflow.agent(...)` 旧式调用、缺 `export const meta`、无顶层 `return`）要等执行时才炸。`validate` 在引擎检查之外补了这些静态检查，agent 写完脚本先跑一遍再执行：

```bash
peri-workflow validate my-workflow.mjs
# ✓ my-workflow.mjs 校验通过 (demo-workflow)
# ✗ my-workflow.mjs 校验失败（2 个错误）：
#   ✗ 检测到旧式调用 workflow.agent(...)：引擎注入的是顶层自由函数，请改为直接调用 agent(...)（无需 workflow. 前缀）。
#   ✗ workflow 脚本必须包含 export const meta = { name, description }（宿主依赖 meta.name 标识 workflow）。请补上 meta 声明。
```

检查项：

| 级别 | 检查 | 来源 |
|------|------|------|
| error | 语法错误 / 多余 export（含 `export default`）/ `import` / meta 非字面量或缺字段 | 引擎 `parseScript` |
| error | `workflow.agent(...)` 等旧式调用（含修复指引） | 静态检查 |
| error | 缺 `export const meta = { name, description }` | 静态检查 |
| warning | 无 `return` 语句（脚本将返回 undefined） | 静态检查 |

```bash
# 结构化输出（供脚本 / agent 消费）
peri-workflow validate my-workflow.mjs --json
# { "file": "...", "ok": false, "meta": {...}, "errors": [...], "warnings": [...] }
```

> **旧格式脚本说明**：早期遗留的 workflow 脚本可能没有 `export const meta`（或使用
> `workflow.agent(...)` 旧式调用），validate 会将其标为 error。这是**预期行为**——按当前
> 引擎与宿主协议，新脚本必须带 `export const meta = { name, description }`；旧脚本
> 执行前建议先用 validate 检查并按提示修正（错误信息自带修复指引）。

## 构建与测试（参与开发）

源码按职责拆分为 `src/` 下的模块（types / rpc / adapter / server / reader / cli / index），
构建时由 `bun build` 打包为单文件；测试用 `bun test`（`test/` 目录：各模块单测 +
`test/e2e.test.ts` 黑盒模拟宿主通过 JSON-RPC 驱动真实二进制全链路）。

```bash
# 1. 类型检查（tsc --noEmit）
npm run typecheck
# 2. 单元 + e2e 测试（bun test）
npm test
# 3. 覆盖率门禁（行覆盖率 ≥ 80%，不达标 exit 1）
npm run test:coverage
# 4. 打包为单文件（零外部依赖）
npm run build   # 等价于：
#   bun build src/index.ts \
#     --outfile=dist/peri-workflow.js \
#     --target=node \
#     --format=esm \
#     --banner:js='#!/usr/bin/env node'

# 验证
node dist/peri-workflow.js < /dev/null && echo "OK"
```

输出是一个完整的自包含 Node.js 脚本（~35KB），无需 `node_modules`。

## 发布（Publishing）

### 前置条件

- npm 账号已登录且拥有 `@peri-code` scope 的发布权限：`npm login`
- bun 可用（构建依赖 `bun build`）

### 发布流程（checklist）

```bash
# 1. bump 版本（自动更新 package.json 并打 git tag）
npm version patch        # 或 minor / major
# 2. 构建 + 测试 + 覆盖率自检（prepublishOnly 会自动重跑）
npm run build && npm run test:coverage && node dist/peri-workflow.js < /dev/null && echo OK
# 3. 发布
npm publish
```

**4. 同步 Peri 宿主（关键联动）**：`peri-workflow/src/runner.rs:19` 的 `WORKFLOW_NPM_VERSION` 常量
必须改为新版本——它决定本地固定安装路径 `~/.peri/workflow/<version>/` 与
`npm install @peri-code/workflow@<version>` 的版本约束。未同步会导致：
本地固定安装停留在旧版、`npx` 回退路径拿到新版，出现版本漂移。

**5. 验证**：重编 peri 后触发一次 workflow（首次调用自动 `npm install --prefix ~/.peri/workflow/<ver>`，
旧版本目录按版本号共存、不清理，无需迁移）。

> 若协议有变更，同步更新 [DESIGN.md](./DESIGN.md) 的「协议版本」表。

### 发布内容

`package.json` 的 `files` 白名单：

| 文件 | 用途 |
|------|------|
| `dist/` | 可执行主文件（`bin` 指向；含 JSON-RPC 引擎 + read/list 子命令） |
| `src/` | 源码（`types` 指向 `src/index.ts`，供 TS 宿主获得类型声明） |
| `README.md` / `DESIGN.md` | 文档 |

### 谁在用、要不要构建

| 使用方 | 接入方式 | 需要构建？ |
|--------|----------|-----------|
| Peri 宿主（Rust） | `ensure_workflow_install` 自动 `npm install --prefix ~/.peri/workflow/<ver> @peri-code/workflow@<ver>`，`node` 直跑 `dist/peri-workflow.js` | 不需要（仅 Peri 自身重编时同步版本常量） |
| 临时手动 / 调试 | `npx -y @peri-code/workflow` 或 `bunx @peri-code/workflow`（每次联网解析 registry） | 不需要 |
| 其他宿主集成 | 按上方「宿主实现检查清单」spawn 子进程 + JSON-RPC 协议 | 不需要 |
| 参与开发 / 发布 | 仓库内 `bun build src/index.ts`（npm run build） | 需要（bun） |

**结论：消费端永远不需要构建**——`dist/peri-workflow.js` 是自包含单文件（内嵌
`@claude-code-best/workflow-engine`、零运行时依赖），Node.js ≥ 18 直接跑。

## 技术栈

- **TypeScript**（`src/` 多模块：types / rpc / adapter / server / reader / cli / index）→ `bun build` → 单文件 JS
- **测试**：`bun test`（`test/` 目录，模块单测 + e2e 黑盒模拟；行覆盖率 ≥ 80% 门禁）
- **类型检查**：`tsc --noEmit`（严格模式）
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
