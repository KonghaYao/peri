# peri-wasm 代码索引

`peri-wasm/src/emscripten/acp.rs` 导出 `PeriWasmAcp`。启动配置注入现有 `ConfigSource`，以 `Resources::open_turso_writable` 打开可写会话存储，并调用 `peri-acp::host::assemble_wasm_server_config` 和 `spawn_acp_server`。`peri-acp::transport::mpsc_transport_pair` 与 `WireBridge` 仅交换原始 JSON-RPC 帧；ACP 方法、Agent 执行、模型和会话规则仍在原有 crate。

Host 配置直接启动普通 ACP runtime，与 stdio 一样不依赖 SDK admission、entry 或 settlement；已移除 `ReverseExecutionAdmission` 接线。历史查询与普通执行保留，不接管旧执行。Cloudflare/SDK 的旧恢复协调消费端仍须单独适配，不能把其旧测试视为当前后端兼容证明。

WASM 依赖图按平台排除 SQLx、进程执行、stdio 和本地工具 MCP packages，保留 `mcp-packages/credentials` 的受信 bootstrap MCP 通道；远程 MCP 客户端与 ACP 承载通路保留。Agent 定义与任务 scope 的共用规则在 `peri-mcp-core/`，原生 package 通过 re-export 复用。配置文件在 Emscripten 虚拟文件系统中由 `peri-config::io` 读取；OAuth 凭证经部署注入的 `OAuthCredentialPort` 和共用 MCP client/server 异步访问。显式关闭会等待凭证清理，意外 drop 的兜底清理在浏览器事件循环中异步执行。`cwd` 是 Emscripten 虚拟文件系统中的绝对路径身份，文件工具需要另接远程 MCP。启动必须传入持久 UUID 作为 machine ID，供 Turso 历史恢复使用。

| 任务 | 入口 |
| --- | --- |
| Emscripten 导出与 ACP Host 生命周期 | `peri-wasm/src/emscripten/acp.rs` |
| 原始 ACP 帧桥 | `peri-acp/src/transport/wire_bridge.rs` |
| WASM Host 装配 | `peri-acp/src/host/assemble.rs` |
| 可写 Turso、当前 schema 与虚拟执行资格 | `peri-resources/src/sessions/remote/{composition,environment,execution}.rs`、`storage_v2_plan.rs` |
| 模型 HTTP/SSE | `peri-model/src/transport/http.rs`、`runtime/retry.rs`；`cloudflare` feature 下原生 Fetch adapter 为 `transport/cloudflare.rs` |
| 工具链 | `scripts/cargo-rmcp-patched.sh`、`scripts/cargo-wasm.sh` |
| ACP 端到端验收 | `scripts/smoke-wasm-acp.mjs` |
| 并发、取消与恢复验收 | `scripts/smoke-wasm-acp-lifecycle.mjs` |
| Workers Emscripten 源码补丁与本地验收 | `patches/emscripten/workers-module-url.patch`、`scripts/prepare-emscripten.sh`、`npm-packages/@peri-sdk/examples/workers/{worker.js,smoke.mjs,wrangler.toml}`；`bun run smoke:workers` 在本地 `workerd` 上按当前 Host 方法表验证四条路径：SDK 宿主形态（`startPeriWasmHost` 只注入 `moduleFactory`，即 peri-cf 形态，响应回带该运行时的 `import.meta.url`）、队列 turn（`session/input/{snapshot,enqueue}` + 投递/turn 结束通知 + `session/close`）、直连 `session/prompt` turn、以及 `session/list`→`session/load` 冷恢复；三条都以有界 `session/close` 收尾。只在本地运行，不代表 hosted Workers 验收 |
| Emscripten 日期格式 | `peri-time/src/calendar.rs` |
| 不依赖 package 的 MCP 共用规则 | `peri-mcp-core/src/{agent_definition,task_scope.rs}` |
| 配置与 OAuth 浏览器适配 | `peri-config/src/io/wasm.rs`、`mcp-packages/credentials/src/client.rs`；凭证 worker 经当前 Tokio Handle 调度，保持 MCP server 和 rmcp 子任务的运行时上下文，回归入口为 `client_test.rs` |
| Cloudflare 网页聊天消费端 | [Peri CF 索引](peri-cf.md)，TS 经 SDK 直连 Turso 查询列表，每聊天 DO 协调执行，无目录 DO 或 D1 |
| 部署环境注入与 Langfuse | `npm-packages/@peri-sdk/src/wasm/loader.ts`、`src/transport/wasm-transport.ts`（同包）；`scripts/cargo-wasm.sh` 导出 `ENV` |

`@peri-code/sdk` 的 `scripts/build.ts` 构建本产物并复制到 npm 包的 `dist/wasm/`，`WasmAcpTransport` 使现有 Agent/Session 接口复用 ACP。示例服务器是 `npm-packages/@peri-sdk/examples/demo/demo-wasm.ts`，复用 `demo.html`。

Peri CF 独立构建 `peri-wasm --features cloudflare`，限制线性内存最大 64 MiB 并记录源码/锁文件/工具链/参数/产物 provenance，SDK 默认构建不变。模型 Fetch adapter 只更换传输，不更换提供商与 ACP 规则；Store 网络仍走既有路径。Emscripten 不支持命令 hook，显式失败而非放行；Skill preload registry 查询避免 `spawn_blocking`。应用 isolate 准入、实际退出证明及 WASM 资源查询见 Peri CF 索引，不以超时或账本存在代替退出证明。

此前合入 CF 的目标构建、链接与 smoke 记录仅覆盖当时的恢复协议，不能替代删除恢复后的验收。2026-10-07 合并复验曾被锁文件的 registry `mio 1.2.4` 阻断，其 Git `1.2.3` Emscripten patch 未使用。后续用户授权补丁修复，已定向锁回固定 Git tag，补丁解析回归及 `scripts/cargo-wasm.sh check --locked --offline --target wasm32-unknown-emscripten -p peri-wasm --features cloudflare` 通过；存在平台条件下的 unused/dead-code 警告。此次只验证目标类型检查，未验证 release 链接、JS 产物或 SDK/CF 消费端 E2E，不推断旧恢复协议兼容。构建条件见 [`peri-wasm/README.md`](../../peri-wasm/README.md)。Hosted Workers 尚未部署验收。
