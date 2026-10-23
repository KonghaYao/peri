# MCP package 代码索引

Builtin MCP 的工具与 server handler 由独立 crate 持有；配置来源 I/O 由独立 `peri-mcp-config` 数据面持有，session MCP 实例注册、配置投影、transport、宿主 context、policy 与生命周期仍由 `peri-middlewares` 装配。每个 crate 的依赖方向面向 `peri-mcp-common`、`peri-agent` / `peri-acp-types` / `peri-resources` 等稳定能力接口，不依赖 `peri-middlewares`。

Workspace 后台任务的资源结算以 `workspace/src/shell_tasks.rs` 为准：owner 在进程退出、管道回收和终态通知完成后结算；会话释放只取消 TaskManager 已知 external task 并等待 settled。没有 scope close barrier、旧 owner 扫描、连接重建或跨进程恢复；generic 模式无 owner，不参与 session 隔离。

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

Workspace Shell Tasks 的归属只有一个身份：`_meta["peri/taskOwner"]`，值 = 直接发起会话的
session id。Peri 宿主只为 builtin workspace 与 `WorkspaceRemote` 注入该字段；通用 MCP 客户端
不发送任何 `peri/*` 元数据时走 generic 模式，7 个基础工具照常可用，后台任务不做 session 隔离。
owner 只是身份，不是秘密或 capability：没有 token 表、签发/解析或调用一致性校验，远程部署
依赖连接级认证。wire 契约见 `mcp-packages/workspace/src/workspace_tasks_wire_test.rs`；owner
仅在当前进程内存在，不跨连接重建、不随进程重启恢复，也不扫描旧任务恢复归属。
插件生命周期、hook 格式、OS 执行环境与存储 locator/credentials 仍按专属边界维护。

| 能力 | crate 与入口 | 主要实现 | 说明 |
| --- | --- | --- | --- |
| 配置数据面 | `peri-mcp-config`：`mcp-packages/config/src/{lib,client,server}.rs`；契约 `peri-acp-types/src/configuration.rs` | `ConfigurationClient`、`ConfigurationMcpServer`、`config/execute`、`write_text_if_unchanged` | 独立 bootstrap byte/env/path I/O 与字节 CAS，合作写者共享跨进程锁；不决定 typed 规则或发布 snapshot。默认 duplex，可首次访问前选 TCP provider，不新增 daemon/model 工具，不回落本机；core 权威见 [peri-config](peri-config.md) |
| MCP 通用映射 | `peri-mcp-common`：`mcp-packages/common/src/lib.rs` | `server_info`、`rmcp_tool_from_base`、`list_tools_of`、`invoke_tool_call`、`parse_optional_u64`；`failure.rs`、`result_mapping.rs`、`process_env.rs` | 多个实例共用的 server metadata、schema 映射、IF-D14 结果映射、保留实际原因的失败类型和 process environment lock；不依赖宿主 middleware。 |
| 只读上下文归档服务 | `peri-mcp-common`：`mcp-packages/common/src/context_archive.rs` | `ContextArchiveMcpServer` | 仅注入会话绑定的 `ContextArchiveReadPort`，公开 ReadContextArchive 的 schema 与 tools/call；模型请求不含 session ID，不持有可写 transcript；客户端与生命周期位于 `peri-middlewares/src/mcp/context_archive.rs`，端到端回归 `context_archive_test.rs`。 |
| Agent 定义格式 | `peri-mcp-core`：`peri-mcp-core/src/agent_definition/` | `ClaudeAgent`、`ClaudeAgentFrontmatter`、`ToolsValue`、`parse_agent_file`、`frontmatter`（`split_frontmatter` / `frontmatter_json` / `frontmatter_summary`） | 本地与远端 MCP Agent 共用的纯数据与 Markdown/YAML 解析，不扫描目录、不读取文件、不签发授权；保留 omitted / explicit zero / allowlist 三态。`frontmatter` 纯函数同时供 workspace 技能/本地 agent 与 web 实例的 builtin agent 资源面使用。workspace 提供扫描与原始资源，宿主 registry 消费定义并按来源实施信任、批准和执行策略；不向计算核心添加 YAML 依赖。 |
| Workspace 宿主读取与回退 | `peri-mcp-workspace`：`mcp-packages/workspace/src/{image,file_observation,file_rewind}.rs` | custom request `image/read` / `workspace/readText` / `workspace/readMention` / `workspace/rewindFiles`，由 `workspace.rs::on_custom_request` 分派 | 图片附件、归因正文和 `@path` 内容由工具环境读取，关闭/不可得不回落宿主磁盘；`workspace/readMention` 按含端点行范围（起止相同仍读该行，回归见 `file_observation_test.rs::mention_equal_range_endpoints_returns_requested_line`）+ 行数上限 + 模型可见正文的 UTF-8 字节预算读取（按行跳读，不整文件进内存），截断返回 `truncated` 与可继续读取行号；项目指令 `@import` 展开共享累计读取预算，最终文本另有字节预算与有界截断说明，`scan_local` / `read_bounded_text` 区分「正常不存在」与读取/编码/超预算错误。Rewind 在 Workspace 端计算，owner 可选；存在 `_meta["peri/taskOwner"]` 时按 owner 准入，generic 客户端无元数据也可使用；ACP 只编排历史。 |
| Workspace 输出与分支 | `mcp-packages/workspace/src/{output_store,git_branch}.rs`；契约 `peri-acp-types/src/workspace_output.rs` | `output/store`、`workspace/gitBranch`；`resources/read` 的 `peri-output://` URI | 输出仅在工具环境持久化；URI 绑定实例，路径供同一环境的 Read 分页；不增加模型工具。宿主适配 `peri-middlewares/src/mcp/client/output_store.rs` 与 `workspace_io.rs` 门控会话可见性、关闭与换代，不可得不回落本机；git 超时与进程树清理归 provider。 |
| Web | `peri-mcp-web`：`mcp-packages/web/src/lib.rs` | `WebMcpServer`、`web_fetch.rs`、`web_search.rs`、`web_common.rs`、`agents.rs`、`agents/web-researcher.md` | 公共工具面为 `WebMcpServer`、`WebFetchTool`、`WebSearchTool`；工具描述位于 `src/descriptions/`；后端地址与可选 Bearer 凭据经 MCP 实例 env（`WebMcpServer::with_instance_env` → 各工具的 `from_instance_env`，变量名见 `docs/standards/environment-variables.md`）。资源面由 `agents.rs` 发布 `agent://builtin/web-researcher/agent.md`（scope = builtin、`io.peri/frontmatter` 投影），能力位声明 `resources`；定义随 web 实例可用性开闭，宿主 Agent registry 只信任宿主绑定的 builtin 实例来源。 |
| Artifact | `peri-mcp-artifact`：`mcp-packages/artifact/src/lib.rs` | `ArtifactMcpServer`、`ArtifactTool`、`ArtifactClient` | 上传协议与 Markdown 转 HTML 归该 crate；模板位于 `src/descriptions/md_to_html_template.html`。 |
| Cron | `peri-mcp-cron`：`mcp-packages/cron/src/lib.rs` | `CronMcpServer`、`scheduler.rs`、`tools.rs` | `CronScheduler`、`CronSchedulerPortHandle`、scheduler types 与任务管理工具由该 crate 导出。`cron://jobs` / `cron://triggers` 由标准资源读取；`cron_toggle` 与宿主专用 `cron_acknowledge` 走工具调用。宿主 tick task 的 spawn、join 与 reconnect 仍归 `peri-middlewares` runtime。 |
| Workflow | `peri-mcp-workflow`：`mcp-packages/workflow/src/{lib,server,resources,input}.rs` | `WorkflowMcpServer`、`WorkflowInstanceInput`、`resources.rs`（`RUNS_URI`/`TASKS_URI`/`read_payload`/`update_uri`） | 9 个工具（3 个模型工具，`WorkflowAgentResult`/`WorkflowTaskObserved`/`WorkflowAgentClaim`/`WorkflowConsumerReady`/`WorkflowCompletionAck`/`WorkflowClose` 为鉴权的宿主工具）与 6 个资源 URI（含 `workflow://background-tasks`） 归该 crate；行为委托 `peri-workflow` 的 `WorkflowRuntime`；宿主接线（池消费、`WorkflowTaskConsumer`、关闭集不 spawn）在 `peri-middlewares`/`peri-acp`，见各自索引 |
| Workspace | `peri-mcp-workspace`：`mcp-packages/workspace/src/lib.rs` | `WorkspaceMcpServer`、`workspace.rs`、`git_watch.rs`、`resources/`、`filesystem/`、`terminal.rs`、`fuzzy.rs`、`shell_hints.rs`、`filesystem/path_hints.rs`、`filesystem/glob_patterns.rs` | 文件、目录、搜索和 Bash 工具的 handler 与实现归该 crate；`git_watch.rs` 持有 `workspace://git/ref` 资源的采样状态机与正文（原宿主 `GitWatchMiddleware` 的逐字搬迁，v4 wave 4 下沉），订阅面（`list_resources` / `read_resource` / `accepted_subscription_filter` / `listen`）在 `workspace.rs`，只在成功的 `tools/call` 后采样且**无订阅者不采样**；输出持久化使用 `peri-mcp-common::shell`，纯截断复用 `peri-agent::agent::async_tasks`；common 的 `shell_executor.rs` 承载本地后台执行，`shell_output.rs` 承载两路 tee 输出，Agent 仅通过 `ShellExecutor` 注入端口消费。Grep 的 `glob` 与 Glob 的 `pattern` 共用 `filesystem/glob_patterns.rs` 的编译点：`{a,b}` 交替在那里展开，非法语法在遍历前报错（不再静默丢弃过滤器）。失败点的可行动诊断（路径 did-you-mean、glob 语法、命令未找到的 PATH 候选）也归该 crate，见下节。builtin Agent 静态表 `resources/builtin.rs::BUILTIN_AGENTS` 发布 coder / explorer / general-purpose / plan / verification；`web-researcher` 归 web 实例。 |
| Workspace Bash 缺省后台（beta flag） | `peri-mcp-workspace`：`workspace.rs::WorkspaceMcpServer::standalone(cwd, default_run_in_background)`、`terminal.rs::BashTool::with_default_run_in_background`；证据 `terminal_background_test.rs`、`workspace_tasks_wire_test.rs` | schema 的 `run_in_background.default` 与 `call_owned_bash` 的判定读**同一字段** | 有效缺省由宿主会话装配从**冻结**的 `full-async-tools`（`peri-acp-types::beta_flags::FULL_ASYNC_TOOLS`）派生后经显式构造参数注入（`peri-acp` → `BuiltinInstanceContext::workspace_bash_default_run_in_background` → dispatch）；显式 `false` 仍前台；缺省 true 且无 task_manager 时维持既有报错（与 Agent 工具有意不对称）。`WorkspaceInstanceInput` 不带该值 |
| Workspace 资源面 | `peri-mcp-workspace`：`mcp-packages/workspace/src/resources/mod.rs` | `resources/{mod,skills,agents,instructions,builtin,scan,path,frontmatter}.rs`；URI/`_meta`/DTO 契约在 `peri-acp-types/src/workspace_resources.rs`，skills 扩展键在 `peri-acp-types/src/skills.rs` | skills / agents / 项目指令的扫描与只读提供（`resources/list|read|templates/list`、`skills/list|get`）。`WorkspaceResourcesInput` 构造期注入：skill 根为 User/Project/Plugin（带 `plugin_name`），builtin 静态资产唯一副本在 provider，由 core `snapshot.resources().disable_bundled_skills` 经资源输入控制，正常 consumer 不重读全局关闭位；`skillsDir` 全局根装配已删除。会话装配也注入 Agent 项目/插件根、指令与 meta 面；未装 provider 不声明 skills 扩展。未知 URI `-32602`、不存在 `-32002`；不注册技能工具，宿主 `SkillTool`/`DiscoverSkillsTool` 仍聚合来源。 |

## 宿主与插件边界

Workspace 后台 Bash 当前运行时入口：`workspace/src/shell_tasks.rs` 持有任务状态并通过 `workspace.rs` 实现 `tasks/get|cancel`、`subscriptions/listen` 的 `taskIds` 通知；独立 CLI 与 builtin dispatch 都构造 `WorkspaceMcpServer::standalone`。Peri 的客户端能力声明在 `peri-middlewares/src/mcp/client/service.rs`，工具回执与订阅消费在 `tool_bridge.rs`、`client/subscription.rs`。ACP 会话装配不再向 Workspace 注入 TaskManager/完成回调。`WorkspaceInstanceInput` 只保留在直接构造器供旧工具测试使用，尚非编译依赖清理。后台 shell 启动失败在模型面只投影分类（launch worker panic/取消、注册表并发上限背压、未分类兜底），模型响应与该错误映射处的 tracing 均不输出底层 boxed error / JoinError 自由文本，tracing 保留稳定 failure_class；并发上限是独立背压，不因该投影改变。

会话任务 owner 的入口在 `peri-mcp-core/src/task_owner.rs`（`TASK_OWNER_META_KEY`、`task_owner_from_meta`）与 `workspace/src/{shell_tasks,workspace}.rs`：`WorkspaceMcpServer` 不持有 authority，每次请求从 `_meta["peri/taskOwner"]` 读取可选 owner；后台 Bash、`tasks/get|cancel` 与 `subscriptions/listen` 的任务事件在 owner 存在时按 owner 过滤，模型不能通过工具参数伪造身份。没有 owner 的 generic 模式不参与 session 隔离。已无 per-session token、`TaskScopeAuthority`、`validate_current_invocation` 或 `workspace/taskFence` 执行代际协议；也不再提供 `workspace/taskSnapshot|taskChanges|taskClose|taskOpen` 自定义生命周期（epoch、close barrier、连接重建恢复一并删除），旧客户端得到标准 method-not-found。终态 transition 由 owner 固定；`tasks/cancel` 只置 cancellation requested，executor 清理完成才发布 Cancelled。独立 CLI 缺省即通用 MCP 服务，不要求任何 `peri/*` 元数据，也已删除 `--trusted-task-scope`；显式 owner 只能经请求 `_meta` 传入。`workspace/rewindFiles` 保留非标准能力：客户端计算变更集，服务端校验并应用，owner 可选。会话释放只取消 TaskManager 已知 external task 并等待 settled，不做 snapshot/close barrier。无认证独立 HTTP CLI 只绑定 loopback，跨主机依赖部署受信传输。目标语义见 [Session 异步任务架构](../design/session-async-tasks.md)。

- `peri-middlewares/src/mcp/builtin/mod.rs` 持有 builtin 配置 overlay、实例关闭策略与名称映射；`dispatch.rs` 构造新 crate 导出的 handler。
- `context.rs` 持有注入给 server 的宿主上下文，`runtime.rs` 持有内存 transport、server task 与有界关闭；Cron tick 监督也在这里。
- `peri-acp-types` 持有 builtin 实例与工具声明、名称及 direct/deferred 策略。插件 crate 不复制这些策略表。
- 各插件 crate 内测试覆盖工具行为、handler 路由和 RMCP wire；host transport、dispatch、bridge、policy、tick 与 pool lifecycle 测试留在对应宿主 crate。
- builtin 派发 Skill（`resources/builtin/skills/{ultracode,multitask}/SKILL.md`）正文带能力预检：表里「谁能跑命令」是对 shipped Agent 定义（`workspace/.../builtin/agents/*.md` 与 `web/src/agents/*.md`）frontmatter 的声明，由 `resources/builtin_skill_capability_test.rs` 从两张 `BUILTIN_AGENTS` 静态表现算逐行核对（缺行、多行、取值非法、与定义冲突都判失败），并用 `plan` + 命令 brief 这类负例证明预检在派发前就给出缺口；`explorer` 用显式 `tools: [Read, Grep, Glob, ReadContextArchive]` 声明工具级只读（表中 runs commands=no），能力表与定义由同一 checker 锁定。provider 侧仍不做能力解释，`resources/agents.rs` 的边界不变。
- 失败恢复指引由工具在失败点生成（`ToolFailure { recovery, detail }`），宿主不做事后文本匹配：路径候选在 `workspace/src/filesystem/path_hints.rs`（Read / Edit / Glob / Grep / folder_operations 的"目标不存在"分支；Edit 的 `old_string not found` 文本失败除外），命令候选在 `workspace/src/shell_hints.rs`（exit 127 + command-not-found 文案，或 exit 1 + PowerShell `FullyQualifiedErrorId: CommandNotFoundException`），两者共用 `workspace/src/fuzzy.rs` 的 Skim 排序。原 `peri-agent` / `peri-middlewares` 的 error_suggest 框架已删除。
- Bash 完成路径的执行证据描述**整个进程组**，不是前台 shell：组内仍有成员时状态为 `running`（有 task_manager，已登记并带 `task_id`）或 `unknown`（无 task_manager，无法登记），两种情况都不声明退出码；只有组已结束才声明退出码。判断来自 `ShellExecutionGuard` 的进程组观测，不用 `command.contains('&')` 之类的命令文本启发式（`&&`、`2>&1` 不产生脱离进程）。回归在 `workspace/src/terminal_process_group_test.rs`。

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
