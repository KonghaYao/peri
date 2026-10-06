# peri-wasm 代码索引

`peri-wasm/src/emscripten/acp.rs` 导出 `PeriWasmAcp`。启动配置注入现有 `ConfigSource`，以 `Resources::open_turso_writable` 打开可写会话存储，并调用 `peri-acp::host::assemble_wasm_server_config` 和 `spawn_acp_server`。`peri-acp::transport::mpsc_transport_pair` 与 `WireBridge` 仅交换原始 JSON-RPC 帧；ACP 方法、Agent 执行、模型和会话规则仍在原有 crate。

Host 配置通过 `AcpRequestBridge` 接入共用 `ReverseExecutionAdmission`，与 stdio 相同地将准入、entry 和 settlement 交给客户端 SDK，不在 WASM 层旁路执行权威。Cloudflare 消费端的持久账本与控制恢复见 Peri CF 索引。

WASM 依赖图按平台排除 SQLx、进程执行、stdio 和本地工具 MCP packages，保留 `mcp-packages/credentials` 的受信 bootstrap MCP 通道；远程 MCP 客户端与 ACP 承载通路保留。Agent 定义与任务 scope 的共用规则在 `peri-mcp-core/`，原生 package 通过 re-export 复用。配置文件在 Emscripten 虚拟文件系统中由 `peri-config::io` 读取；OAuth 凭证经部署注入的 `OAuthCredentialPort` 和共用 MCP client/server 异步访问。显式关闭会等待凭证清理，意外 drop 的兜底清理在浏览器事件循环中异步执行。`cwd` 是 Emscripten 虚拟文件系统中的绝对路径身份，文件工具需要另接远程 MCP。启动必须传入持久 UUID 作为 machine ID，供 Turso 历史恢复使用。

| 任务 | 入口 |
| --- | --- |
| Emscripten 导出与 ACP Host 生命周期 | `peri-wasm/src/emscripten/acp.rs` |
| 原始 ACP 帧桥 | `peri-acp/src/transport/wire_bridge.rs` |
| WASM Host 装配 | `peri-acp/src/host/assemble.rs` |
| 可写 Turso、schema v12 与虚拟执行资格 | `peri-resources/src/sessions/remote/{composition,environment,execution}.rs`、`storage_v2_plan.rs` |
| 模型 HTTP/SSE | `peri-model/src/transport/http.rs`、`runtime/retry.rs` |
| 工具链 | `scripts/cargo-rmcp-patched.sh`、`scripts/cargo-wasm.sh` |
| ACP 端到端验收 | `scripts/smoke-wasm-acp.mjs` |
| 并发、取消与恢复验收 | `scripts/smoke-wasm-acp-lifecycle.mjs` |
| Workers Emscripten 源码补丁与本地验收 | `patches/emscripten/workers-module-url.patch`、`scripts/prepare-emscripten.sh`、`npm-packages/@peri-sdk/examples/workers/{worker.js,smoke.mjs,wrangler.toml}` |
| Emscripten 日期格式 | `peri-time/src/calendar.rs` |
| 不依赖 package 的 MCP 共用规则 | `peri-mcp-core/src/{agent_definition,task_scope.rs}` |
| 配置与 OAuth 浏览器适配 | `peri-config/src/io/wasm.rs`、`mcp-packages/credentials/src/client.rs`；凭证 worker 经当前 Tokio Handle 调度，保持 MCP server 和 rmcp 子任务的运行时上下文，回归入口为 `client_test.rs` |
| Cloudflare 网页聊天消费端 | [Peri CF 索引](peri-cf.md)，TS 经 SDK 直连 Turso 查询列表，每聊天 DO 协调执行，无目录 DO 或 D1 |
| 部署环境注入与 Langfuse | `npm-packages/@peri-sdk/src/wasm/loader.ts`、`src/transport/wasm-transport.ts`（同包）；`scripts/cargo-wasm.sh` 导出 `ENV` |

`@peri-code/sdk` 的 `scripts/build.ts` 构建本产物并复制到 npm 包的 `dist/wasm/`，`WasmAcpTransport` 使现有 Agent/Session 接口复用 ACP。示例服务器是 `npm-packages/@peri-sdk/examples/demo/demo-wasm.ts`，复用 `demo.html`。

Emscripten 目标、原生 workspace 构建与 release 链接已通过。`scripts/cargo-wasm.sh` 自动对 Emscripten 6.0.10 应用 Cloudflare epoll/异步 DNS、Bun socket 和 Workers 模块 URL 源码补丁；Hyper DNS 的目标补丁在 `patches/`。链接时禁用动态执行，Emscripten 日期格式使用 UTC，生成产物无需改写。Node 的 ACP/sqld/模型、远程 MCP、并发取消恢复 smoke，以及 SDK 的 WASM 集成测试和 Bun demo HTTP/SSE 路径已通过。Bun/Wrangler 本地 `workerd` probe 验证 ACP prompt、模型 HTTP 和 Host 重启后 Turso 恢复；Hosted Workers 尚未部署验收。构建条件见 [`peri-wasm/README.md`](../../peri-wasm/README.md)。
