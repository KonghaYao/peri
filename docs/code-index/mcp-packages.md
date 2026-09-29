# MCP package 代码索引

Builtin MCP 的工具、server handler 与 LSP 客户端/pool 由独立 crate 持有；MCP 实例注册、配置、transport、宿主 context、policy 与生命周期仍由 `peri-middlewares` 装配。每个 crate 的依赖方向面向 `peri-mcp-common`、`peri-agent` / `peri-acp-types` / `peri-resources` 等稳定能力接口，不依赖 `peri-middlewares`。

宿主侧入口见 [`peri-middlewares` 代码索引](peri-middlewares.md)。本文是插件包的当前路径索引；设计契约见 [`architecture-contracts.md`](../standards/architecture-contracts.md) 与 [`MCP 适配设计`](../design/mcp-adaptation-v4-part-1.md)。

## Package 路由

| 能力 | crate 与入口 | 主要实现 | 说明 |
| --- | --- | --- | --- |
| MCP 通用映射 | `peri-mcp-common`：`mcp-packages/common/src/lib.rs` | `server_info`、`rmcp_tool_from_base`、`list_tools_of`、`invoke_tool_call`、`parse_optional_u64`；`failure.rs`、`result_mapping.rs`、`process_env.rs` | 多个实例共用的 server metadata、schema 映射、IF-D14 结果映射、安全失败类型和 process environment lock；不依赖宿主 middleware。 |
| Web | `peri-mcp-web`：`mcp-packages/web/src/lib.rs` | `WebMcpServer`、`web_fetch.rs`、`web_search.rs`、`web_common.rs` | 公共工具面为 `WebMcpServer`、`WebFetchTool`、`WebSearchTool`；工具描述位于 `src/descriptions/`。 |
| Artifact | `peri-mcp-artifact`：`mcp-packages/artifact/src/lib.rs` | `ArtifactMcpServer`、`ArtifactTool`、`ArtifactClient` | 上传协议与 Markdown 转 HTML 归该 crate；模板位于 `src/descriptions/md_to_html_template.html`。 |
| Cron | `peri-mcp-cron`：`mcp-packages/cron/src/lib.rs` | `CronMcpServer`、`scheduler.rs`、`tools.rs` | `CronScheduler`、`CronSchedulerPortHandle`、scheduler types 与三种工具由该 crate 导出。宿主 tick task 的 spawn、join 与 reconnect 仍归 `peri-middlewares` runtime。 |
| LSP | `peri-mcp-lsp`：`mcp-packages/lsp/src/lib.rs` | `LspMcpServer`、`LspTool`、`LspClient`、`LspServerPool`、`tool.rs`、`formatters.rs`、`config.rs` | MCP 工具、客户端、协议格式化、配置合并与 host 级唯一 pool 归该 crate；host 装配经 `create_host_lsp_pool` 注入同一 `Arc`，`LspSyncMiddleware` 只消费端口，host shutdown 负责调用有界关闭。 |
| Workspace | `peri-mcp-workspace`：`mcp-packages/workspace/src/lib.rs` | `WorkspaceMcpServer`、`workspace.rs`、`git_watch.rs`、`resources/`、`filesystem/`、`terminal.rs`、`fuzzy.rs`、`shell_hints.rs`、`filesystem/path_hints.rs` | 文件、目录、搜索和 Bash 工具的 handler 与实现归该 crate；`git_watch.rs` 持有 `workspace://git/ref` 资源的采样状态机与正文（原宿主 `GitWatchMiddleware` 的逐字搬迁，v4 wave 4 下沉），订阅面（`list_resources` / `read_resource` / `accepted_subscription_filter` / `listen`）在 `workspace.rs`，只在成功的 `tools/call` 后采样且**无订阅者不采样**；输出持久化 / 截断复用 `peri-agent::agent::async_tasks` 的 canonical helper。失败点的可行动诊断（路径 did-you-mean、命令未找到的 PATH 候选）也归该 crate，见下节。 |
| Workspace 资源面（W1，2026-09-29） | `peri-mcp-workspace`：`mcp-packages/workspace/src/resources/mod.rs` | `resources/{mod,skills,agents,instructions,builtin,scan,path,frontmatter}.rs`；契约在 `peri-acp-types/src/workspace_resources.rs`（URI/`_meta`/skills DTO 的唯一事实源） | skills / agents / 项目指令三类**来源**的扫描、manifest 与只读提供（`resources/list|read|templates/list`、`skills/list|get`）：本地三根 + 插件根 + builtin 静态资产（`resources/builtin/skills/`，**唯一副本**，宿主注册表经 `include_str!` 引用至 W4）；输入由宿主经 `WorkspaceResourcesInput` 构造期注入（`with_resources`）；生产装配点已接线（W4a，2026-09-29）——会话装配只装载 meta 面（J6 的 `peri-meta://workspace/{section_id}`，`disable_bundled = true` 作域隔离），skill / agent 根与真实关闭位留 W4b；顶层三路径不装载（资源面未接线）；未知 URI 按 MCPP 约定 `-32602`、目标不存在 `-32002`；**不注册任何技能工具**（`SkillTool`/`DiscoverSkillsTool` 留宿主，J3）。 |

## 宿主与插件边界

- `peri-middlewares/src/mcp/builtin/mod.rs` 持有 builtin 配置 overlay、实例关闭策略与名称映射；`dispatch.rs` 构造新 crate 导出的 handler。
- `context.rs` 持有注入给 server 的宿主上下文，`runtime.rs` 持有内存 transport、server task 与有界关闭；Cron tick 监督也在这里。
- `peri-acp-types` 持有 builtin 实例与工具声明、名称及 direct/deferred 策略。插件 crate 不复制这些策略表。
- `LspSyncMiddleware` 保留在 `peri-middlewares/src/lsp/`，只消费 host 装配从 `peri_mcp_lsp::create_host_lsp_pool` 投影出的 `LspPoolPort`，与 builtin `lsp` handler 共用同一 pool；它不提供 `LspTool`，也不构造 pool。
- 各插件 crate 内测试覆盖工具行为、handler 路由和 RMCP wire；host transport、dispatch、bridge、policy、tick 与 pool lifecycle 测试留在对应宿主 crate。
- 失败恢复指引由工具在失败点生成（`ToolFailure { recovery, detail }`），宿主不做事后文本匹配：路径候选在 `workspace/src/filesystem/path_hints.rs`（Read / Edit / Glob / Grep / folder_operations 的"目标不存在"分支；Edit 的 `old_string not found` 文本失败除外），命令候选在 `workspace/src/shell_hints.rs`（仅 exit 127 + command-not-found 文案），两者共用 `workspace/src/fuzzy.rs` 的 Skim 排序。原 `peri-agent` / `peri-middlewares` 的 error_suggest 框架已删除。

## 验证

```bash
cargo test -p peri-mcp-web --lib
cargo test -p peri-mcp-artifact --lib
cargo test -p peri-mcp-cron --lib
cargo test -p peri-mcp-lsp --lib
cargo test -p peri-mcp-workspace --lib
cargo test -p peri-mcp-workspace --lib -- git_watch
cargo test -p peri-mcp-workspace --lib -- resources
cargo test -p peri-middlewares --lib -- mcp::builtin::dispatch
cargo check -p peri-mcp-common
cargo clippy -p peri-mcp-common -p peri-mcp-web -p peri-mcp-artifact -p peri-mcp-cron -p peri-mcp-lsp -p peri-mcp-workspace --all-targets -- -D warnings
```

`peri-mcp-common` 当前没有自有 unit tests；其 helper 的可观察行为由上面的 plugin crate 测试覆盖，`cargo check` 与 workspace strict clippy 验证其独立 crate 边界。宿主 runtime/dispatch 回归继续用 `cargo test -p peri-middlewares --lib -- <module_filter>`；筛选器应指向宿主测试模块，不再指向已迁入 package 的 handler 或工具模块。当前模块名见 `peri-middlewares/src/mcp/mod.rs`。
