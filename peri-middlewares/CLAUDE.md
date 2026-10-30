# peri-middlewares

## Scope

`peri-middlewares` 承载提示词、插件、审批、SubAgent 和 MCP 宿主生命周期。链序蓝本在 `../peri-agent/src/session/factory.rs::production_blueprint`，装配在 `src/assembly.rs`。builtin handler/tool 位于 `../mcp-packages/`；本 crate 的 `src/mcp/builtin/` 保留策略、分派和运行时。

自动 compact 属于 Agent 执行阶段，参见 `../peri-agent/CLAUDE.md`。

## 数据流/架构

`SessionContext/config → Agent session factory（唯一触发点与链序蓝本）→ assembly 槽位构造 → MiddlewareChain → prompt/tools → Agent stage`。

- 插件提供 skill roots、agent dirs、hook groups 与 MCP 配置，由对应中间件消费。
- MCP 配置按数据面全局路径（默认 `~/.peri/settings.json`，尊重 `--config-file`）、插件、项目 `{cwd}/.mcp.json` 合并；配置正文、保存与来源身份经独立 `peri-mcp-config` MCP 通道，不依赖尚未启动的工具池。工具与资源仅在 pool 可用时注册。
- 宿主或全局配置可用 `mcpServers.workspace: { "url": "https://…/mcp" }` 接管内置 Workspace；项目/插件配置不可把同名远端标记为受信 Workspace。此远端的 `tools/list` 是 direct 工具全集的事实源（空列表有效），资源面也从该连接读取；`WorkspaceMiddleware: false` 只关闭内置实现，不关闭显式远端替代。连接协议由 MCP client 自动协商，不声明 `protocolVersion` 或 `system_mcp_tools`。
- 图片附件及归因的文件正文经当前会话可见的 builtin workspace MCP 读取；能力关闭、断连或换代后不回落宿主文件系统。hook loader 的正文与 canonical 来源去重经配置数据面，保留 symlink 同文件判定；身份不可得时跳过项目来源，不假定为独立来源。
- Skills 的**来源**（用户目录、项目目录、插件根、内置静态资产）由 builtin `workspace` 实例的资源面（`skills/list` + `resources/read`）提供；宿主只做根解析适配器（`src/skills/loader.rs::resolve_skill_roots`：路径 + scope/标签，不检查目录存在性），`src/settings.rs` 只读取 `disableBundledSkills`；`skillsDir` 配置链路已删除。扫描语义（叶子、深度/目录预算、symlink 口径、同名先到先得）在 provider（`mcp-packages/workspace/src/resources/skills.rs`）。
- SubAgent 从父工具、冻结上下文、取消策略与事件处理器派生执行上下文；具体 agent 定义和内置 agent 请直接查 `src/subagent/` 与项目 `.claude/agents/`，如需举例只使用 `explorer`。
- SubAgent 在父 Reason 目录发布时绑定当前 MCP 工具快照，避免装配早于 MCP 就绪而漏工具；子链收集自身工具并经 ToolSearch 发现、执行 deferred 工具。搜索与执行只消费子 Agent 过滤后的目录，`tools: []` 不得获得元工具。

## 任务路由

| 任务 | 首选位置 |
| --- | --- |
| 生产链顺序、条件注册、跨 crate 装配 | `../peri-agent/src/session/factory.rs`（蓝本）与 `src/assembly.rs`（槽位构造） |
| hook 状态能力与回写（ARC-MIDDLEWARE-CAPABILITY-001） | `../docs/standards/architecture-contracts.md`；`../peri-agent/src/middleware/capabilities.rs` |
| MCP 合并、server/tool bridge | `src/mcp/` |
| Plugin manifest、commands、agents、MCP 回退 | `src/plugin/` |
| Hook 事件与执行器 | `src/hooks/` |
| Skills 根解析、registry 投影、预载、工具（零 FS） | `src/skills/`（`loader.rs` 根解析 / `mod.rs` 投影与摘要 / `tools.rs` 两工具）、`src/settings.rs`、`src/subagent/skill_preload.rs` |
| SubAgent、后台任务、取消和事件 | `src/subagent/`；Agent 定义类型与纯解析在 `../peri-mcp-core/src/agent_definition/`，本地扫描在 workspace MCP 资源面 |
| HITL 权限与审批 | `src/hitl/` |
| Workflow、工具搜索 | `../peri-workflow/`、`src/tool_search/` |
| Builtin MCP handler、工具与公共映射 | `../mcp-packages/{common,web,artifact,cron,workspace,workflow}/`；宿主分派与生命周期在 `src/mcp/builtin/{context,dispatch,runtime}.rs` |
| 文件 / 终端工具与 Cron 调度 | `../mcp-packages/workspace/src/{filesystem,terminal}/`、`../mcp-packages/cron/`；Todo 仍在 `src/middleware/todo.rs` |

## 稳定不变量

- **链顺序**：只能在 Agent 层 session 工厂的链序蓝本（`production_blueprint`）与 `src/assembly.rs` 槽位构造中判断与修改生产顺序；不得按名称或局部便利重排。
- **MCP**：保留三层合并、内容去重和插件命名空间；配置来源或工具注册变更必须同时检查 pool、资源与 bridge 路径。init/OAuth/reconnect/subscription 任务由 deployment-held non-Clone `McpTaskOwner` 持有，并实现契约层 `McpTaskOwnerPort` 供 ACP boxed 注入；pool 只持 weak spawner。正常关闭顺序固定为 pool begin-close → owner abort/join → pool service close。Pool service close 由 pool-held 单一 transaction 持有，waiter 取消/并发/重试必须观察同一 `McpPoolShutdownReport`；cleanup timeout 保持 `Closing`，不得发布 `Closed`（ARC-HOST-SHUTDOWN-001）。
- **MCP 运行控制**：ACP 经 `McpPoolPort` 注册、重启、关闭普通 stdio/HTTP server，复用当前环境 pool 与唯一任务 owner；连接代次、调用 gate、清理事务和目录来源均由 pool 持有。关闭先撤销旧 handle 新调用，再排空资源；失败保留 owner 供重试，不另建动态覆盖 registry 或 Agent 工具。wire 与受保护来源边界见 `../docs/design/acp-mcp-control.md`。
- **当前调用与任务**：从可信 `ToolContext` 生成 `peri.invocation` v2 metadata，区分 invocation ID 与模型 tool-call ID；不要求持久执行身份，不读取 Work 或恢复历史任务。已登记当前任务的监控、通知、结果投递、取消及 pool 关闭继续有效；关闭时查询资源 scope 仅用于释放资源，不把历史 task 装配为活跃任务。
- **SubAgent 能力标签与只读角色**：目录 `readonly/writes` 必须与 `filter_tools` 的声明语义一致——`tools` 省略或独占 `*` 继承父工具，恒标 `writes`（继承面含 MCP 与 `ExecuteExtraTool`）；只有显式只读白名单或 `tools: []` 才能标 `readonly`。builtin `explorer` 用 `tools: [Read, Grep, Glob, ReadContextArchive]` 做工具级只读；`plan` 保留 `.peri/plans/` 写入例外，仍继承父工具并由 prompt 约束，不得把它误标为工具级只读。白名单被父工具集裁成空集时只记 warn，不改变裁剪语义。
- **MCP 调用恢复**：仅 workspace builtin 保留原生期限，其他 builtin 与外部 MCP 发送/响应共用 120 秒。超时或 drop 发有界取消，handler 监听 request token；Workspace Bash 的后台完成证据归 Workspace MCP Task owner，Peri 通过 `tasks/get` 与 `subscriptions/listen` 消费。仅投影安全原因及工具生成的任务、日志、草稿引用，不透传任意错误。宿主入口见 `src/mcp/`，workspace 工具见 `../mcp-packages/workspace/`。blocking 搜索只协作取消，不承诺 drop 时已 join。
- **MCP over ACP**：client 在会话 setup 声明的 `type: "acp"` server 由 `src/mcp/acp/`（`AcpMcpService`）承载。`mcp/message` 是请求作用域的：每次内层 MCP 调用单独发出（`serverId` + `requestId`），client 只反向下发通知，协议层无 `mcp/connect` / `mcp/disconnect`。`attach` 先通过连接级共享注册表校验归属（跨会话重名返回 CODE_UNAVAILABLE）再登记并后台发现（会话建立不等连接），失败留在池状态面（`ConfigSource::Acp` 条目）而不回抛；声明按声明它的会话归属，工具桥接、发现面与状态面必须按 `is_visible_to_session` 过滤，不得跨会话泄漏；内层错误码原样透传（lifecycle 依赖方法级错误码）；会话结束在池关闭前 `close_session`（幂等）（ARC-MCP-ACP-001）。
- **System MCP 启动准入**：`system_mcp=true` 的 server 须在首个 Reason 前完成 transport、initialize、能力协商与真实 `tools/list`（空数组成功，Err 不是发现证据），由 `before_react_start` 闸门阻断未就绪 loop。失败/timeout 返回类型化错误，不发布 ready；取消按中断分类。`system_mcp_tools` 按所属 server 原始工具名精确匹配；仅选中项 direct，未选中项维持原发现路径，`[]` 仅要求 ready。选中项以原始工具名进入模型面，普通 MCP 与 deferred 工具仍使用 `mcp__<server>__<tool>`。模型名冲突按确定准入顺序 first-wins，记录 warning 并跳过后续项；真实必需工具缺失仍失败。权限使用绑定来源身份，不凭裸名授予 builtin 权限。readiness 不绕过 Permission/HITL/事件/cancel。builtin 同构：web/artifact/workspace 选中各自 direct 集，cron 零提升。默认层注入，加载期拒绝 `disabled + system_mcp`。
- **插件 MCP 配置严格路径**：`load_enabled_plugins_for_mcp` 对非法 MCP 配置直接失败、不降级为空配置；宽容展示 API（`load_enabled_plugins_aggregated` 等）行为保持不变。
- **Plugin manifest**：`commands` 条目兼容字符串路径与对象；字符串是相对插件根目录的路径。agents 未声明时仍保留约定目录回退。不要把路径条目当作名称。
- **Skills（J5）**：宿主不做任何技能目录扫描或正文读取——目录来自会话级 `McpSkillRegistry` 投影（`before_agent`），正文只经统一 activation（`resources/read` + digest/frontmatter）。系统来源按连接身份识别（builtin 与受信 WorkspaceRemote），仅投影 `core:{skill}` 裸名命令；外部 MCP 仅投影 `{server}:{skill}`。`SkillsMiddleware` 关闭位由同一份 `disabled_middlewares` 派生，关闭后撤下系统命令并跳过主 Agent 的 slash token 自动预载；旧 `/server:skill` 系统 token 不再自动激活，子代理/workflow 的显式名单仍按原解析语义。根优先级/递归边界/叶子语义/同名覆盖是 **provider** 的契约；宿主只解析根列表（`resolve_skill_roots`）并与配置位一起作为 `WorkspaceResourcesInput` 注入。缺失即缺口（warn/空），**不回落磁盘**。
- **SubAgent**：同一会话的子 Agent 复用冻结指引、skills 与 prompt；同步子任务继承父取消，独立后台任务自管取消。`Agent(resume_thread_id, prompt)` 优先把非空 Info 投递给 live 执行（`action: send / status: queued`）；无接收者但磁盘仍 active 时拒绝，非 active 才 resume。Info 不打断模型，queued 不代表已读；事件按 `source_agent_id` 归属，改动需覆盖完成与取消路径。
- **HITL**：审批以解析后的 effective tool name 为准，包装、搜索或代理工具不得绕过审批；权限模式与 broker 的选择必须保持一致。
- **工具可见性**：direct/deferred 语义由工具声明和工具搜索路径共同保证，包装层不得改变其可见性。

## 目标命令

从仓库根目录执行：

```bash
./scripts/cargo-rmcp-patched.sh build --locked -p peri-middlewares
./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib
./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- mcp::task_scope
./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- mcp::acp
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib
```

## 按需引用 / Verify

- 链、工具注册与条件中间件：`../peri-agent/src/session/factory.rs` 与 `src/assembly.rs`；同时遵守 `../docs/standards/architecture-contracts.md` 的 `ARC-MIDDLEWARE-001`、`ARC-TOOLS-001`、`ARC-FROZEN-001`。
- Plugin/MCP 或 Skills 改动：阅读目标模块的实现与测试后运行对应 package 与 host 测试；workspace host filters 为 `mcp::workspace_builtin_tests`、`mcp::workspace_recovery_tests`，Cron host 生命周期为 `mcp::builtin_cron_runtime_tests`。
- System MCP 准入 / 工具注入改动：`cargo test -p peri-middlewares --lib -- mcp::system_tools`、`-- mcp::client::readiness`、`-- mcp::middleware`；契约测试 `cargo test -p peri-middlewares --test mcp_host_policy_contract -- --test-threads=1` 与 `--test mcp_isolation_contract -- --test-threads=1`。
- SubAgent 或 HITL 改动：覆盖冻结数据、取消、事件归属及 effective tool name 的相关测试。
- 所有修改完成后运行 `git diff --check`；不得在日志、错误或测试 fixture 中写入密钥、token、密码或连接串。
