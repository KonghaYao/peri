# MCP package 代码索引

Builtin MCP 的工具与 server handler 由独立 crate 持有；MCP 实例注册、配置、transport、宿主 context、policy 与生命周期仍由 `peri-middlewares` 装配。每个 crate 的依赖方向面向 `peri-mcp-common`、`peri-agent` / `peri-resources` 等稳定能力接口，不依赖 `peri-middlewares`。

宿主侧入口见 [`peri-middlewares` 代码索引](peri-middlewares.md)。本文是插件包的当前路径索引；设计契约见 [`architecture-contracts.md`](../standards/architecture-contracts.md) 与 [`MCP 适配设计`](../design/mcp-adaptation-v4-part-1.md)。

## Package 路由

| 能力 | crate 与入口 | 主要实现 | 说明 |
| --- | --- | --- | --- |
| MCP 通用映射 | `peri-mcp-common`：`mcp-packages/common/src/lib.rs` | `server_info`、`rmcp_tool_from_base`、`list_tools_of`、`invoke_tool_call`、`parse_optional_u64`；`failure.rs`、`result_mapping.rs`、`process_env.rs` | 多个实例共用的 server metadata、schema 映射、IF-D14 结果映射、安全失败类型和 process environment lock；不依赖宿主 middleware。 |
| Web | `peri-mcp-web`：`mcp-packages/web/src/lib.rs` | `WebMcpServer`、`web_fetch.rs`、`web_search.rs`、`web_common.rs` | 公共工具面为 `WebMcpServer`、`WebFetchTool`、`WebSearchTool`；工具描述位于 `src/descriptions/`。 |
| Artifact | `peri-mcp-artifact`：`mcp-packages/artifact/src/lib.rs` | `ArtifactMcpServer`、`ArtifactTool`、`ArtifactClient` | 上传协议与 Markdown 转 HTML 归该 crate；模板位于 `src/descriptions/md_to_html_template.html`。 |
| Cron | `peri-mcp-cron`：`mcp-packages/cron/src/lib.rs` | `CronMcpServer`、`scheduler.rs`、`tools.rs` | `CronScheduler`、`CronSchedulerPortHandle`、scheduler types 与三种工具由该 crate 导出。宿主 tick task 的 spawn、join 与 reconnect 仍归 `peri-middlewares` runtime。 |
| LSP | `peri-mcp-lsp`：`mcp-packages/lsp/src/lib.rs` | `LspMcpServer`、`LspTool`、`tool.rs`、`formatters.rs` | MCP 工具、协议格式化与配置快照归该 crate。写入后的文档同步中间件 `LspSyncMiddleware` 仍在 `peri-middlewares/src/lsp/middleware.rs`。 |
| Workspace | `peri-mcp-workspace`：`mcp-packages/workspace/src/lib.rs` | `WorkspaceMcpServer`、`workspace.rs`、`filesystem/`、`terminal.rs` | 文件、目录、搜索和 Bash 工具的 handler 与实现归该 crate；输出持久化 / 截断复用 `peri-agent::agent::async_tasks` 的 canonical helper。 |

## 宿主与插件边界

- `peri-middlewares/src/mcp/builtin/mod.rs` 持有 builtin 配置 overlay、实例关闭策略与名称映射；`dispatch.rs` 构造新 crate 导出的 handler。
- `context.rs` 持有注入给 server 的宿主上下文，`runtime.rs` 持有内存 transport、server task 与有界关闭；Cron tick 监督也在这里。
- `peri-acp-types` 持有 builtin 实例与工具声明、名称及 direct/deferred 策略。插件 crate 不复制这些策略表。
- `LspSyncMiddleware` 保留在 `peri-middlewares/src/lsp/`，只消费 host LSP pool 端口同步 `Write` / `Edit`；它不提供 `LspTool`。
- 各插件 crate 内测试覆盖工具行为、handler 路由和 RMCP wire；host transport、dispatch、bridge、policy、tick 与 pool lifecycle 测试留在对应宿主 crate。

## 验证

```bash
cargo test -p peri-mcp-web --lib
cargo test -p peri-mcp-artifact --lib
cargo test -p peri-mcp-cron --lib
cargo test -p peri-mcp-lsp --lib
cargo test -p peri-mcp-workspace --lib
cargo check -p peri-mcp-common
cargo clippy -p peri-mcp-common -p peri-mcp-web -p peri-mcp-artifact -p peri-mcp-cron -p peri-mcp-lsp -p peri-mcp-workspace --all-targets -- -D warnings
```

`peri-mcp-common` 当前没有自有 unit tests；其 helper 的可观察行为由上面的 plugin crate 测试覆盖，`cargo check` 与 workspace strict clippy 验证其独立 crate 边界。宿主 runtime/dispatch 回归继续用 `cargo test -p peri-middlewares --lib -- <module_filter>`；筛选器应指向宿主测试模块，不再指向已迁入 package 的 handler 或工具模块。当前模块名见 `peri-middlewares/src/mcp/mod.rs`。
