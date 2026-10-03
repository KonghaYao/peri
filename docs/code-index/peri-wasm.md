# peri-wasm 代码索引

`peri-wasm/src/emscripten/acp.rs` 导出 `PeriWasmAcp`。启动配置注入现有 `ConfigSource`，以 `Resources::open_turso_writable` 打开可写会话存储，并调用 `peri-acp::host::assemble_wasm_server_config` 和 `spawn_acp_server`。`peri-acp::transport::mpsc_transport_pair` 与 `WireBridge` 仅交换原始 JSON-RPC 帧；ACP 方法、Agent 执行、模型和会话规则仍在原有 crate。

WASM 依赖图按平台排除 SQLx、进程执行、stdio 和全部 builtin MCP；远程 MCP 客户端与 ACP 承载通路保留。`cwd` 是 Emscripten 虚拟文件系统中的绝对路径身份，文件工具需要另接远程 MCP。启动必须传入持久 UUID 作为 machine ID，供 Turso 历史恢复使用。

| 任务 | 入口 |
| --- | --- |
| Emscripten 导出与 ACP Host 生命周期 | `peri-wasm/src/emscripten/acp.rs` |
| 原始 ACP 帧桥 | `peri-acp/src/transport/wire_bridge.rs` |
| WASM Host 装配 | `peri-acp/src/host/assemble.rs` |
| 可写 Turso、schema v12 与虚拟执行资格 | `peri-resources/src/sessions/remote/composition.rs`、`storage_v2_plan.rs`、`wasm_execution.rs` |
| 模型 HTTP/SSE | `peri-model/src/transport/http.rs`、`runtime/retry.rs` |
| 工具链 | `scripts/cargo-rmcp-patched.sh`、`scripts/cargo-wasm.sh` |
| ACP 端到端验收 | `scripts/smoke-wasm-acp.mjs` |
| 并发、取消与恢复验收 | `scripts/smoke-wasm-acp-lifecycle.mjs` |

`@peri-code/sdk` 的 `scripts/build.ts` 构建本产物并复制到 npm 包的 `dist/wasm/`，`WasmAcpTransport` 使现有 Agent/Session 接口复用 ACP。示例服务器是 `npm-packages/@peri-sdk/examples/demo/demo-wasm.ts`，复用 `demo.html`。

Emscripten 目标、原生 workspace 构建与 release 链接已通过。`scripts/cargo-wasm.sh` 自动对 Emscripten 6.0.10 应用 Cloudflare epoll/异步 DNS 和 Bun socket 补丁；Hyper DNS 的目标补丁在 `patches/`。Node 的 ACP/sqld/模型、远程 MCP、并发取消恢复 smoke，以及 SDK 的 WASM 集成测试和 Bun demo HTTP/SSE 路径已通过。构建条件见 [`peri-wasm/README.md`](../../peri-wasm/README.md)。
