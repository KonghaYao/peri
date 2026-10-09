# MCP package 代码索引

Builtin MCP 的工具与 server handler 由独立 crate 持有；配置来源 I/O 由独立 `peri-mcp-config` 数据面持有，session MCP 实例注册、配置投影、transport、宿主 context、policy 与生命周期仍由 `peri-middlewares` 装配。每个 crate 的依赖方向面向 `peri-mcp-common`、`peri-agent` / `peri-acp-types` / `peri-resources` 等稳定能力接口，不依赖 `peri-middlewares`。

Workspace scope 关闭的真实资源证明见 `workspace/src/shell_tasks.rs`：任务 owner 在进程退出、管道回收和终态通知完成后登记 `resources_settled`，scope barrier 不以客户端取消意图或模型任务状态替代资源结算。主会话共享环境不能绕过历史子会话资源/required 工作检查；冷关闭缺失可信 owner 连接时保留 Incomplete，不提前删除关闭意图。

宿主侧入口见 [`peri-middlewares` 代码索引](peri-middlewares.md)。本文是插件包的当前路径索引；设计契约见 [`architecture-contracts.md`](../standards/architecture-contracts.md) 与 [`MCP 适配设计`](../design/mcp-adaptation-v4-part-1.md)。

Workspace git ref 更新由 `workspace/src/workspace.rs` 在 MCP 通知
`_meta["peri/messageKind"]` 中逐条声明 `info`；其他 MCP server 可按同一字段声明
`info` 或 `defer`。类型字面量与键在 `peri-acp-types/src/mcp.rs` 统一定义。

## Package 路由

Emscripten 目标引入 MCP 通用映射、配置数据面与凭证 bootstrap MCP；builtin 工具 package 不进入
`peri-wasm` 依赖图。`peri-mcp-config/src/wasm.rs` 在宿主映射的 MEMFS 和进程环境上
执行同步读写、路径查询与字节 CAS，避免启动本地配置 server 或 TCP client。
原生目标继续使用 `ConfigurationClient`、跨进程锁及现有 server。

配置数据面还提供 `ConfigurationClient::read_environment`（`ReadEnvironment`）：
只按名称读取 provider 环境，缺省与非法名称/编码区分，不回落计算宿主。
该数据面还提供 `write_text_if_unchanged`：expected 正文字节 CAS，进程与按目标路径
协调的跨进程锁覆盖比较和 atomic replacement；不合作编辑器与跨文件事务不在保证内。
[`peri-config`](peri-config.md) 已持有核心 settings/provider/MCP/Langfuse/UI 的纯 typed
规则与资源开关 projection、scoped snapshot/revision/explain/updateCAS；输入 provider 不获得组装权威。

Workspace Shell Tasks 的直接发起会话由可信 task scope capability 绑定；
`mcp-packages/workspace/src/shell_tasks.rs` 的 snapshot/changes 显式携带 `initiatorSessionId`，
支持客户端连接重建后恢复归属，不从模型参数、父会话或 transcript 猜测。wire 契约见
`mcp-packages/workspace/src/workspace_tasks_wire_test.rs`；owner 记录仍驻内存，不承诺 owner 进程重启恢复。
插件生命周期、hook 格式、OS 执行环境与存储 locator/credentials 仍按专属边界维护。

| 能力 | crate 与入口 | 主要实现 | 说明 |
| --- | --- | --- | --- |
| 配置数据面 | `peri-mcp-config`：`mcp-packages/config/src/{lib,client,server}.rs`；契约 `peri-acp-types/src/configuration.rs` | `ConfigurationClient`、`ConfigurationMcpServer`、`config/execute`、`write_text_if_unchanged` | 独立 bootstrap byte/env/path I/O 与字节 CAS，合作写者共享跨进程锁；不决定 typed 规则或发布 snapshot。默认 duplex，可首次访问前选 TCP provider，不新增 daemon/model 工具，不回落本机；core 权威见 [peri-config](peri-config.md) |
| MCP 通用映射 | `peri-mcp-common`：`mcp-packages/common/src/lib.rs` | `server_info`、`rmcp_tool_from_base`、`list_tools_of`、`invoke_tool_call`、`parse_optional_u64`；`failure.rs`、`result_mapping.rs`、`process_env.rs` | 多个实例共用的 server metadata、schema 映射、IF-D14 结果映射、保留实际原因的失败类型和 process environment lock；不依赖宿主 middleware。 |
| Agent 定义格式 | `peri-mcp-core`：`peri-mcp-core/src/agent_definition/` | `ClaudeAgent`、`ClaudeAgentFrontmatter`、`ToolsValue`、`parse_agent_file` | 本地与远端 MCP Agent 共用的纯数据与 Markdown/YAML 解析，不扫描目录、不读取文件、不签发授权；保留 omitted / explicit zero / allowlist 三态。workspace 提供扫描与原始资源，宿主 registry 消费定义并按来源实施信任、批准和执行策略；不向计算核心添加 YAML 依赖。 |
| Workspace 宿主读取与回退 | `peri-mcp-workspace`：`mcp-packages/workspace/src/{image,file_observation,file_rewind}.rs` | custom request `image/read` / `workspace/readText` / `workspace/readMention` / `workspace/rewindFiles`，由 `workspace.rs::on_custom_request` 分派 | 图片附件、归因正文和 `@path` 内容由工具环境读取，关闭/不可得不回落宿主磁盘；`workspace/readMention` 按含端点行范围（起止相同仍读该行，回归见 `file_observation_test.rs::mention_equal_range_endpoints_returns_requested_line`）+ 行数上限 + 模型可见正文的 UTF-8 字节预算读取（按行跳读，不整文件进内存），截断返回 `truncated` 与可继续读取行号；项目指令 `@import` 展开共享累计读取预算，最终文本另有字节预算与有界截断说明，`scan_local` / `read_bounded_text` 区分「正常不存在」与读取/编码/超预算错误。Rewind 在 Workspace 端以 task scope capability 准入，预检历史变更后回退文件；ACP 只编排历史。 |
| Workspace 输出与分支 | `mcp-packages/workspace/src/{output_store,git_branch}.rs`；契约 `peri-acp-types/src/workspace_output.rs` | `output/store`、`workspace/gitBranch`；`resources/read` 的 `peri-output://` URI | 输出仅在工具环境持久化；URI 绑定实例，路径供同一环境的 Read 分页；不增加模型工具。宿主适配 `peri-middlewares/src/mcp/client/output_store.rs` 与 `workspace_io.rs` 门控会话可见性、关闭与换代，不可得不回落本机；git 超时与进程树清理归 provider。 |
| Web | `peri-mcp-web`：`mcp-packages/web/src/lib.rs` | `WebMcpServer`、`web_fetch.rs`、`web_search.rs`、`web_common.rs` | 公共工具面为 `WebMcpServer`、`WebFetchTool`、`WebSearchTool`；工具描述位于 `src/descriptions/`。 |
| Artifact | `peri-mcp-artifact`：`mcp-packages/artifact/src/lib.rs` | `ArtifactMcpServer`、`ArtifactTool`、`ArtifactClient` | 上传协议与 Markdown 转 HTML 归该 crate；模板位于 `src/descriptions/md_to_html_template.html`。 |
| Cron | `peri-mcp-cron`：`mcp-packages/cron/src/lib.rs` | `CronMcpServer`、`scheduler.rs`、`tools.rs` | `CronScheduler`、`CronSchedulerPortHandle`、scheduler types 与三种工具由该 crate 导出。宿主 tick task 的 spawn、join 与 reconnect 仍归 `peri-middlewares` runtime。 |
| Workspace | `peri-mcp-workspace`：`mcp-packages/workspace/src/lib.rs` | `WorkspaceMcpServer`、`workspace.rs`、`git_watch.rs`、`resources/`、`filesystem/`、`terminal.rs`、`fuzzy.rs`、`shell_hints.rs`、`filesystem/path_hints.rs` | 文件、目录、搜索和 Bash 工具的 handler 与实现归该 crate；`git_watch.rs` 持有 `workspace://git/ref` 资源的采样状态机与正文（原宿主 `GitWatchMiddleware` 的逐字搬迁，v4 wave 4 下沉），订阅面（`list_resources` / `read_resource` / `accepted_subscription_filter` / `listen`）在 `workspace.rs`，只在成功的 `tools/call` 后采样且**无订阅者不采样**；输出持久化使用 `peri-mcp-common::shell`，纯截断复用 `peri-agent::agent::async_tasks`；common 的 `shell_executor.rs` 承载本地后台执行，`shell_output.rs` 承载两路 tee 输出，Agent 仅通过 `ShellExecutor` 注入端口消费。失败点的可行动诊断（路径 did-you-mean、命令未找到的 PATH 候选）也归该 crate，见下节。 |
| Workspace Bash 缺省后台（beta flag） | `peri-mcp-workspace`：`workspace.rs::WorkspaceMcpServer::standalone(cwd, default_run_in_background)`、`terminal.rs::BashTool::with_default_run_in_background`；证据 `terminal_background_test.rs`、`workspace_tasks_wire_test.rs` | schema 的 `run_in_background.default` 与 `call_owned_bash` 的判定读**同一字段** | 有效缺省由宿主会话装配从**冻结**的 `full-async-tools`（`peri-acp-types::beta_flags::FULL_ASYNC_TOOLS`）派生后经显式构造参数注入（`peri-acp` → `BuiltinInstanceContext::workspace_bash_default_run_in_background` → dispatch）；显式 `false` 仍前台；缺省 true 且无 task_manager 时维持既有报错（与 Agent 工具有意不对称）。`WorkspaceInstanceInput` 不带该值 |
| Workspace 资源面 | `peri-mcp-workspace`：`mcp-packages/workspace/src/resources/mod.rs` | `resources/{mod,skills,agents,instructions,builtin,scan,path,frontmatter}.rs`；URI/`_meta`/DTO 契约在 `peri-acp-types/src/workspace_resources.rs`，skills 扩展键在 `peri-acp-types/src/skills.rs` | skills / agents / 项目指令的扫描与只读提供（`resources/list|read|templates/list`、`skills/list|get`）。`WorkspaceResourcesInput` 构造期注入：skill 根为 User/Project/Plugin（带 `plugin_name`），builtin 静态资产唯一副本在 provider，由 core `snapshot.resources().disable_bundled_skills` 经资源输入控制，正常 consumer 不重读全局关闭位；`skillsDir` 全局根装配已删除。会话装配也注入 Agent 项目/插件根、指令与 meta 面；未装 provider 不声明 skills 扩展。未知 URI `-32602`、不存在 `-32002`；不注册技能工具，宿主 `SkillTool`/`DiscoverSkillsTool` 仍聚合来源。 |

## 宿主与插件边界

Workspace 后台 Bash 当前运行时入口：`workspace/src/shell_tasks.rs` 持有任务状态并通过 `workspace.rs` 实现 `tasks/get|cancel`、`subscriptions/listen` 的 `taskIds` 通知；独立 CLI 与 builtin dispatch 都构造 `WorkspaceMcpServer::standalone`。Peri 的客户端能力声明在 `peri-middlewares/src/mcp/client/service.rs`，工具回执与订阅消费在 `tool_bridge.rs`、`client/subscription.rs`。ACP 会话装配不再向 Workspace 注入 TaskManager/完成回调。`WorkspaceInstanceInput` 只保留在直接构造器供旧工具测试使用，尚非编译依赖清理。

会话任务 scope 的资源 owner 入口在 `peri-mcp-core/src/task_scope.rs` 与 `workspace/src/{shell_tasks,workspace}.rs`：宿主创建 `TaskScopeAuthority`，调用 `issue(session_id)` 签发 capability；`WorkspaceMcpServer::with_task_scope_authority` 按请求解析 `_meta["peri/taskScope"]`，隔离 session 身份，不接受模型自造身份。capability 不携带 Store execution owner token 或 epoch/nonce，已无 `workspace/taskFence` 执行代际协议；Peri 不提供跨重启执行接管，SDK alpha 的协调机制不在本次支持范围。`workspace/taskSnapshot` 返回 scoped 任务、cursor、epoch 与 closing 状态，`workspace/taskChanges` 从 cursor 取变更并可有界等待。`workspace/taskClose(epoch)` 关闭准入，待在途创建登记完成后返回 barrier cursor；`workspace/taskOpen(epoch)` 仅在全部任务终态且无在途创建时推进 scope epoch 并恢复准入，旧 scope epoch 的 close/open 请求被拒绝。Workspace 资源 owner 在请求取消后仍持有创建 admission 直到任务登记，防止 close barrier 漏掉已发起的 shell。终态 transition ID 由 owner 固定；`tasks/cancel` 只置 cancellation requested，executor 清理完成才发布 Cancelled。独立 CLI 经部署可信的 MCP 连接接收 scope 元数据；已删除 `OwnerIncarnationGuard`、owner marker 写入及遗留 marker 启动阻断，不以本机 marker 判定接管资格；任务排空仍由资源 owner 负责。无认证独立 HTTP CLI 只绑定 loopback，跨主机依赖部署受信传输。未注入 authority 的 standalone 仍属 legacy 非 scoped 模式，不具备按 session 冷恢复。目标语义见 [Session 异步任务架构](../design/session-async-tasks.md)。

- `peri-middlewares/src/mcp/builtin/mod.rs` 持有 builtin 配置 overlay、实例关闭策略与名称映射；`dispatch.rs` 构造新 crate 导出的 handler。
- `context.rs` 持有注入给 server 的宿主上下文，`runtime.rs` 持有内存 transport、server task 与有界关闭；Cron tick 监督也在这里。
- `peri-acp-types` 持有 builtin 实例与工具声明、名称及 direct/deferred 策略。插件 crate 不复制这些策略表。
- 各插件 crate 内测试覆盖工具行为、handler 路由和 RMCP wire；host transport、dispatch、bridge、policy、tick 与 pool lifecycle 测试留在对应宿主 crate。
- 失败恢复指引由工具在失败点生成（`ToolFailure { recovery, detail }`），宿主不做事后文本匹配：路径候选在 `workspace/src/filesystem/path_hints.rs`（Read / Edit / Glob / Grep / folder_operations 的"目标不存在"分支；Edit 的 `old_string not found` 文本失败除外），命令候选在 `workspace/src/shell_hints.rs`（exit 127 + command-not-found 文案，或 exit 1 + PowerShell `FullyQualifiedErrorId: CommandNotFoundException`），两者共用 `workspace/src/fuzzy.rs` 的 Skim 排序。原 `peri-agent` / `peri-middlewares` 的 error_suggest 框架已删除。

## 验证

文件工具测试位于 `workspace/src/filesystem/{read,edit,write,mod}_test.rs`：Read/Edit 的 Unix 权限失败保留实际诊断与原文件；Write 的提交失败不得创建目标目录；路径解析 fixture 使用独立临时目录。行为回归：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-mcp-workspace --lib -- filesystem::`。Unix 权限用例需要非特权执行用户，不以非 Unix 空分支冒充通过。

```bash
cargo test -p peri-mcp-config --lib
cargo test -p peri-mcp-web --lib
cargo test -p peri-mcp-artifact --lib
cargo test -p peri-mcp-cron --lib
cargo test -p peri-mcp-workspace --lib
cargo test -p peri-mcp-workspace --lib -- git_watch
cargo test -p peri-mcp-workspace --lib -- resources
cargo test -p peri-middlewares --lib -- mcp::builtin::dispatch
cargo check -p peri-mcp-common
cargo clippy -p peri-mcp-common -p peri-mcp-web -p peri-mcp-artifact -p peri-mcp-cron -p peri-mcp-workspace --all-targets -- -D warnings
```

Agent 定义与任务 scope 的规则及测试归 `peri-mcp-core`，`peri-mcp-common` 只 re-export；运行 `cargo test -p peri-mcp-core --lib`。`peri-mcp-common` 的 shell/输出契约测试仍运行 `cargo test -p peri-mcp-common --lib`。宿主 runtime/dispatch 回归用 `cargo test -p peri-middlewares --lib -- <module_filter>`；筛选器应指向宿主测试模块，不再指向已迁入 package 的 handler 或工具模块。当前模块名见 `peri-middlewares/src/mcp/mod.rs`。
