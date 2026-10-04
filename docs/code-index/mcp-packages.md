# MCP package 代码索引

Builtin MCP 的工具、server handler 与 LSP 客户端/pool 由独立 crate 持有；配置来源 I/O 由独立 `peri-mcp-config` 数据面持有，session MCP 实例注册、配置投影、transport、宿主 context、policy 与生命周期仍由 `peri-middlewares` 装配。每个 crate 的依赖方向面向 `peri-mcp-common`、`peri-agent` / `peri-acp-types` / `peri-resources` 等稳定能力接口，不依赖 `peri-middlewares`。

宿主侧入口见 [`peri-middlewares` 代码索引](peri-middlewares.md)。本文是插件包的当前路径索引；设计契约见 [`architecture-contracts.md`](../standards/architecture-contracts.md) 与 [`MCP 适配设计`](../design/mcp-adaptation-v4-part-1.md)。

## Package 路由

Emscripten 目标仅引入 MCP 通用映射与配置数据面；builtin MCP package 不进入
`peri-wasm` 依赖图。`peri-mcp-config/src/wasm.rs` 在宿主映射的 MEMFS 和进程环境上
执行同步读写、路径查询与字节 CAS，避免启动本地配置 server 或 TCP client。
原生目标继续使用 `ConfigurationClient`、跨进程锁及现有 server。

配置数据面还提供 `ConfigurationClient::read_environment`（`ReadEnvironment`）：
只按名称读取 provider 环境，缺省与非法名称/编码区分，不回落计算宿主。
该数据面还提供 `write_text_if_unchanged`：expected 正文字节 CAS，进程与按目标路径
协调的跨进程锁覆盖比较和 atomic replacement；不合作编辑器与跨文件事务不在保证内。
[`peri-config`](peri-config.md) 已持有核心 settings/provider/MCP/Langfuse/UI 的纯 typed
规则与资源开关 projection、scoped snapshot/revision/explain/updateCAS；输入 provider 不获得组装权威。
LSP、插件生命周期、hook 格式、OS 执行环境与存储 locator/credentials 仍按专属边界维护。

| 能力 | crate 与入口 | 主要实现 | 说明 |
| --- | --- | --- | --- |
| 配置数据面 | `peri-mcp-config`：`mcp-packages/config/src/{lib,client,server}.rs`；契约 `peri-acp-types/src/configuration.rs` | `ConfigurationClient`、`ConfigurationMcpServer`、`config/execute`、`write_text_if_unchanged` | 独立 bootstrap byte/env/path I/O 与字节 CAS，合作写者共享跨进程锁；不决定 typed 规则或发布 snapshot。默认 duplex，可首次访问前选 TCP provider，不新增 daemon/model 工具，不回落本机；core 权威见 [peri-config](peri-config.md) |
| MCP 通用映射 | `peri-mcp-common`：`mcp-packages/common/src/lib.rs` | `server_info`、`rmcp_tool_from_base`、`list_tools_of`、`invoke_tool_call`、`parse_optional_u64`；`failure.rs`、`result_mapping.rs`、`process_env.rs` | 多个实例共用的 server metadata、schema 映射、IF-D14 结果映射、安全失败类型和 process environment lock；不依赖宿主 middleware。 |
| Agent 定义格式 | `peri-mcp-common`：`mcp-packages/common/src/agent_definition/` | `ClaudeAgent`、`ClaudeAgentFrontmatter`、`ToolsValue`、`parse_agent_file` | 本地与远端 MCP Agent 共用的纯数据与 Markdown/YAML 解析，不扫描目录、不读取文件、不签发授权；保留 omitted / explicit zero / allowlist 三态。workspace 提供扫描与原始资源，宿主 registry 消费定义并按来源实施信任、批准和执行策略；不向计算核心添加 YAML 依赖。 |
| Workspace 宿主读取与回退 | `peri-mcp-workspace`：`mcp-packages/workspace/src/{image,file_observation,file_rewind}.rs` | custom request `image/read` / `workspace/readText` / `workspace/readMention` / `workspace/rewindFiles`，由 `workspace.rs::on_custom_request` 分派 | 图片附件、归因/LSP 正文和 `@path` 内容由工具环境读取，关闭/不可得不回落宿主磁盘。Rewind 在 Workspace 端以 task scope/fence 准入，预检历史变更后回退文件；ACP 只编排历史。 |
| Workspace 输出与分支 | `mcp-packages/workspace/src/{output_store,git_branch}.rs`；契约 `peri-acp-types/src/workspace_output.rs` | `output/store`、`workspace/gitBranch`；`resources/read` 的 `peri-output://` URI | 输出仅在工具环境持久化；URI 绑定实例，路径供同一环境的 Read 分页；不增加模型工具。宿主适配 `peri-middlewares/src/mcp/client/output_store.rs` 与 `workspace_io.rs` 门控会话可见性、关闭与换代，不可得不回落本机；git 超时与进程树清理归 provider。 |
| Web | `peri-mcp-web`：`mcp-packages/web/src/lib.rs` | `WebMcpServer`、`web_fetch.rs`、`web_search.rs`、`web_common.rs` | 公共工具面为 `WebMcpServer`、`WebFetchTool`、`WebSearchTool`；工具描述位于 `src/descriptions/`。 |
| Artifact | `peri-mcp-artifact`：`mcp-packages/artifact/src/lib.rs` | `ArtifactMcpServer`、`ArtifactTool`、`ArtifactClient` | 上传协议与 Markdown 转 HTML 归该 crate；模板位于 `src/descriptions/md_to_html_template.html`。 |
| Cron | `peri-mcp-cron`：`mcp-packages/cron/src/lib.rs` | `CronMcpServer`、`scheduler.rs`、`tools.rs` | `CronScheduler`、`CronSchedulerPortHandle`、scheduler types 与三种工具由该 crate 导出。宿主 tick task 的 spawn、join 与 reconnect 仍归 `peri-middlewares` runtime。 |
| LSP | `peri-mcp-lsp`：`mcp-packages/lsp/src/lib.rs` | `LspMcpServer`、`LspTool`、`LspClient`、`LspServerPool`、`tool.rs`、`formatters.rs`、`config.rs` | MCP 工具、客户端、协议格式化、配置合并与 host 级唯一 pool 归该 crate；host 装配经 `create_host_lsp_pool` 注入同一 `Arc`，`LspSyncMiddleware` 只消费端口，host shutdown 负责调用有界关闭。 |
| Workspace | `peri-mcp-workspace`：`mcp-packages/workspace/src/lib.rs` | `WorkspaceMcpServer`、`workspace.rs`、`git_watch.rs`、`resources/`、`filesystem/`、`terminal.rs`、`fuzzy.rs`、`shell_hints.rs`、`filesystem/path_hints.rs` | 文件、目录、搜索和 Bash 工具的 handler 与实现归该 crate；`git_watch.rs` 持有 `workspace://git/ref` 资源的采样状态机与正文（原宿主 `GitWatchMiddleware` 的逐字搬迁，v4 wave 4 下沉），订阅面（`list_resources` / `read_resource` / `accepted_subscription_filter` / `listen`）在 `workspace.rs`，只在成功的 `tools/call` 后采样且**无订阅者不采样**；输出持久化使用 `peri-mcp-common::shell`，纯截断复用 `peri-agent::agent::async_tasks`；common 的 `shell_executor.rs` 承载本地后台执行，`shell_output.rs` 承载两路 tee 输出，Agent 仅通过 `ShellExecutor` 注入端口消费。失败点的可行动诊断（路径 did-you-mean、命令未找到的 PATH 候选）也归该 crate，见下节。 |
| Workspace 资源面 | `peri-mcp-workspace`：`mcp-packages/workspace/src/resources/mod.rs` | `resources/{mod,skills,agents,instructions,builtin,scan,path,frontmatter}.rs`；URI/`_meta`/DTO 契约在 `peri-acp-types/src/workspace_resources.rs`，skills 扩展键在 `peri-acp-types/src/skills.rs` | skills / agents / 项目指令的扫描与只读提供（`resources/list|read|templates/list`、`skills/list|get`）。`WorkspaceResourcesInput` 构造期注入：skill 根为 User/Project/Plugin（带 `plugin_name`），builtin 静态资产唯一副本在 provider，由 core `snapshot.resources().disable_bundled_skills` 经资源输入控制，正常 consumer 不重读全局关闭位；`skillsDir` 全局根装配已删除。会话装配也注入 Agent 项目/插件根、指令与 meta 面；未装 provider 不声明 skills 扩展。未知 URI `-32602`、不存在 `-32002`；不注册技能工具，宿主 `SkillTool`/`DiscoverSkillsTool` 仍聚合来源。 |

## 宿主与插件边界

Workspace 后台 Bash 当前运行时入口：`workspace/src/shell_tasks.rs` 持有任务状态并通过 `workspace.rs` 实现 `tasks/get|cancel`、`subscriptions/listen` 的 `taskIds` 通知；独立 CLI 与 builtin dispatch 都构造 `WorkspaceMcpServer::standalone`。Peri 的客户端能力声明在 `peri-middlewares/src/mcp/client/service.rs`，工具回执与订阅消费在 `tool_bridge.rs`、`client/subscription.rs`。ACP 会话装配不再向 Workspace 注入 TaskManager/完成回调。`WorkspaceInstanceInput` 只保留在直接构造器供旧工具测试使用，尚非编译依赖清理。

会话任务 scope 的 owner 入口在 `common/src/task_scope.rs` 与 `workspace/src/{shell_tasks,workspace}.rs`：宿主创建 `TaskScopeAuthority`；取得 Store-issued execution owner token 后调用 `issue_execution(session_id, epoch, nonce)` 编码受信 scope 元数据；`WorkspaceMcpServer::with_task_scope_authority` 启用按请求解析 `_meta["peri/taskScope"]` 并执行 scope 与代际栅栏。`workspace/taskFence` 以执行代际的 `(epoch, nonce)` 单调推进 owner floor、拦截旧 Agent，并等待旧准入创建完成登记；此执行代际独立于 task close/open 的 scope epoch。`workspace/taskSnapshot` 返回 scoped 任务、cursor、epoch 与 closing 状态，`workspace/taskChanges` 从 cursor 取变更并可有界等待。`workspace/taskClose(epoch)` 关闭准入，待在途创建登记完成后返回 barrier cursor；`workspace/taskOpen(epoch)` 仅在全部任务终态且无在途创建时推进 epoch 并恢复准入，旧 epoch 的 close/open 请求被拒绝。Workspace owner 在请求取消后仍持有创建 admission 直到任务登记，防止 close barrier 漏掉已发起的 shell。终态 transition ID 由 owner 固定；`tasks/cancel` 只把任务置于 working/cancellation requested，shell executor 清理后回调才发布 Cancelled。独立 CLI 通过部署可信的 MCP 连接接收 scope 元数据，无本机密钥文件或签名交互；floor 激活后旧代际与旧版无代际 scope 均不能再修改 scope。独立 CLI 在 Workspace `.peri` 目录写入 fsync 的 owner marker，防止进程崩溃后从空内存目录错误接管；遗留 marker 令新实例启动失败，须证明孤儿 shell 已清理后才能人工移除。无认证的独立 HTTP CLI 只绑定 loopback；跨主机连接需要部署层提供受信传输与准入。未注入 authority 的 `standalone` 仍属 legacy 非 scoped 模式，不具备按 session 冷恢复。目标语义见 [Session 异步任务架构](../design/session-async-tasks.md)。

- `peri-middlewares/src/mcp/builtin/mod.rs` 持有 builtin 配置 overlay、实例关闭策略与名称映射；`dispatch.rs` 构造新 crate 导出的 handler。
- `context.rs` 持有注入给 server 的宿主上下文，`runtime.rs` 持有内存 transport、server task 与有界关闭；Cron tick 监督也在这里。
- `peri-acp-types` 持有 builtin 实例与工具声明、名称及 direct/deferred 策略。插件 crate 不复制这些策略表。
- `LspSyncMiddleware` 保留在 `peri-middlewares/src/lsp/`，只消费 host 装配从 `peri_mcp_lsp::create_host_lsp_pool` 投影出的 `LspPoolPort`，与 builtin `lsp` handler 共用同一 pool；它不提供 `LspTool`，也不构造 pool。
- 各插件 crate 内测试覆盖工具行为、handler 路由和 RMCP wire；host transport、dispatch、bridge、policy、tick 与 pool lifecycle 测试留在对应宿主 crate。
- 失败恢复指引由工具在失败点生成（`ToolFailure { recovery, detail }`），宿主不做事后文本匹配：路径候选在 `workspace/src/filesystem/path_hints.rs`（Read / Edit / Glob / Grep / folder_operations 的"目标不存在"分支；Edit 的 `old_string not found` 文本失败除外），命令候选在 `workspace/src/shell_hints.rs`（仅 exit 127 + command-not-found 文案），两者共用 `workspace/src/fuzzy.rs` 的 Skim 排序。原 `peri-agent` / `peri-middlewares` 的 error_suggest 框架已删除。

## 验证

```bash
cargo test -p peri-mcp-config --lib
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
