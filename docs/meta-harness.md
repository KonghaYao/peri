# MetaHarness — 系统提示词自定义与 middleware 卸载

> 面向使用者的特性说明。内部机制与设计决策见
> `docs/design/meta-harness.md`（单一事实源）；文档站版本见
> `peri-cool/src/content/docs/docs/features/meta-harness.mdx`。

MetaHarness 是 Peri 的一项配置能力：一个 `settings.json` kv 字段（
`meta_harness`）同时承载两项能力，bool 值决定动作：

- `true` key = **段落 ID** → 覆盖系统提示词段落（用 `.peri/meta/<ID>.md`
  全文替换内置段落）；
- `false` key = **middleware 名** → 装配期关闭该 middleware（卸载其工具与
  钩子，无需 md 文件）。

## 快速开始

`~/.peri/settings.json`（全局）或工作区 `.peri/settings.json`（项目级）：

```json
{
  "config": {
    "meta_harness": {
      "01_intro": true,
      "05_using_tools": true,
      "WebMiddleware": false
    }
  }
}
```

- `"01_intro": true`：用 `.peri/meta/01_intro.md` 全文替换内置的 01 段落
  （角色定义）；
- `"05_using_tools": true`：同理替换工具使用纪律段落；
- `"WebMiddleware": false`：关闭 builtin `web` 实例（`WebSearch` /
  `WebFetch` 从 direct tools、deferred 目录与检索、subagent
  `parent_tools`、workflow agent 工具列表一并消失）；
- `"CronMiddleware": false`：关闭 builtin `cron` 实例。该实例的三个工具是
  deferred，因此关闭面是 deferred 目录与 `SearchExtraTools` 检索结果（后两个面
  本来就不含 deferred 工具），实例、handler 与 1s tick 保留；
- `"LspMiddleware": false`：关闭 builtin `lsp` 实例的 `mcp__lsp__LSP` 工具面
  **并同时关闭文档同步目标**（不再读文件、不发 `didChange` / `didSave`）。只想关
  同步、保留工具时用链槽位名 `"LspSyncMiddleware": false`；
- `"WorkspaceMiddleware": false`：关闭 builtin `workspace` 实例的工具面
  （`Read` / `Write` / `Edit` / `Glob` / `Grep` /
  `folder_operations` / `Bash`）。这 7 个工具全部是 direct，因此关闭面是三个真实面
  （首个模型请求 tools、subagent `parent_tools`、workflow agent 工具列表）；deferred
  目录与检索面本来就不含 direct 工具（平凡成立）。关闭后链上不再提供任何文件系统与
  shell 工具（该实例之外：PTC 的 direct Node API、外部 MCP server 与 subagent 的
  `WriteSandbox` 不在此关闭范围）。实例、handler 与 pool 级连接状态保留（策略关闭不是
  物理销毁）。

## 段落 ID 清单

段落 ID = `peri-acp/prompts/sections/` 文件名去 `.md`，另有渲染生成段。权威全集以 `peri-acp-types/src/meta_harness.rs::SECTION_IDS` 为准：

`01_intro`、`02_system`、`03_doing_tasks`、`04_actions`、`05_using_tools`、
`06_tone_style`、`07_runtime`、`10_hitl`、`11_subagent`、`12_ask_user`、
`13_skills`、`15_channel`、`persona`、`language`

- `persona` / `language` 是渲染生成段，可经 `.peri/meta/persona.md` /
  `.peri/meta/language.md` 覆盖；
- `15_channel` 无持有 middleware（gate 恒关闭），覆盖也仅在能力装配后
  生效；
- 覆盖全文**整段替换**内置段落，段落渲染顺序（位置 + 段内序号）不变；
- 覆盖为**空串**时段落整体消失；空白串原样渲染（不 trim）。

## 能力关闭键清单

`false` key 有两张表，**已知键集合是两者的并集**（`peri-acp/src/provider/config.rs`
消费，ARC-CAPABILITY-CLOSURE-001）：

1. **链槽位名**：使用装配面 middleware 的 `name()` 返回值；权威清单以
   `peri-acp-types/src/meta_harness.rs::MIDDLEWARE_NAMES` 为准。常用项包括：

`DefaultSystemPromptMiddleware`、`LangMiddleware`、`AgentsMdMiddleware`、
`AgentDefineMiddleware`、`PluginMiddleware`、`SkillsMiddleware`、
`SkillPreloadMiddleware`、`AtMentionMiddleware`、`ImageMiddleware`、
`GitAttributionMiddleware`、
`TodoMiddleware`、`HookMiddleware`、
`PermissionMiddleware`、`HumanInTheLoopMiddleware`、`SubAgentMiddleware`、
`McpMiddleware`、`WorkflowMiddleware`、`ToolSearch`、
`LspSyncMiddleware`、`GoalMiddleware`

> `FilesystemMiddleware` / `TerminalMiddleware` 已不是链槽位名（v4-part-4 wave 3）：
> 7 个文件/终端工具迁为由 builtin `workspace` 实例提供，这两个键不再是**已知键**，
> 配置里继续写它们会按未知键 warn 后丢弃。关闭该能力请用下面的 `WorkspaceMiddleware`。

2. **builtin MCP 实例策略键**：`WebMiddleware` / `ArtifactMiddleware` /
   `CronMiddleware` / `LspMiddleware` / `WorkspaceMiddleware`
   （`BUILTIN_INSTANCE_POLICY_KEYS`）。v4-part-2 起 Web / Artifact、v4-part-3 起
   Cron / LSP、v4-part-4 起 `workspace`（7 个文件/终端工具：`Read` / `Write` /
   `Edit` / `Glob` / `Grep` / `folder_operations` / `Bash`，模型面名字
   原始名）不再是链槽位——能力由同进程 builtin 实例（`web` /
   `artifact` / `cron` / `lsp` / `workspace`）提供，这五个键是该实例的关闭键；映射唯一来源是声明表
   `peri-acp-types/src/builtin_mcp.rs` 的 `policy_key`，**不按 `mcp__` 前缀或实例名
   硬编码过滤**。实例的另外两条关闭路径是配置片段
   `{"<实例>": {"disabled": true}}` 与进程级环境开关（`PERI_MCP_BUILTIN=off`），
   详见 [MCP 生态参考](reference/mcp-ecosystem.md)。

**LSP 的两个键不要混用**：`LspSyncMiddleware` 是**链槽位名**（`after_tool` 文档
同步中间件），`LspMiddleware` 是**实例策略键**。前者只关同步，后者关工具面**且**
关同步目标；两者都不是物理销毁（host LSP pool、实例与 readiness 都保留）。

关闭语义：

- 关闭 = 该能力提供者退出注入面：链槽位 middleware 不进链（工具、钩子、提示词
  贡献一并消失）；builtin MCP 实例的策略键关闭实例工具面（实例仍在 MCP 面板可见，
  按配置路径另见上文第 2 条）；
- **策略关闭不是物理销毁**：五个维度分开看——工具可见性、LSP 文档同步、cron tick、
  readiness 与物理生命周期互不连坐。关闭 `cron` 实例后 1s tick 由该代 builtin
  监督者继续运行、实例保持 1R ready；关闭 `lsp` 实例或 `LspSyncMiddleware` 不关闭
  host LSP pool；两者都不停 handler；
- **审批与提问独立**：关闭 `PermissionMiddleware` 会移除审批钩子与
  `10_hitl`；关闭 `HumanInTheLoopMiddleware` 会移除 `AskUserQuestion` 与
  `12_ask_user`；两者互不替代；
- **配置迁移提醒**：旧配置中的 `"HumanInTheLoopMiddleware": false` 现表示
  “关闭提问”，不再表示“关闭审批”；若要关闭审批必须使用
  `"PermissionMiddleware": false`；
- **段落联动**：关闭 `SubAgentMiddleware` / `SkillsMiddleware` /
  `DefaultSystemPromptMiddleware` / `LangMiddleware` 会同时移除其持有的段落
  （11_subagent / 13_skills / 01-06 + 07_runtime + persona / language）；
- 关闭面覆盖全部装配入口（主链 / 子链 / Workflow agent 链 / /bg 后台
  agent），无链下泄漏。

## 配置来源与合并

- 全局 `~/.peri/settings.json` + 项目级 `{cwd}/.peri/settings.json` 经
  生产入口 `load()` 合并；
- meta_harness 为**逐 key 合并**专属特例：项目级 key 覆盖全局同 key，
  全局其余 key 保留；
- 未知 key（非段落 ID、非链槽位名、非 builtin 实例策略键）：解析期 warn + 忽略，
  不 fail。

## 生效时机

- 配置与 md 文件在**会话创建（session/new）冻结期**一次读取；会话内不
  中途重读（ARC-FROZEN-001）；
- 删除 md / 删除 key → 下次会话创建生效；
- SubAgent / fork / workflow 子面复用主会话冻结状态，覆盖同源。

## 风险提示

覆盖系统提示词是**强能力**：

- 覆盖安全相关段落（01_intro 防御性安全、10_hitl 审批机制等）会改变模型
  的安全行为基线，覆盖内容需自行承担后果；
- 关闭 `DefaultSystemPromptMiddleware` 会清空全部基础段与 persona 覆盖
  （纯净模式）；关闭 `LangMiddleware` 模型失去语言指令；
- 建议先用"关闭 middleware"做能力裁剪，段落覆盖仅在有明确改写需求时使用。

## 纯净模式

关闭 `DefaultSystemPromptMiddleware` + `LangMiddleware` + 其余全部
middleware（AskUserQuestion 除外）= 系统提示词只剩无持有者的
15_channel（gate 恒关闭）——"完全纯净"路径。

## 相关文档

- 设计文档（机制与决策单一事实源）：`docs/design/meta-harness.md`
- 文档站特性页：`peri-cool/src/content/docs/docs/features/meta-harness.mdx`
- System Prompt 冻结、缓存与安全边界：`docs/design/system-prompt.md`
