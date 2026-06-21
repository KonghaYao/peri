# peri-middlewares

中间件实现 crate，依赖 `peri-agent` 和 `peri-lsp`。19 个中间件按固定顺序组成链。

## 中间件链执行顺序

```
1.  AgentsMdMiddleware       ← CLAUDE.md/AGENTS.md 注入
2.  AgentDefineMiddleware    ← agent 定义，model/maxTurns 覆盖
3.  SkillsMiddleware         ← Skills 摘要注入（含插件 plugin_roots）
4.  SkillPreloadMiddleware   ← #skill-name 全文注入
5.  AtMentionMiddleware      ← @path 解析，注入 Read 工具调用
6.  FilesystemMiddleware     ← 6 个文件系统工具
7.  GitAttributionMiddleware ← before_tool/after_tool 追踪 Write/Edit 贡献字符数
8.  TerminalMiddleware       ← Bash
9.  WebMiddleware            ← WebFetch/WebSearch
10. TodoMiddleware           ← after_tool 解析 TodoWrite
11. CronMiddleware           ← Cron 调度
12. HookMiddleware           ← hooks 事件拦截（多组实例）
13. HumanInTheLoopMiddleware ← before_tool 拦截
14. SubAgentMiddleware       ← Agent 工具
15. McpMiddleware            ← MCP 工具和资源（pool 成功时注册）
16. ToolSearchMiddleware     ← SearchExtraTools/ExecuteExtraTool 代理
17. LspMiddleware            ← LSP 工具 + after_tool 文件变更同步
18. CompactMiddleware        ← 上下文压缩（before_model 钩子，含 once-per-prompt 守卫）
19. GoalMiddleware           ← after_agent 注入递增紧迫感 steering + 设 block_continue 自驱循环（链最后）
[ReActAgent.with_system_prompt()] ← prepend
```

插件通过 `plugin_skill_roots: Vec<SkillRoot>` → `SkillsMiddleware.with_plugin_roots()`、`plugin_hooks` → `HookMiddleware` 注入，无独立 PluginMiddleware。

## MCP 中间件

`McpMiddleware` 基于 `rmcp` crate。配置三层合并：全局 `~/.peri/settings.json` → 插件层 → 项目 `{cwd}/.mcp.json`（含内容 hash 去重）。工具命名 `mcp__{server_name}__{tool_name}`。插件 MCP 使用 `plugin:{plugin_name}:{server_name}` 前缀命名空间。

**[TRAP]** `ClaudeSettings` 的 `extraKnownMarketplaces` 和 `enabledPlugins` 需同时支持对象和数组格式。**`enabledPlugins` 写入必须用对象格式** `{"id": true}`。

**Plugin Sources 旁路表**：`load_merged_config_full` 返回 `(McpConfigFile, HashMap<String, String>)`，key 格式 `"plugin:{name}:{server}"`，value `"name@marketplace"`。

## 插件系统

兼容 Claude Code 插件生态。配置：`~/.peri/settings.json`（全局）+ `~/.claude/plugins/cache/`（插件 manifest）。

**Hooks**（`src/hooks/`）：4 种执行类型（Command/Prompt/Http/Agent），14 种事件。exit code 控制流程：0=Allow，1=Warn，2=Block。SSRF 防护阻止内网地址（`ssrf_guard.rs`），回环地址允许。

**Frontmatter 解析**：skill 和插件命令用 `gray_matter` crate（YAML engine），必须复用 `Matter::<YAML>::new()` 模式。

**Skills**：搜索顺序 `~/.claude/skills/` → `skillsDir` → `./.claude/skills/` → 插件 skills → **Builtin（随二进制分发）**。统一收口到 `scan_skill_roots(roots: &[SkillRoot])`：递归扫描（深度上限 6，目录数上限 1000/root），symlink 跟随 + canonicalize 防环，叶子语义（含 SKILL.md 则停止下钻），跨根去重按 roots 顺序先到先得。`SkillsMiddleware.with_plugin_roots()` 是插件扩展点。

**Builtin skills**：`include_str!` 编译期嵌入 SKILL.md 到二进制（注册表 `skills::builtin::BUILTIN_SKILLS`），最低优先级，被任意层级同名覆盖。`scan_skill_roots_impl` 主循环对 `SkillSource::Builtin` 特判（跳过 `is_dir()` 检查，直接从常量数组加载）。虚拟路径 `<builtin>/<name>` 不对应磁盘文件，`SkillPreloadMiddleware` 通过 `source == Builtin` 判断走常量查找（而非 `std::fs::read_to_string`）。`settings.json::config.disableBundledSkills: true` 全局禁用，session/new 时一次性冻结（main agent 路径）或每次调用读取（TUI/Stdio 显示路径）。新增 builtin skill：把 SKILL.md 放到 `src/skills/builtin/skills/<name>/SKILL.md`，在 `BUILTIN_SKILLS` 数组追加 entry（`test_builtin_skills_frontmatter_valid` 自动覆盖）。

**[TRAP]** Manifest `skills` 字段语义：`skills` 数组条目是相对于插件根目录的路径（如 `"./skills/"`、`"skills/tdd"`），不是 skill 名称。`extract_skills_paths` 用 `base_dir.join(entry)` 解析路径并返回 `Vec<SkillRoot>`（携带 `source=Plugin` + `plugin_name`）。容器根的递归下钻与叶子检测统一由 `scan_skill_roots` 处理，extract_skills_paths 不再扫描子目录验证 SKILL.md 存在性。绝不能把条目当名称拼接到 `base_dir/skills/` 下——会生成 `base_dir/skills/./skills/` 这样的无效路径。

**[TRAP]** Manifest `commands` 字段类型：Claude Code 插件 manifest 的 `commands` 支持混合数组（字符串路径 + 对象），如 `["./commands/", {"path":"x.md","name":"x"}]`。`PluginManifest.commands` 类型是 `Option<Vec<PluginCommandEntry>>`（`PluginCommandEntry` 枚举：`Path(String)` | `Full(PluginCommand)`）。`extract_commands` 必须用 match 分支处理两种变体。禁止假设所有条目都是 `PluginCommand` 对象——字符串路径是 Claude Code 插件的常见格式（如 ECC 的 `"commands": ["./commands/"]`）。

**[TRAP]** Agent 目录回退扫描：`extract_agents_paths` 在 manifest 无 `agents` 字段时必须回退扫描插件根目录下的 `agents/` 和 `.agents/` 子目录。Claude Code 插件常把 agent 定义放在 `.agents/` 目录但不在 manifest 中声明。新增 agent 目录约定时必须同步更新回退扫描的目录列表。

**[TRAP]** 插件 MCP `.mcp.json` 回退：`extract_mcp_servers` 有两层加载逻辑——先处理 manifest `mcpServers` 字段，结果为空时回退加载 `install_path/.mcp.json`。MCP pool 初始化通过 `load_merged_config_full` 独立调用 `load_enabled_plugins_aggregated`，不依赖 TUI 层传递插件 MCP 数据。

## Compact 中间件

`CompactMiddleware`：`before_model` 钩子，在 ReAct 循环内触发 compact。**[TRAP]** Micro compact 必须加 once-per-prompt 守卫（AtomicBool），否则每轮都重复触发。（详见 spec/global/domains/compact.md#issue_2026-05-23-micro-compact-repeated-triggering）

## LSP 中间件

`LspMiddleware` + `LspTool` + `peri-lsp` 客户端库。10 种操作（goToDefinition/findReferences/hover 等），`after_tool` 自动同步文件变更（`didChange` + `didSave`）。

## SubAgents

`.claude/agents/` 下定义，支持扁平 `{agent_id}.md` 和嵌套 `{agent_id}/agent.md`。`tools` 为空继承父工具（排除 Agent 防递归），有值仅保留允许列表，`disallowedTools` 额外排除。插件 agent 通过 `scan_agents_with_extra_dirs` 追加搜索路径。内置 agents（coder/explore/general-purpose/plan/verification/web-researcher 共 6 个）编译期嵌入，同名被项目级覆盖。

**[TRAP]** Background agent 工具完全依赖 `register_tool` 传递，跨 async 边界需确保 Arc 引用生命周期。多语义叠加（fork+background）需明确优先级，跨轮次累积数据（frozen_vms）必须有清理机制。（详见 spec/global/domains/agent.md#issue_2026-05-12-background-agent-display-and-continuation-bugs）

**[TRAP]** Normal/Fork 子 Agent 透传 event_handler 导致事件溢出，`StateSnapshot`/`ContextWarning`/`LlmRetrying` 缺少 `in_subagent()` 守卫——新增事件类型时必须同步检查所有事件处理路径的守卫。（详见 spec/global/domains/agent.md#issue_2026-05-13-sync-subagent-events-leak-to-parent）

**[TRAP]** 并发 SubAgent 场景：事件路由必须用 `source_agent_id` 精确匹配而非位置堆栈；流式循环必须 `tokio::select!` 竞争取消令牌防止 Ctrl+C 死锁；Background Fork 使用 `CancelPolicy::Independent`。（详见 spec/global/domains/agent.md#issue_2026-05-16-concurrent-subagent-tool-call-routing-and-background）

**[TRAP]** 同步 SubAgent 取消传播：父 Agent 的 cancel token 通过 `CancelPolicy::Cascade → child_token()` 传播到同步 SubAgent 执行上下文。（详见 spec/global/domains/agent.md#issue_2026-05-25-ctrl-c-cannot-interrupt-sync-subagent）

## HITL 审批

默认需审批：`Bash`、`folder_operations`、`Agent`、`Write`、`Edit`、`delete_*`、`rm_*`、`mcp__*`、`WebFetch`、`WebSearch`。
