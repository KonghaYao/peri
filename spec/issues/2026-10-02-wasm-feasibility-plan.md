# Peri WASM ACP 接入验收

状态：Node/Bun 的 WASM SDK 接入已完成并通过目标运行时验收；Wrangler 本地 `workerd` 的 ACP、模型及持久会话验收通过，Cloudflare 托管部署尚未验收。原始目标见 `/Users/konghayao/Downloads/peri-wasm-plan.md`。分支为 `refactor/wasm`。

## 已确定的边界

- `@peri-code/sdk` 保留 `Sandbox`、`ManagedAgents`、`Agent`、`Session` 的公共接口。`Sandbox.transportFactory` 在原生 stdio transport 与 `WasmAcpTransport` 之间替换底层，前端继续复用 `examples/demo/demo.html`。
- `peri-wasm::PeriWasmAcp` 只承载原始 JSON-RPC 帧。现有 `peri-acp` Host 处理 ACP 方法、事件、会话、取消和 MCP 反向请求；Rust 和 TypeScript 均不复制 ACP dispatcher。
- `peri-resources` 以同一 `SessionResources` 契约接入可写 Turso adapter。Machine、Workspace、Session 归属及执行快照保存在 Turso schema 13；WASM 虚拟工作区只承载宿主观测和进程内 lease。固定 machine UUID 使跨 Host 重启的会话定位保持一致。Native 同时编译 SQLite/Turso 并按 locator 装配；WASM 仅编译 Turso，SQLite/SQLx 不进入目标依赖图。
- Emscripten 目标编译闭包排除 SQLx、`peri-process`、stdio transport、本地 LSP 和全部 builtin MCP。远程 HTTP MCP client 与 ACP 承载的 MCP server 保留。文件工具需由外部 MCP 提供。
- Workers 模块 URL 从 Emscripten 源码补丁接入 `Module.mainScriptUrlOrBlob`；禁用动态执行，Emscripten 日期使用 UTC。构建产物不做文本改写。
- 模型、远程 MCP 与 Turso 复用 reqwest native/Hyper HTTP。Cloudflare Mio/Tokio 分支提供 hosted Tokio I/O；Emscripten 6.0.10 的 epoll/异步 DNS 补丁、Hyper 异步 DNS 补丁和 Bun socket 补丁由仓库脚本固定。

## 已有运行证据与本次重构复验

下表记录 WASM 分支已有的运行结果；SQLite/Turso 装配边界重构后须重跑目标编译、依赖图及原生 Resources 回归，不能把旧结果视为本次变更已验收。

| 验收面 | 命令或脚本 | 结果 |
| --- | --- | --- |
| 目标编译 | `./scripts/cargo-wasm.sh check --locked -p peri-wasm --target wasm32-unknown-emscripten` | 通过 |
| 实际产物 | `./scripts/cargo-wasm.sh build --locked --release -p peri-wasm --target wasm32-unknown-emscripten` | 通过，生成 JS 与 WASM |
| 目标依赖图 | `./scripts/cargo-wasm.sh tree --locked -p peri-wasm --target wasm32-unknown-emscripten -e normal` | 无 SQLx、进程 crate 或 builtin MCP 包 |
| ACP、模型与持久会话 | `PERI_WASM_PROFILE=release node scripts/smoke-wasm-acp.mjs` | initialize/new/prompt/list/load、模型 HTTP 调用、通知与 Host 重启后加载均通过；使用真实本地 sqld |
| 远程 MCP | `PERI_WASM_PROFILE=release node scripts/smoke-wasm-remote-mcp.mjs` | ACP 反向连接、工具发现与 echo 工具调用通过 |
| 生命周期 | `node scripts/smoke-wasm-acp-lifecycle.mjs` | 两个并存会话、运行中取消、独立会话完成、Host 关闭重启后历史重放通过 |
| 真实 SDK | `bun test`，含 `tests/wasm-acp-integration.test.ts` | 41/41 通过；真实 sqld 与模型服务下完成 Agent 创建、发送、列表和加载 |
| Bun demo | `bun run demo:wasm` 配本地 sqld 与模拟 Anthropic SSE | 首页、前端脚本、Session 创建与列表、`/api/session/send` 交付、SSE 回复均通过 |
| Workers 本地运行 | `cd npm-packages/@peri-sdk && bun install --frozen-lockfile && bun run smoke:workers` | Wrangler 4.147.0 本地 `workerd` 中 initialize/new/prompt、模型 HTTP、session/list/load 及 Host 关闭后恢复通过；使用真实本地 sqld 与模拟模型 |
| 原生回归 | `./scripts/cargo-rmcp-patched.sh build --locked --workspace` | 通过 |

存储离线契约 111 项通过、21 项需要云库而跳过。中间件选定的 builtin/动态 MCP 回归 11 项通过；ACP 帧桥原生测试 3 项通过。全量文件大小扫描发现 15 个存量超限文件，本次新增/修改的源码未超限。

## 部署范围

当前已验证宿主是 Node、Bun 与 Wrangler 本地 `workerd`。Workers 需要 `nodejs_compat`、独立 ES 模块、预编译 WASM module import，以及 `mainScriptUrlOrBlob` 启动参数；托管部署、生产端点、资源限制和 Workers 中的 TypeScript SDK 入口尚无验收。普通浏览器不能直接使用 Node socket 路线。文件工具需由外部 MCP 提供；全部 builtin MCP 按本次要求从 WASM 中移除。

已按 `DOC-UPDATE-001` 核对代码索引、构建文档和本文件；尚未 push 或部署。
