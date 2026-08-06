# peri-middlewares

## Scope

`peri-middlewares` 提供 Agent 的提示词注入、工具、插件、审批与子 Agent 中间件。生产链的唯一事实源是 Agent 层 session 工厂（`../peri-agent/src/session/factory.rs` 的链序蓝本 `production_blueprint`）；链装配实现位于本 crate 的 `src/assembly.rs`（L2 自 ACP builder 迁入，依赖反转完成后物理迁回 Agent 层）。顺序是行为契约，修改、增删或重排必须先以蓝本与装配实现为准。Hook 实例以及 MCP、Workflow、LSP、Goal 是否加入链均取决于会话与配置；不要复制或维护固定编号清单。

自动 compact 属于 `peri-agent` 的执行阶段，不在本 crate 的路由范围；参见 `../peri-agent/CLAUDE.md`。

## 数据流/架构

`SessionContext/config → Agent 层 session 工厂（`build_middleware_chain` 唯一触发点 + `production_blueprint` 链序蓝本）→ assembly.rs 槽位构造 → MiddlewareChain → prompt_contribution + collect_tools → Agent stage`。

- 插件加载结果提供 skill roots、agent dirs、hook groups 与 MCP 配置；各中间件消费对应输入。
- MCP 配置按全局 `~/.peri/settings.json`、插件、项目 `{cwd}/.mcp.json` 合并；工具与资源仅在 pool 可用时注册。
- Skills 按用户目录、配置的 `skillsDir`、项目目录、插件和内置来源的优先级搜索；目录含 `SKILL.md` 即为叶子，不再下钻。同名按来源顺序优先。
- SubAgent 从父工具、冻结上下文、取消策略与事件处理器派生执行上下文；具体 agent 定义和内置 agent 请直接查 `src/subagent/` 与项目 `.claude/agents/`，如需举例只使用 `explorer`。

## 任务路由

| 任务 | 首选位置 |
| --- | --- |
| 生产链顺序、条件注册、跨 crate 装配 | `../peri-agent/src/session/factory.rs`（蓝本）与 `src/assembly.rs`（槽位构造） |
| MCP 合并、server/tool bridge | `src/mcp/` |
| Plugin manifest、commands、agents、MCP 回退 | `src/plugin/` |
| Hook 事件与执行器 | `src/hooks/` |
| Skills 扫描、预加载、工具 | `src/skills/`、`src/skills/tools.rs` |
| SubAgent、后台任务、取消和事件 | `src/subagent/` |
| HITL 权限与审批 | `src/hitl/` |
| Workflow、LSP、工具搜索 | `src/workflow/`、`src/lsp/`、`src/tool_search/` |
| Todo、Cron、文件/终端/Web 工具 | `src/` 下对应模块 |

## 稳定不变量

- **链顺序**：只能在 Agent 层 session 工厂的链序蓝本（`production_blueprint`）与 `src/assembly.rs` 槽位构造中判断与修改生产顺序；不得按名称或局部便利重排。
- **MCP**：保留三层合并、内容去重和插件命名空间；配置来源或工具注册变更必须同时检查 pool、资源与 bridge 路径。
- **Plugin manifest**：`commands` 条目兼容字符串路径与对象；字符串是相对插件根目录的路径。agents 未声明时仍保留约定目录回退。不要把路径条目当作名称。
- **Skills**：扫描必须保持根优先级、递归边界、符号链接防环、叶子语义和同名覆盖规则；插件 skill root 通过既有扩展点传入。
- **SubAgent**：同一会话的子 Agent 复用冻结的项目指引、skills 与 system prompt；同步子任务继承父取消，独立后台任务使用自身取消策略。事件必须按 `source_agent_id` 归属，新增事件同时检查父/子边界、完成和取消路径。
- **HITL**：审批以解析后的 effective tool name 为准，包装、搜索或代理工具不得绕过审批；权限模式与 broker 的选择必须保持一致。
- **工具可见性**：direct/deferred 语义由工具声明和工具搜索路径共同保证，包装层不得改变其可见性。

## 目标命令

从仓库根目录执行：

```bash
cargo build -p peri-middlewares
cargo test -p peri-middlewares --lib
cargo test -p peri-acp --lib
```

## 按需引用 / Verify

- 链、工具注册与条件中间件：`../peri-agent/src/session/factory.rs` 与 `src/assembly.rs`；同时遵守 `../docs/standards/architecture-contracts.md` 的 `ARC-MIDDLEWARE-001`、`ARC-TOOLS-001`、`ARC-FROZEN-001`。
- Plugin/MCP 或 Skills 改动：阅读目标模块的实现与测试后运行对应 `cargo test -p peri-middlewares --lib <过滤词>`。
- SubAgent 或 HITL 改动：覆盖冻结数据、取消、事件归属及 effective tool name 的相关测试。
- 所有修改完成后运行 `git diff --check`；不得在日志、错误或测试 fixture 中写入密钥、token、密码或连接串。
