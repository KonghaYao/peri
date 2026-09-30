# MCP package 代码索引

Builtin MCP 的工具、server handler 与 LSP 客户端/pool 由独立 crate 持有；MCP 实例注册、配置、transport、宿主 context、policy 与生命周期仍由 `peri-middlewares` 装配。每个 crate 的依赖方向面向 `peri-mcp-common`、`peri-agent` / `peri-acp-types` / `peri-resources` 等稳定能力接口，不依赖 `peri-middlewares`。

宿主侧入口见 [`peri-middlewares` 代码索引](peri-middlewares.md)。本文是插件包的当前路径索引；设计契约见 [`architecture-contracts.md`](../standards/architecture-contracts.md) 与 [`MCP 适配设计`](../design/mcp-adaptation-v4-part-1.md)。

## Package 路由

| 能力 | crate 与入口 | 主要实现 | 说明 |
| --- | --- | --- | --- |
| MCP 通用映射 | `peri-mcp-common`：`mcp-packages/common/src/lib.rs` | `server_info`、`rmcp_tool_from_base`、`list_tools_of`、`invoke_tool_call`、`parse_optional_u64`；`failure.rs`、`result_mapping.rs`、`process_env.rs` | 多个实例共用的 server metadata、schema 映射、IF-D14 结果映射、安全失败类型和 process environment lock；不依赖宿主 middleware。 |
| Agent 定义格式 | `peri-mcp-common`：`mcp-packages/common/src/agent_definition/` | `ClaudeAgent`、`ClaudeAgentFrontmatter`、`ToolsValue`、`parse_agent_file` | 本地与远端 MCP Agent 共用的纯数据与 Markdown/YAML 解析，不扫描目录、不读取文件、不签发授权；保留 omitted / explicit zero / allowlist 三态。workspace 提供扫描与原始资源，宿主 registry 消费定义并按来源实施信任、批准和执行策略；不向计算核心添加 YAML 依赖。 |
| Workspace 宿主读取 | `peri-mcp-workspace`：`mcp-packages/workspace/src/{image,file_observation}.rs` | custom request `image/read` / `workspace/readText`，由 `workspace.rs::on_custom_request` 分派 | 图片附件读取、格式与大小校验及归因/LSP 的完整正文读取归工具环境；不增加模型工具。宿主 image reader 与 `peri-middlewares/src/workspace_io.rs` 只经当前会话可见的 builtin workspace 句柄读取，关闭/不可得不回落宿主磁盘。 |
| Workspace 输出与分支 | `mcp-packages/workspace/src/{output_store,git_branch}.rs`；契约 `peri-acp-types/src/workspace_output.rs` | `output/store`、`workspace/gitBranch`；`resources/read` 的 `peri-output://` URI | 输出仅在工具环境持久化；URI 绑定实例，路径供同一环境的 Read 分页；不增加模型工具。宿主适配 `peri-middlewares/src/mcp/client/output_store.rs` 与 `workspace_io.rs` 门控会话可见性、关闭与换代，不可得不回落本机；git 超时与进程树清理归 provider。 |
| Web | `peri-mcp-web`：`mcp-packages/web/src/lib.rs` | `WebMcpServer`、`web_fetch.rs`、`web_search.rs`、`web_common.rs` | 公共工具面为 `WebMcpServer`、`WebFetchTool`、`WebSearchTool`；工具描述位于 `src/descriptions/`。 |
| Artifact | `peri-mcp-artifact`：`mcp-packages/artifact/src/lib.rs` | `ArtifactMcpServer`、`ArtifactTool`、`ArtifactClient` | 上传协议与 Markdown 转 HTML 归该 crate；模板位于 `src/descriptions/md_to_html_template.html`。 |
| Cron | `peri-mcp-cron`：`mcp-packages/cron/src/lib.rs` | `CronMcpServer`、`scheduler.rs`、`tools.rs` | `CronScheduler`、`CronSchedulerPortHandle`、scheduler types 与三种工具由该 crate 导出。宿主 tick task 的 spawn、join 与 reconnect 仍归 `peri-middlewares` runtime。 |
| LSP | `peri-mcp-lsp`：`mcp-packages/lsp/src/lib.rs` | `LspMcpServer`、`LspTool`、`LspClient`、`LspServerPool`、`tool.rs`、`formatters.rs`、`config.rs` | MCP 工具、客户端、协议格式化、配置合并与 host 级唯一 pool 归该 crate；host 装配经 `create_host_lsp_pool` 注入同一 `Arc`，`LspSyncMiddleware` 只消费端口，host shutdown 负责调用有界关闭。 |
| Workspace | `peri-mcp-workspace`：`mcp-packages/workspace/src/lib.rs` | `WorkspaceMcpServer`、`workspace.rs`、`git_watch.rs`、`resources/`、`filesystem/`、`terminal.rs`、`fuzzy.rs`、`shell_hints.rs`、`filesystem/path_hints.rs` | 文件、目录、搜索和 Bash 工具的 handler 与实现归该 crate；`git_watch.rs` 持有 `workspace://git/ref` 资源的采样状态机与正文（原宿主 `GitWatchMiddleware` 的逐字搬迁，v4 wave 4 下沉），订阅面（`list_resources` / `read_resource` / `accepted_subscription_filter` / `listen`）在 `workspace.rs`，只在成功的 `tools/call` 后采样且**无订阅者不采样**；输出持久化使用 `peri-mcp-common::shell`，纯截断复用 `peri-agent::agent::async_tasks`；common 的 `shell_executor.rs` 承载本地后台执行，`shell_output.rs` 承载两路 tee 输出，Agent 仅通过 `ShellExecutor` 注入端口消费。失败点的可行动诊断（路径 did-you-mean、命令未找到的 PATH 候选）也归该 crate，见下节。 |
| Workspace 资源面 | `peri-mcp-workspace`：`mcp-packages/workspace/src/resources/mod.rs` | `resources/{mod,skills,agents,instructions,builtin,scan,path,frontmatter}.rs`；URI/`_meta`/DTO 契约在 `peri-acp-types/src/workspace_resources.rs`，skills 扩展键在 `peri-acp-types/src/skills.rs` | skills / agents / 项目指令的扫描与只读提供（`resources/list|read|templates/list`、`skills/list|get`）。`WorkspaceResourcesInput` 构造期注入：skill 根为 User/Project/Plugin（带 `plugin_name`），builtin 静态资产唯一副本在 provider，由 `disableBundledSkills` 控制；`skillsDir` 全局根装配已删除。会话装配也注入 Agent 项目/插件根、指令与 meta 面；未装 provider 不声明 skills 扩展。未知 URI `-32602`、不存在 `-32002`；不注册技能工具，宿主 `SkillTool`/`DiscoverSkillsTool` 仍聚合来源。 |

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

`peri-mcp-common` 的 Agent 定义与 shell/输出契约测试归本 crate；运行 `cargo test -p peri-mcp-common --lib`。宿主 runtime/dispatch 回归用 `cargo test -p peri-middlewares --lib -- <module_filter>`；筛选器应指向宿主测试模块，不再指向已迁入 package 的 handler 或工具模块。当前模块名见 `peri-middlewares/src/mcp/mod.rs`。
