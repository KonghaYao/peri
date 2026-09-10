# peri-middlewares 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-10（MCP client / Dynamic registry 职责拆分与路径校准）
> 依据：peri-middlewares/CLAUDE.md、docs/standards/architecture-contracts.md、docs/design/{mcp-multiplexing,middleware-system,workflow}.md、docs/reference/mcp-ecosystem.md、源码

## 架构速览

- 数据流：`SessionContext/config → Agent 层 session 工厂（build_middleware_chain 唯一触发点 + production_blueprint 链序蓝本）→ assembly.rs 按槽位调用 assembly/ 私有构造模块 → MiddlewareChain → prompt_contribution + collect_tools → Agent stage`
- 链序事实源：`peri-agent/src/session/factory.rs:95` 的 `production_blueprint()`（`ChainSlot` 枚举 :27，槽位顺序 = 行为契约）；链装配实现 `peri-middlewares/src/assembly.rs` 的 `ProductionChainAssembler::assemble`（按蓝本逐槽位构造，disabled 条件在根判断，复杂构造与 Hook 组展开委托私有模块）
- 稳定不变量：链顺序只可在蓝本与装配实现中判断/修改（ARC-MIDDLEWARE-001）；`BaseTool::is_direct()` 是工具可见性事实源（ARC-TOOLS-001）；frozen 数据会话内不可漂移（ARC-FROZEN-001）；装配输入经 `peri-acp-types` 端口（McpPoolPort / ToolSearchPort / WorkflowMiddlewarePort / CronSchedulerPort 等）注入，装配时 downcast 还原具体实例
- 自动 compact 属 `peri-agent` 执行阶段，不在本 crate 路由范围（见 `docs/code-index/peri-agent.md`）

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改中间件链序 | 蓝本 `peri-agent/src/session/factory.rs`（`ChainSlot` :27、`production_blueprint` :95、`build_middleware_chain` :153）；装配 `peri-middlewares/src/assembly.rs` | `ProductionChainAssembler::assemble`（按 `blueprint: &[ChainSlot]` 唯一逐槽位 match，具体构造调用私有模块） | 顺序 = 行为契约禁止重排；增删/重排必须先以 `production_blueprint()` 的完整槽位序列与装配实现为准（ARC-MIDDLEWARE-001）；`MiddlewareChainAssembler` trait 在 factory.rs:139 |
| 改条件注册 / 关闭面 | `src/assembly.rs` + `src/assembly/{preparation,mcp,lsp,hooks}.rs` | 根 `disabled.contains(名)` 在构造前过滤；`build_parent_tools` 过滤继承工具；复杂槽位分别由 `add_mcp` / `add_lsp` / `add_hooks` 构造 | 蓝本只定槽位顺序；根逐槽位调用，Hook 每非空 group 展开一个实例；workflow agent 独立装配在 `assembly/workflow.rs` |
| 加新工具（direct/deferred） | trait 事实源 `peri-acp-types/src/tools.rs`；注册面 = 各 middleware `collect_tools()`（例 `src/middleware/filesystem.rs:39`）；LLM 可见面过滤在 Agent 层 `peri-agent/src/agent/stages/reason.rs:138` | `BaseTool::is_direct()`（默认 false = deferred）；包装层 `src/tools/mod.rs`（ArcToolWrapper :44 / BoxToolWrapper :50） | `is_direct()=true` 直接进 LLM tools；false 经 `SearchExtraTools` 发现 + `ExecuteExtraTool` 代理执行；direct 集合同时是 tool_search 声明段数据源；包装层须透传 is_direct；契约 ARC-TOOLS-001、ARC-SERIAL-001 |
| 改 PTC / `RunPtcCode` | `src/ptc/mod.rs` + `peri-js-runtime/src/{artifact,executor}.rs` + `npm-packages/@peri-ptc/`；canonical seam 在 `peri-agent/src/agent/stages/tool_dispatch.rs` | `RunPtcCode` invoke；`PtcMiddleware::before_agent` / `prompt_contribution`；`PtcRouter::route`；`EffectiveToolDispatcher::dispatch`；`ptc/start` | canonical `RunPtcCode` 为 deferred-only，经 `SearchExtraTools → ExecuteExtraTool` 执行；before_agent 从当前 session-local 视图生成安全语义/RPC catalog，主 bridge 在首个 Reason 的 ModelRequest 构造时读取。执行环境 ESM-only；artifact 由 `PtcArtifactProvider` 受控安装并缓存固定 `@peri-code/ptc@0.2.3`，Rust 不内嵌 artifact、不从仓库 `dist` 启动、Cargo 构建不要求 Bun；默认 fail closed，仅显式环境变量允许精确版本 `npx` fallback；工具错误仅投影 stable code + fixed safe message；policy/HITL/event/tool card 投影 effective target 并复用 timeout/cancel；assistant raw wrapper call 仅保留协议配对；契约 ARC-PTC-ARTIFACT-001 |
| 改 ToolSearch direct 能力声明 / 元工具名 | `src/tool_search/core_tools.rs` + `middleware.rs` + `search_tool.rs` + `execute_tool.rs` | `direct_tools_sorted_csv` / `direct_tools_description`；`ToolSearchMiddleware::rebind_catalog` / `before_reason_catalog`；`SEARCH_EXTRA_TOOLS_NAME` / `EXECUTE_EXTRA_TOOL_NAME` | 元工具 description、deferred index 与 Execute request-local resolver 在 Reason catalog refresh 后、before_model/pin 前重绑同一 session-local working map；`before_agent` 复用同一幂等 helper。静态工具名常量不能声明运行时能力；disabled/filter 后的工具不得继续显示，契约 ARC-TOOLS-001 |
| 改 deferred 搜索（索引 / 评分 / 声明） | `src/tool_search/tool_index.rs` + `keyword_search.rs` + `declaration.rs` | `ToolSearchIndex::build` / `search` / `get_tool` / `format_deferred_list`；`keyword_score`；`collect_declarations` | 索引只收当前 turn 本地视图中的 `!is_direct()` 工具；查询按 camelCase / MCP 前缀切分；v1/测试兼容路径才回退 shared tools；声明段驱动提示词层 |
| 改 deferred 执行（ExecuteExtraTool） | `src/tool_search/execute_tool.rs` | `ExecuteExtraTool`；`ExecuteExtraToolResolver::resolve`；`parse_extra_tool_call` / `resolve_effective_tool_name` | 从 `tool_name`/`params` 字段解包；v2 目标解析绑定当前 turn session-local 工具视图，只有 v1/测试兼容路径回退 shared tools；非 ExecuteExtraTool 调用直通不包装；Resolver 由装配层注入 |
| 改 ToolSearch 中间件注入 | `src/tool_search/middleware.rs` + `peri-agent/src/agent/stages/reason.rs` + `peri-agent/src/agent/model_bridge.rs` | `ToolSearchMiddleware::rebind_catalog` / `before_agent` / `before_reason_catalog` / `prompt_contribution`；`run_reason`；`AgentModelBridge::build_request` | 优先读 v2 每 turn 本地工具视图（`state.local_tools()`），无则回退 shared_tools（v1/测试路径）；Reason refresh 后经专用 hook 幂等重绑 Search/Execute，随后 before_model、pin 与 ModelRequest 读取同一 contribution；不重跑全部 before_agent，fresh stage 不复用旧 middleware cache |
| 改 artifacts 上传工具 | `src/artifact/` | `ArtifactMiddleware`（`mod.rs`）；`ArtifactTool`（`tool.rs`）；`ArtifactClient`（`client.rs`） | 独立 middleware 注册 direct `artifact` 工具；可由 MetaHarness 的 `ArtifactMiddleware: false` 单独关闭而不影响 ToolSearch；上传地址来自 env 或默认值；结果格式化统一经 `ArtifactClient::format_output` |
| 改 MCP 连接 / 初始化 / 生命周期 | `src/mcp/client.rs`（pool 状态所有权与端口）+ `src/mcp/client/lifecycle.rs`（准入、连接提交与关闭）+ `src/mcp/task_scope.rs` + `src/mcp/initialize.rs` + `src/mcp/reconnect.rs` + `src/mcp/client/subscription.rs` + `src/mcp/client_oauth.rs`；owner contract `peri-acp-types/src/ports.rs` | `McpClientPool::{begin_shutdown,shutdown,spawn_background,try_commit_connection}`（lifecycle.rs:131/:179/:144/:163）；`spawn_reconnect`（reconnect.rs）；`McpTaskOwner` / `McpTaskOwnerPort`；`run_initialize`；`reconnect` | init/OAuth/reconnect/subscription 的 handle 只在 deployment-held non-Clone concrete owner，ACP 仅持 boxed owner port，pool 只持 weak spawner；pool lifecycle gate 线性化任务准入与 service/client commit。关闭顺序固定为 pool begin-close → owner abort/join → pool service close；service close 是 pool-held 单一 transaction，waiter 取消/并发/重试观察同一 report，超时保持 Closing。callback/notifier 使用 weak pool capture。连接超时 STDIO 10s / HTTP 30s / shutdown 5s；契约 ARC-HOST-SHUTDOWN-001 |
| 改 MCP 状态通知 / 缓冲 | `src/mcp/middleware.rs` + `src/mcp/client/status.rs`（`record_status_change` :257）；缓冲所有权 `src/mcp/client.rs` 的 `pending_changes` | `McpMiddleware::first_turn_reminder`（middleware.rs:335，连接概览注入）；`before_model`（:364，drain pending_changes 成 Info 消息）；`push_status_changes`（:281） | 运行中变化进全局缓冲（任一会话消费一次即清空）；初始化中（initialized=false）的状态写入不通知，避免与首 turn 概览重复 |
| 改 MCP 缓存准入 / RPC 包装 / 失效 | `src/mcp/client/cache.rs`；磁盘缓存实现 `src/mcp/resource_cache.rs` | `persistent_cache_allowed_for`（:41）；`read_resource_cached`（:72）；`list_all_tools_cached`（:258） | dynamic connection 暂不读写持久化缓存；静态连接按认证策略和 server cache version 判定复用，资源内容验证成功后才持久化 |
| 改 MCP 配置合并 | `src/mcp/config.rs` | `load_merged_config_full`（:223）→ `load_merged_config`（:349）；`load_global_config`（:60）；`load_from_path`（:45）；`remove_server_from_config`（:385）/`set_server_disabled`（:473） | 三层合并：global `~/.peri/settings.json` → 插件（`plugin:{name}:{server}` 命名空间 + 与手动配置内容 hash 去重，:304-319）→ 项目 `{cwd}/.mcp.json`（后插覆盖）；插件 env 按插件独立上下文在合并前展开（CLAUDE_PLUGIN_ROOT / CLAUDE_PLUGIN_DATA） |
| 改 DynamicMCP 工具契约 | `src/mcp/dynamic/tool.rs`；DTO 事实源 `peri-acp-types/src/dynamic_mcp.rs` | `DynamicMcpTool::parameters`；`DynamicMcpAction::from_tool_input` / `canonicalize` | 工具 schema、解析与 canonical 校验共同定义 session-scoped MCP 的 load/status/unload 输入契约 |
| 改 Dynamic MCP load / unload / session close | `src/mcp/dynamic/registry.rs`（RegistryState 单一 owner、deployment port）；`registry/{connector,load,unload,operations,lifecycle}.rs` | `load`（load.rs:12）/`run_load`（:139）；`unload`（unload.rs:13）；`close_session_impl`（lifecycle.rs:28）；`notify_operation`（operations.rs:83） | load/unload 保留 scoped instance 与 operation identity；后台任务沿用 weak registry 和 start gate，session close 先撤销再 drain/close，超时保留未完成实例 |
| 改 Dynamic MCP capability / collision / projection | `src/mcp/dynamic/registry/capability.rs`；稳定导出路径 `registry::CheckedSessionMcpProjection` | `tools_collide`（:64）；`publish_capability`（:102）/`revoke_capability`（:156）；`CheckedSessionMcpProjection`（:194）；catalog 入口 `registry.rs::register_catalog`（:112） | collision baseline 保持 first-write-wins；load commit、capability snapshot 与 projection 刷新在同一 RegistryState 锁内发布，撤销按精确 incarnation 判定；失效 lease 恢复静态 handles |
| 改 MCP 工具 / 资源注册 | `src/mcp/tool_bridge.rs` + `resource_tool.rs` + `discover_tool.rs` | `build_tool_bridges`（tool_bridge.rs:207，pool → Vec<Box<dyn BaseTool>>）；`McpToolBridge`（:31，`new` :57）；`McpResourceTool`（resource_tool.rs:45）；`DiscoverMCPTool`（discover_tool.rs:32） | 工具/资源仅在 pool 可用时注册（`assembly/mcp.rs::add_mcp` 条件分支）；`McpMiddleware::collect_tools`（middleware.rs:311）提供 Discover + Resource；注册变更必须同时检查 pool、资源与 bridge 路径 |
| 改 MCP skill 发现 | `src/mcp/skill_discovery.rs` + `skill_discovery/{skills_list,legacy_scan,verify}.rs` | `run_discovery`（skill_discovery.rs:97，规范 skills/list 与 legacy 扫描分流）；`mcp_route_entries`（:346）；`finish_command_source`（:166）；`refresh_entry_and_content`（skills_list.rs:327）；`is_skill_scheme`（legacy_scan.rs:18）；`verify_and_build`（skills_list.rs:499） | 经 `McpSkillRegistry`（peri-acp-types）注册；`skill://` scheme 资源拉取 + digest 校验后入注册表；命令面 `McpSkillPlaceholder` 占位已由 `McpSkillReleaser`（skill_discovery.rs:264，放行跳板：交互式 Inject 原文 / RPC 直返全文）替代 |
| 改 MCP Agent 发现 / 激活 | `src/mcp/agent_registry.rs` + `src/subagent/tool/define.rs` + `src/assembly.rs` | `McpAgentRegistry::entries` / `activate`；`SubAgentTool::load_and_approve_mcp_agent` | 从 `resources/list` 快照只发现 `agent://.../agent.md` 元数据；`Agent(subagent_type="mcp__<origin>__<name>")` 激活时才 read/校验/digest/批准，远端危险本地字段默认忽略，执行复用父工具交集与现有 subagent runtime，不落盘、不覆盖本地定义 |
| 改 MCP 多路复用 / Apps relay | `src/mcp/{channel_handler,apps,apps_relay}.rs` + ACP host 装配 | `ChannelHandler::new`；`PoolMcpAppsRelay`；Apps deployment profile 与 binding lease registry | Channel broker 只参与 Approval，AskUser 使用原始 broker；`PERI_MCP_APPS` 启用 stdio relay，App `tools/call` 必须经 connection-owned lease 和 canonical Permission/HITL dispatcher；现行契约见 `docs/design/mcp-multiplexing.md` |
| 改 MCP OAuth / 凭证 | `src/mcp/client/oauth.rs`（flow 准入与回调）+ `src/mcp/oauth_flow.rs` + `auth_store.rs` + `callback_server.rs` + `client_oauth.rs`（授权执行与连接） | `OAuthCallbackServer::bind`（callback_server.rs:31）/`wait_for_code`（:43）/`parse_code_from_url`（:144）；`FileCredentialStore`（auth_store.rs）；OAuth 流程 `spawn_oauth_flow`（client_oauth.rs:25）/ `start_oauth_flow`（client_oauth.rs:63） | 每个 scoped connection 最多一个活跃 flow（`reserve_oauth_flow_scoped`，client/oauth.rs:266）；授权码经 ACP RPC → `register_oauth_callback`（:46）/`deliver_dynamic_oauth_callback`（:130）按完整 identity 投递，回调表仍由 pool 持有；token 只落本机权限保护文件，跨进程文件锁内 read-modify-write，并经同目录唯一临时文件原子替换（ARC-SECRET-001） |
| 改 plugin manifest / 加载 | `src/plugin/loader.rs`；类型事实源 `peri-acp-types/src/plugin.rs`（`PluginManifest` :269，`src/plugin/types.rs` 仅 re-export） | `load_manifest`（loader.rs:77）；`load_plugins`（:491）；`load_enabled_plugins_aggregated`（:632）；`PluginCommandProvider`（:595，`new` :600） | manifest 字段类型以 peri-acp-types 为事实源（McpServerConfig :35、PluginCommand :175、PluginAgent :211、PluginLspServer :217、PluginManifest :269）；`PluginMiddleware`（middleware.rs:7）只持有 LoadedPlugin 列表 |
| 改 plugin 命令 / agents / MCP 回退 | `src/plugin/loader.rs` | `parse_command_md`（:66）；`plugin_route_entries`（:276）；`merge_plugin_mcp_servers`（:612）；`CommandFrontmatter`（:53） | `commands` 兼容字符串路径与对象（字符串 = 相对插件根路径，不是名称，勿当名称解析）；agents 未声明仍保留 `.claude/agents` 约定目录回退；插件 MCP 配置命名空间 `plugin:{name}:{server}` |
| 改 plugin 安装 / 市场 | `src/plugin/installer/` + `src/plugin/marketplace/` + `src/plugin/config.rs` | `install_plugin`（installer/install.rs:12）/`update_plugin`（:168）/`uninstall_plugin`（uninstall.rs:15）/`check_updates`（:109）/`cleanup_orphaned_plugins`（:150）；`MarketplaceManager`（marketplace/manager.rs:20，`init` :129 / `spawn_refresh` :199）；路径 `claude_home` / `installed_plugins_path` 等（config.rs:106-143） | 安装状态持久化 `installed_plugins.json`（load/save config.rs:175/:325）；启用名单在 `~/.claude/settings.json`（save/load :428/:465）；marketplace 缓存与刷新（manager.rs:57/:199） |
| 改 skills 扫描 / 优先级 | `src/skills/loader.rs` | `resolve_skill_roots`（:273）；`scan_skill_roots`（:87，SKILL.md 即叶子不再下钻）；`find_skill_content`（:332）；`list_skills`（:253）；`load_skill_metadata`（:54） | 根优先级 User(`~/.claude/skills`) → Global(`skillsDir`) → Project(`{cwd}/.claude/skills`) → Plugin → Builtin（`disable_bundled` 控制）；同名按来源顺序先者优先；符号链接防环；插件 skill root 经 `with_plugin_roots` 扩展点传入 |
| 改 skills 注入 / 工具 | `src/skills/mod.rs` + `src/skills/tools.rs` + `src/subagent/skill_preload.rs` | `SkillsMiddleware`（mod.rs:104，`build_frozen_summary` :267、`format_discovery_protocol` :137、`resolve_roots_static` :283）；`SkillTool`（tools.rs:29）/`DiscoverSkillsTool`（:114）；`extract_skill_names_from_text`（skill_preload.rs:21） | 渐进式摘要注入：会话开始冻结（`with_frozen_summary`，ARC-FROZEN-001）；SkillTool 走 `find_skill_content` 统一查找入口；预加载以 fake `SkillTool(skill_name)` ToolUse→ToolResult 序列 `add_message` 注入（放用户消息后，不碰 prompt cache 前缀）；MCP skill 经 `with_mcp_registry` 接入 |
| 改 Ultra-ADLC 编排契约 | `src/skills/builtin/skills/ultra-adlc/SKILL.md`；设计 `docs/design/ultra-adlc.md`；契约测试 `src/skills/builtin_test.rs` | builtin skill 的 `Logical Workflow 1`、`Main Agent decision seam`、`Progress reporting` | 默认由 fresh independent Opus Decision Arbiter 在 Workflow 1 内裁决，完整 packet 经两次 Opus 失败才升 Fable；裁决重试复用 Main Agent 以 SHA-256 验证的不可变 packet；只有枚举的意图/授权例外问用户；主管进度以四维完成率最小值计算，所有 ID 必须带语义内容 |
| 改 subagent 定义 / 扫描 | `src/subagent/mod.rs` + `src/subagent/built_in_agents.rs` | `scan_agents`（:326）/`scan_agents_with_extra_dirs`（:338）/`scan_agents_detailed`（:427）；`infer_agent_capability`（:395）；`SubAgentMiddlewareConfig`（:44，`for_fork` :64 / `for_agent_def` :77 / `with_frozen` :103） | agent 定义来自项目 `.claude/agents/` 与内置（built_in_agents.rs）；举例只用 `explorer`；capability 由 frontmatter 推断 |
| 改 subagent 工具 / 继承 / 取消 | `src/subagent/tool/define.rs` + `src/subagent/fork.rs` + `src/subagent/tool/mod.rs` | `SubAgentTool`（define.rs:38，`new` :78、`is_direct` :417、builder 链 `with_cancel` :110 / `with_frozen_data` :190 / `with_frozen_system_prompt` :203 / `with_task_manager` :148 / `with_bg_event_sender` :156）；`filter_tools`（fork.rs:22）；`build_subagent_middlewares`（tool/mod.rs:37）；`SubagentChainAssemblerImpl`（:133） | 父工具继承：tools 省略 = 全继承但恒排除 `Agent` 防递归；`tools: []` = 无；有值 = 白名单；再删 disallowedTools（大小写不敏感）；同步子任务继承父取消，独立后台任务自身 CancelPolicy；同一会话复用 frozen 数据 + 冻结 system prompt（ARC-FROZEN-001）；事件按 source_agent_id 归属 |
| 改 HITL 提问通道（AskUser） | `src/hitl/mod.rs` + `src/ask_user/mod.rs` | `HumanInTheLoopMiddleware`（hitl/mod.rs:39，`tool_names` :45、`new` :74、`collect_tools` :90）；`parse_ask_user`（ask_user/mod.rs:31）；`ask_user_tool_definition`（:65） | 2026-08-15 职责拆分：本中间件只做提问通道（AskUserQuestion 工具 + 12_ask_user 段落），审批归 PermissionMiddleware；broker 恒 None 时不装配（workflow agent 无提问，`assembly/workflow.rs::build_middlewares`）；装配须用原始 broker（见多路复用行） |
| 改 permission 审批主链路 | `src/permission/mod.rs` + `shared_mode.rs` | `PermissionMiddleware`（mod.rs:178，`new` :221、`disabled` :235、`with_shared_mode` :246、`process_batch` :290 批量审批）；`with_broker_timeout`（:262） | 审批以解析后的 effective tool name 为准：`effective_tool_name` 是 `resolve_effective_tool_name` 的 re-export（mod.rs:168 ← tool_search/core_tools.rs:51，识别 ExecuteExtraTool 解包后的真实目标名）；包装/搜索/代理工具不得绕过审批；broker + permission_mode 均 Some 才启用，否则 Bypass（后台 agent 默认，`assembly/workflow.rs::build_middlewares`） |
| 改 permission 敏感清单 / 分类器 | `src/permission/mod.rs` + `auto_classifier.rs` | `default_requires_approval`（mod.rs:42）；`sensitive_tool_entries`（:86，11 项）；`format_sensitive_tools`（:147）；`LlmAutoClassifier`（auto_classifier.rs:45，`new` :53、`with_cache_ttl` :62）；`Classification`（:16） | 敏感清单驱动 10_hitl 段落与运行时判定；LLM 分类器经 `auto_classifier` 注入 PermissionMiddleware（`assembly/preparation.rs::resolve_ports`）；workflow agent 不需要分类器（`assembly/workflow.rs::build_middlewares` 传 None） |
| 改 workflow 中间件 | `src/workflow/mod.rs` | `WorkflowMiddleware`（:36，`new` :55、`resume_workflow` :135、`create_tool` :92、`subscribe_notifications` :308、`progress_store` :108）；`WorkflowMiddlewareAdaptor`（:332，端口适配 + `collect_tools` :386） | executor 可用时才注册（根 `assembly.rs` 的 workflow adaptor 准备与 `ChainSlot::Workflow`）；优先复用 session 级实例（progress_store/registry/runner 跨 turn 存活），无则临时实例（print 模式）；经 `WorkflowMiddlewarePort` 注入装配面；通知双路径见 docs/design/workflow.md §4（Path A bg-task-completed、Path B push_defer 唤醒） |
| 改 cron 调度 / 工具 | `src/cron/mod.rs` + `src/cron/tools.rs` + `src/cron/middleware.rs` | `CronScheduler`（mod.rs:46，`register` :75、`remove` :101、`toggle` :106、`tick` :119、`list_tasks` :169、`get_task` :181）；`CronRegisterTool`（tools.rs:10）/`CronListTool`（:75）/`CronRemoveTool`（:134）；`CronMiddleware`（middleware.rs:13，collect_tools :29） | 调度器持 trigger 通道（`CronTrigger`，subscribe :68），tick 到点触发推送；任务按 id 增删/启停；经 `CronSchedulerPortHandle`（mod.rs:199）端口注入装配面 |
| 改 LSP 诊断工具 | `src/lsp/middleware.rs` + `src/lsp/tool.rs` + `src/lsp/formatters.rs` | `LspMiddleware`（middleware.rs:20，`new` :25、`from_pool` :32、`shared_pool` :43、collect_tools :54）；`LspTool`（tool.rs:90）；`format_*` 结果格式化（formatters.rs:54-410） | servers 非空才注册（`assembly/lsp.rs::add_lsp`）；优先复用 session 级 `LspServerPool`（跨 turn 存活服务器进程/初始化/诊断状态），None 时临时 pool；结果统一经 formatters 转文本 |
| 改 hooks 加载 / 执行 | `src/hooks/`（loader.rs + executor.rs + middleware.rs） | `HookMiddleware`（middleware.rs:46，`new` :69、`with_session_start` :91、`fire_post_tool_batch` :160）；`load_global_settings_hooks`（loader.rs:84）/`load_settings_local_hooks`（:176）/`load_settings_project_hooks`（:241）；`execute_command_hook`（executor.rs:19）/`execute_prompt_hook`（:151）/`execute_http_hook`（:211）/`execute_agent_hook`（:318） | 装配按 hook group 逐个展开（`assembly/hooks.rs::add_hooks`，每个非空 group 一个实例，组内顺序保留）；hook 输入/决策类型在 types.rs（HookInput :14 / HookDecision :108）；阶段触发 fire_pre_compact/fire_post_compact（stage_firing.rs:12/:37） |
| 改 hooks 匹配 / 护栏 | `src/hooks/matcher.rs` + `stop_block_guard.rs` + `once_tracker.rs` + `permission_gate.rs` | `matches_matcher`（matcher.rs:10）/`matches_if_condition`（:32）；`StopBlockGuard`（stop_block_guard.rs:28，`on_block` :40 / `current_count` :69）；`OnceTracker`（once_tracker.rs:16，`was_fired` :44）；`needs_permission_dialog`（permission_gate.rs:21） | matcher + if 条件决定命中；stop block 连续计次并格式化反馈（`format_stop_block_feedback` :89）；once hook 只触发一次（`is_once_hook` :28）；hook 审批与 PermissionMiddleware 判定共用 permission_mode |
| 改 goal steering | `src/goal_middleware.rs` + `src/goal/tool.rs` | `GoalMiddleware`（goal_middleware.rs:24，`new` :33、`after_agent` :85、`render_steering` :45）；`GoalTool`（goal/tool.rs:17，deferred，is_direct 默认 false） | controller 可用才装配（根 `assembly.rs::ChainSlot::Goal`，链最后）；goal active 且无既有 block_continue 时按 round 递增紧迫感模板注入（round 1/2/3+ 三档），必须以 `Human + <system-reminder>` 经 v2 MessageQueue `Defer` kind 注入（禁止 BaseMessage::system——会污染 frozen_system_prompt），并设 `block_continue = "goal_active"` 让 executor 自驱续跑 |
| 改 AGENTS.md 注入 | `src/agents_md/mod.rs` | `AgentsMdMiddleware`（:22，`with_extra_paths` :45、`with_excludes` :51、`with_frozen_content` :61、`read_frozen_content` :84） | 会话创建时读取冻结（主 + 本地 CLAUDE.md/AGENTS.md，excludes 过滤），SubAgent 复用冻结内容；禁止中途重读（ARC-FROZEN-001，测试 `frozen_claude_md`） |
| 改文件 / 终端 / Web / Todo / Image 工具 | `src/middleware/` | filesystem.rs（collect_tools :39）；terminal.rs（BashTool :21、TerminalMiddleware :480，collect_tools :534）；web.rs（WebMiddleware :7，WebFetchTool :38 / WebSearchTool :37）；todo.rs（TodoMiddleware :18，`new` 收 notify_tx :25）；image/（ImageMiddleware :26 + compressor.rs :30） | 纯工具提供器：collect_tools 注册 + 透传 is_direct；Todo 带通知通道；Image 处理 @image 附件转 ContentBlock::Image |
| 改 agent 定义 / 默认 prompt / 归属注入 | `src/agent_define/` + `src/default_system_prompt/` + `src/at_mention/` + `src/attribution/` | `load_overrides`（agent_define/mod.rs:78，`candidate_paths` :45）；`DefaultSystemPromptMiddleware`（default_system_prompt/mod.rs:112，`sections` :127）/`LangMiddleware`（:159）；`AtMentionMiddleware`（at_mention/mod.rs:28）；`GitAttributionMiddleware`（attribution/mod.rs:42，`attribution_text` :62、`current_branch` :118） | 链第一组上下文注入器；agent 定义 overrides 同时供 DefaultSystemPrompt 与 SubAgent fork 复用；Lang 语言指令段持有者；attribution 按 model_name 生成归属文本，并以 null stdin、1 秒异步等待预算与 direct-child kill-on-drop 做 best-effort 分支漂移观测，等待超时后继续 agent |
| 改装配输入端口实现 | `src/host_ports.rs` | `PluginManager`（:26，PluginManagerPort）、`SettingsHooksLoader`（:406，SettingsHooksPort）、`SkillsProvider`（:425，SkillsPort） | 3.0 批 2 波 2：插件加载 / 设置 hooks / skills provider 经端口注入 Agent 层装配面，本文件是端口实现方；其余端口（McpPoolPort / ToolSearchPort / WorkflowMiddlewarePort / CronSchedulerPort）实现在 Agent 层 session 工厂 |

## 子系统（按目录）

### 链装配（src/assembly.rs + src/assembly/）

| 功能 | 入口/关键点 |
| --- | --- |
| 装配实现 | 根 `ProductionChainAssembler::assemble` 唯一遍历蓝本并保留完整 disabled guards；`AssemblyContext` / `ChainAssembly` / `OnBgCompleteFn` / `SystemPromptBuilder` 仍 re-export 自 `peri-agent::session::factory` |
| 端口与继承工具准备 | `assembly/preparation.rs`：`ResolvedPorts` / `resolve_ports` / `build_parent_tools`；还原具体池与 broker，返回句柄由原 assemble 作用域继续持有，disabled 工具不进入父工具集 |
| 冻结 prompt / skills | `assembly/prompt.rs`：`add_agents_md` / `add_skills` / `add_skill_preload`；仅在相应启用槽位调用，使用冻结数据和 session 注册表 |
| Hook 组展开 | `assembly/hooks.rs::add_hooks`；保留组序、空组跳过及按槽位展开行为 |
| MCP 构造副作用 | `assembly/mcp.rs::add_mcp`；checked projection lease 复用/绑定、ensure_discovery、notifier 注入按原顺序，仅从启用 Mcp 槽位调用 |
| LSP 配置与池 | `assembly/lsp.rs`：`load_merged_lsp_servers` / `create_session_lsp_pool` / `add_lsp`；前两项仍由根公开，槽位复用 session 池或构造临时池 |
| Workflow agent 工厂 | `assembly/workflow.rs`：`WorkflowAgentMiddlewareFactory` / `default_workflow_middleware_factory`（根 re-export）；`resolve_agent_definition` / `build_tools` / `build_middlewares` / `build_tool_resolver` / `build_error_suggest` / `build_workflow_middleware`；独立链序、disabled 与 sandbox 契约不变 |

### deferred 工具（src/tool_search/）

| 功能 | 入口/关键点 |
| --- | --- |
| 中间件 | `ToolSearchMiddleware`（middleware.rs:26，before_agent 索引构建 :64/:100） |
| 索引 | `ToolSearchIndex`（tool_index.rs:149，build :190 / search :215 / get_tool :292 / format_deferred_list :306 / cached_prompt :357） |
| 元工具 | `SearchExtraTools`（search_tool.rs:18，is_direct=true）；`ExecuteExtraTool`（execute_tool.rs:72，is_direct=true）+ `ExecuteExtraToolResolver`（:17） |
| 搜索/声明/能力描述 | keyword_search.rs（评分）；declaration.rs（collect_declarations）；core_tools.rs（调用解析、direct_tools_sorted_csv / direct_tools_description） |

### 内置工具（src/middleware/）

| 功能 | 入口/关键点 |
| --- | --- |
| 文件 / 终端 | filesystem.rs（collect_tools :39）；terminal.rs（BashTool :21、TerminalMiddleware :480） |
| Web / Todo / Image | web.rs（WebMiddleware :7）；todo.rs（TodoMiddleware :18）；image/（ImageMiddleware :26 + compressor） |

### MCP（src/mcp/）

| 功能 | 入口/关键点 |
| --- | --- |
| 连接 / pool / task owner | client.rs（McpClientPool 状态所有权与稳定 re-export）；client/lifecycle.rs（begin_shutdown/shutdown、try_commit_connection）；client/service.rs（McpServiceWrapper 与 capability 声明）；client/types.rs（句柄、状态、connection key）；task_scope.rs（McpTaskOwner / weak McpTaskSpawner / keyed completion）；client/transport.rs（serve_client_auto、spawn_stdio_transport、build_http_transport）；client/subscription.rs（资源订阅循环）；initialize.rs（run_initialize）；reconnect.rs（spawn_reconnect/reconnect） |
| Dynamic registry | dynamic/registry.rs（RegistryState 所有权、deployment port 与公开 re-export）；registry/connector.rs（ProductionDynamicMcpConnector）；registry/load.rs / unload.rs / lifecycle.rs（操作与关闭）；registry/operations.rs（查询与通知）；registry/capability.rs（collision、snapshot 与 projection lease） |
| 状态 / 缓存 / OAuth 回调 | client/status.rs（server_infos、record_status_change、notify_initial_connections）；client/cache.rs（persistent_cache_allowed_for、read_resource_cached、list_all_tools_cached）；client/oauth.rs（reserve_oauth_flow_scoped、register_oauth_callback_scoped、release_oauth_flow_scoped） |
| 配置合并 | config.rs（load_merged_config_full :223 / load_merged_config :349 / remove_server_from_config :385 / set_server_disabled :473） |
| 工具 / 资源 / skill | tool_bridge.rs（build_tool_bridges :207）；resource_tool.rs（McpResourceTool :45）；discover_tool.rs（DiscoverMCPTool :32）；skill_discovery.rs + skill_discovery/（run_discovery :97、verify_and_build skills_list.rs:499） |
| 中间件 | middleware.rs（McpMiddleware :24，collect_tools :311 / before_agent :354 / before_model :364 / first_turn_reminder :335 / ensure_discovery :80 / attach_connection_notifier :196） |
| OAuth / 凭证 / 信道 | oauth_flow.rs、client_oauth.rs、auth_store.rs（FileCredentialStore）、callback_server.rs（OAuthCallbackServer :26）、channel_handler.rs（ChannelHandler :17）、mcp_notify.rs |

### 插件（src/plugin/）

| 功能 | 入口/关键点 |
| --- | --- |
| 加载 | loader.rs（load_manifest :77 / load_plugins :491 / load_enabled_plugins_aggregated :632 / merge_plugin_mcp_servers :612 / parse_command_md :66） |
| 配置 / 持久化 | config.rs（ClaudeSettings :11、installed_plugins 持久化 :175/:325、settings.json 启用名单 :428/:465） |
| 安装 / 市场 | installer/（install.rs:12 / update_plugin install.rs:168 / uninstall.rs:15）；marketplace/（MarketplaceManager :20）；install_counts.rs |
| 中间件 / 类型 | middleware.rs（PluginMiddleware :7）；types.rs（仅 re-export，事实源 peri-acp-types/src/plugin.rs:269） |

### Skills（src/skills/）

| 功能 | 入口/关键点 |
| --- | --- |
| 扫描 / 查找 | loader.rs（resolve_skill_roots :273 / scan_skill_roots :87 / find_skill_content :332 / list_skills :253） |
| 中间件 / 摘要 | mod.rs（SkillsMiddleware :104 / build_frozen_summary :267 / format_discovery_protocol :137 / global_config_path :28） |
| 工具 | tools.rs（SkillTool :29 / DiscoverSkillsTool :114）；builtin/（BuiltinSkill :14 / parse_builtin_frontmatter :50） |

### SubAgent（src/subagent/）

| 功能 | 入口/关键点 |
| --- | --- |
| 中间件 / 扫描 | mod.rs（SubAgentMiddleware :143 / scan_agents :326 / infer_agent_capability :395 / scan_agents_detailed :427） |
| 工具 / 链装配 | tool/（define.rs SubAgentTool :38、mod.rs build_subagent_middlewares :37、SubagentChainAssemblerImpl :133） |
| fork / 预加载 / 内置 | fork.rs（filter_tools :22）；skill_preload.rs（extract_skill_names_from_text :26）；built_in_agents.rs；agent_result.rs；descriptions/ |

### HITL 与审批（src/hitl/ + src/ask_user/ + src/permission/）

| 功能 | 入口/关键点 |
| --- | --- |
| 提问通道 | hitl/mod.rs（HumanInTheLoopMiddleware :39）；ask_user/mod.rs（parse_ask_user :31 / ask_user_tool_definition :65） |
| 审批 | permission/mod.rs（PermissionMiddleware :178 / default_requires_approval :42 / sensitive_tool_entries :86 / effective_tool_name re-export :168）；auto_classifier.rs（LlmAutoClassifier :45）；shared_mode.rs |

### Hooks（src/hooks/）

| 功能 | 入口/关键点 |
| --- | --- |
| 中间件 / 加载 | middleware.rs（HookMiddleware :46 / with_session_start :91）；loader.rs（:84/:176/:241） |
| 执行 / 匹配 / 护栏 | executor.rs（:19/:151/:211/:318）；matcher.rs（:10/:32）；action_resolver.rs（:20）；once_tracker.rs（:16）；stage_firing.rs（:12/:37）；stop_block_guard.rs（:28）；permission_gate.rs（:21）；types.rs（HookInput :14） |

### Workflow / Cron / LSP

| 功能 | 入口/关键点 |
| --- | --- |
| Workflow | workflow/mod.rs（WorkflowMiddleware :36 / WorkflowMiddlewareAdaptor :332 / resume_workflow :135） |
| Cron | cron/mod.rs（CronScheduler :46 / CronTask :33 / CronSchedulerPortHandle :199）；cron/tools.rs（:10/:75/:134）；cron/middleware.rs（CronMiddleware :13） |
| LSP | lsp/middleware.rs（LspMiddleware :20 / from_pool :32）；lsp/tool.rs（LspTool :90）；lsp/formatters.rs（:54+） |

### 上下文注入器与辅助（src/ 各单模块）

| 功能 | 入口/关键点 |
| --- | --- |
| AGENTS.md 注入 | agents_md/mod.rs（AgentsMdMiddleware :22 / read_frozen_content :84） |
| Goal steering | goal_middleware.rs（GoalMiddleware :24 / after_agent :85）；goal/tool.rs（GoalTool :17） |
| agent 定义 / 默认 prompt / @mention / 归属 | agent_define/（:35/:78）；default_system_prompt/（:112/:159）；at_mention/（:28）；attribution/（:38） |
| 工具包装 / 解析 / 辅助 | tools/（ArcToolWrapper :44 / BoxToolWrapper :50）；claude_agent_parser/（parse_agent_file :186 / format_agent_id :170）；meta_harness/（scan_harness_docs :23）；error_suggest/（build_tool_registry_snapshot，default_registry.rs:27）；host_ports.rs（:26/:406/:425） |

### 跨 crate 事实源（不在本 crate 内，改前先看这里）

| 功能 | 入口/关键点 |
| --- | --- |
| 工具 trait | `peri-acp-types/src/tools.rs`（BaseTool，is_direct 默认 false）；注册面 = 各 middleware `collect_tools()`：filesystem.rs:39、mcp/middleware.rs:311、lsp/middleware.rs:54、subagent/mod.rs:546、hitl/mod.rs:90、cron/middleware.rs:29、tool_search/middleware.rs:52、goal_middleware.rs:76 等 |
| 链序蓝本 | `peri-agent/src/session/factory.rs`（ChainSlot :27 / production_blueprint :89 / build_middleware_chain :144 / MiddlewareChainAssembler :130） |

## 跨模块契约（指向 architecture-contracts.md，不复制正文）

- ARC-MIDDLEWARE-001：链序唯一事实源是 Agent 层 `production_blueprint`，装配实现 `assembly.rs` 按蓝本一一对应；不得按名称/便利性重排
- ARC-TOOLS-001：`is_direct()` 自声明工具可见性；deferred 只能由 `SearchExtraTools` 发现、`ExecuteExtraTool` 执行；包装层透传
- ARC-FROZEN-001：frozen 数据（frozen_claude_md / skills 冻结摘要 / system prompt）会话内不可漂移，SubAgent 复用（`with_frozen_data` / `with_frozen_summary`）
- ARC-SERIAL-001：工具注册 / 序列化顺序确定（BTreeMap 工具表、稳定排序），不得依赖 HashMap 迭代序（prompt cache 前缀）
- ARC-PTC-ARTIFACT-001：`@peri-code/ptc@0.2.3`、Cargo `build.rs` 从 TypeScript 生成 `OUT_DIR` artifact、Rust 内嵌 bytes、npm 发布独立生成 gitignored `dist`、缓存 identity/hash、private temp、`node <entry>`、source 前 handshake 与 opt-in 精确 `npx` fallback 必须保持同步；fallback 明确承担供应链风险
- ARC-SECRET-001：MCP 凭证（FileCredentialStore / auth_store）、OAuth token 只落本机权限保护存储，不写日志/错误/fixture
- ARC-CANCEL-001：cancel 按 (session_id, turn_id, attempt_id) 三元组；SubAgent 同步子任务继承父取消（`with_cancel`），独立后台任务自身取消策略
- ARC-EVENT-001：事件链路单事实源 Agent 发射 → ACP 映射 → TUI 消费；SubAgent / Hook 事件须按 `source_agent_id` 归属
- ARC-BOUNDARY-001：TUI 不得直驱 Agent 运行时；MCP pool / 初始化由 Agent 层会话路径持有（装配端口注入），TUI 仅经 ACP 命令面读取快照
- ARC-HOST-SHUTDOWN-001：MCP pool lifecycle gate、external non-Clone task owner、weak callback/spawner 与有序关闭契约
