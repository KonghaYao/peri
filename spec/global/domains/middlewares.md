# 中间件 / 工具生态领域

## 领域综述

Peri 的中间件与工具生态：MCP / Plugin / Skills 等外部能力接入，SubAgent 构建与调度，HITL 审批，Workflow 编排，以及 Hook / Goal / Cron 等横切能力。核心数据流为 `SessionContext/config → ACP builder → MiddlewareChain → prompt_contribution + collect_tools → Agent stage`；生产链的唯一事实源是 `peri-acp/src/agent/builder.rs`，链顺序是行为契约，修改、增删或重排必须先以该 builder 为准。

## 核心流程

- **中间件装配**：`peri-acp/src/agent/builder.rs` 组装 `MiddlewareChain`，按功能分组固定顺序——上下文注入器（AgentsMd → AgentDefine → Plugin → Skills → SkillPreload → AtMention → Image）→ 工具提供器（Filesystem → GitAttribution → Terminal → Web）→ Todo / Cron → Hook 组 → HITL / SubAgent → MCP / Workflow / ToolSearch → LSP → Goal（链最后）。装配后 `collect_prompt_contributions` 合并声明式 prompt 段，`collect_tools` 把中间件提供的工具填充到 `shared_tools`（deferred 工具注册表）。
- **工具分发**：v2 stages 每轮从 `shared_tools`（BTreeMap）按名读取工具；非核心工具（MCP / Cron / Workflow 等）注册为 deferred，由 `ToolSearchMiddleware` 提供 SearchExtraTools / ExecuteExtraTool 两个元工具实现按需发现与代理执行；HITL 审批以解析后的 effective tool name 为准，包装、搜索或代理工具不得绕过审批。
- **SubAgent 构建**：fork（`execute_fork.rs`，继承父会话 parent_messages + fork directive，Cancel = Cascade）/ non-fork（`define.rs`，按 agent_def 过滤父工具集，Cancel 由 cancel_policy 决定）/ background（`execute_bg.rs` + `spawner.rs`，Cancel = Independent，tokio::spawn 内运行）三路径，统一经 `build_v2_subagent_context`（`v2_bridge.rs`）构造 v2 `StageContext` 后跑 `run_react_loop`；子链固定 AgentsMd → Skills → [SkillPreload] → Todo。
- **MCP 工具注册**：配置三层合并（全局 `~/.peri/settings.json`、插件、项目 `{cwd}/.mcp.json`，`load_merged_config`）→ `McpClientPool` 管理 rmcp 连接（stdio / Streamable HTTP / OAuth）→ `McpMiddleware::collect_tools` 调 `build_tool_bridges` 将各 server 工具包装为 `McpToolBridge`（命名 `mcp__server__tool`），资源可用时注册 `McpResourceTool`。

## 技术方案总结

| 维度 | 选型 |
|------|------|
| 中间件系统 | `MiddlewareChain`（before_agent / prompt_contribution / collect_tools / before_tool），装配唯一事实源 `peri-acp/src/agent/builder.rs` |
| MCP | rmcp crate；`McpClientPool` + `McpToolBridge`（`mcp__server__tool` 命名）；三层配置合并、内容去重与插件命名空间；stdio / Streamable HTTP + OAuth |
| Plugin | manifest 加载（`load_enabled_plugins`）提供 skill roots、agent dirs、hook groups 与 MCP 配置；commands 兼容字符串路径与对象；marketplace 安装 / 更新 / 卸载 |
| Skills | `SkillsMiddleware` 渐进式摘要注入；SkillTool / DiscoverSkillsTool 动态发现加载；五来源优先级（User / Global / Project / Plugin / Builtin），目录含 `SKILL.md` 即叶子，同名按来源顺序优先 |
| SubAgent | fork（继承父消息）/ non-fork（agent_def 构建）/ background（独立取消 + 后台任务注册表）三路径，统一 `build_v2_subagent_context` → `run_react_loop` |
| HITL | `HumanInTheLoopMiddleware` before_tool 拦截 + `UserInteractionBroker` 审批；`PermissionMode`（Default / AcceptEdit / AutoMode / Bypass）动态切换 + `LlmAutoClassifier` 自动审批 |
| Workflow | peri-workflow crate；`WorkflowMiddleware`（session 级共享 runner / registry / progress_store / journal_store）+ `WorkflowMiddlewareAdaptor` 每轮注册 WorkflowTool 为 deferred |
| Cron | 纯内存 `CronScheduler`（croner 解析 5 段表达式，上限 20 任务）+ cron_register / cron_list / cron_remove 工具；触发经 CronTrigger 注入会话队列 |

---

## 稳定入口

| 模块路径 | 职责 |
|---------|------|
| `peri-acp/src/agent/builder.rs` | 生产中间件链装配（顺序契约，禁止重排） |
| `peri-middlewares/src/mcp/` | MCP 配置合并、client pool、tool/resource bridge、OAuth 与重连 |
| `peri-middlewares/src/plugin/` | 插件 manifest、loader、marketplace、installer |
| `peri-middlewares/src/skills/` | Skills 扫描、摘要注入、SkillTool / DiscoverSkillsTool |
| `peri-middlewares/src/subagent/` | SubAgent 构建（fork / non-fork / background）、`v2_bridge`、后台任务注册表 |
| `peri-middlewares/src/hitl/` | HITL 审批、`PermissionMode` 共享模式、自动分类器 |
| `peri-middlewares/src/workflow/` | Workflow 编排中间件（runner / registry / progress / journal） |
| `peri-middlewares/src/hooks/` | Hook 事件类型、matcher、executor、`HookMiddleware` |
| `peri-middlewares/src/cron/` | 定时任务调度器与 Cron 工具 |
| `peri-middlewares/src/tool_search/` | 延迟工具 SearchExtraTools / ExecuteExtraTool、工具搜索索引 |
| `peri-middlewares/src/lsp/` | LSP 诊断中间件与工具 |
| `peri-middlewares/src/goal_middleware.rs` | Goal 自驱 steering（链最后，after_agent 注入 Defer 续跑） |
| `peri-middlewares/src/middleware/`、`src/tools/` | 文件 / 终端 / Web / Todo 基础工具中间件与 `ArcToolWrapper` / `BoxToolWrapper` |
| `peri-middlewares/src/agents_md/`、`agent_define/`、`at_mention/`、`attribution/`、`claude_agent_parser/` | 指引注入、agent 定义解析、@ 提及、git 贡献追踪等基础中间件 |

---

## Issue 经验附录

相关历史 issue 见 [domains/agent.md](agent.md)，不迁移条目。
