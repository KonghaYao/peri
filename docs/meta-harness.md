# MetaHarness — 系统提示词自定义与 middleware 卸载

> 面向使用者的特性说明。内部机制与设计决策见
> `docs/design/meta-harness.md`（单一事实源）；文档站版本见
> `peri-cool/src/content/docs/docs/features/meta-harness.mdx`。

MetaHarness 是 Peri 的一项配置能力：一个 `settings.json` kv 字段（
`meta_harness`）承载段落覆盖、能力裁剪与内置 Agent 定义策略：

- `true` key = **段落 ID** → 覆盖系统提示词段落（用 `.peri/meta/<ID>.md`
  全文替换内置段落；宿主经 workspace 实例的 `peri-meta://workspace/<ID>`
  资源读取——workspace 关闭、文档缺失或读取失败时 warn 并保持内置段落，
  不回落磁盘）；
- `false` key = **middleware 名** → 装配期关闭该 middleware（卸载其工具与
  钩子，无需 md 文件）；builtin 实例策略键则关闭对应 MCP 能力面；
- `BuiltInSubagents` = **内置定义策略** → bool 控制 compile-time 内置
  SubAgent 定义（默认开启），不关闭项目/plugin agents、fork/resume 或 Agent 工具。

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
  （角色定义）；读取经 `peri-meta://workspace/01_intro` 资源（见上）；
- `"05_using_tools": true`：同理替换工具使用纪律段落；
- `"WebMiddleware": false`：关闭 builtin `web` 实例（`WebSearch` /
  `WebFetch` 从 direct tools、deferred 目录与检索、subagent
  `parent_tools`、workflow agent 工具列表一并消失）；
- `"CronMiddleware": false`：关闭 builtin `cron` 实例。该实例的三个工具是
  deferred，因此关闭面是 deferred 目录与 `SearchExtraTools` 检索结果（后两个面
  本来就不含 deferred 工具），实例、handler 与 1s tick 保留；
- `"WorkspaceMiddleware": false`：关闭 builtin `workspace` 实例的工具面
  （`Read` / `Write` / `Edit` / `Glob` / `Grep` /
  `folder_operations` / `Bash`）。这 7 个工具全部是 direct，因此关闭面是三个真实面
  （首个模型请求 tools、subagent `parent_tools`、workflow agent 工具列表）；deferred
  目录与检索面本来就不含 direct 工具（平凡成立）。关闭后链上不再提供任何文件系统与
  shell 工具（该实例之外：外部 MCP server 与 subagent 的
  `WriteSandbox` 不在此关闭范围）。实例、handler 与 pool 级连接状态保留（策略关闭不是
  物理销毁）。

## 段落 ID 清单

段落 ID = `peri-acp/prompts/sections/` 文件名去 `.md`，另有渲染生成段。权威全集以 `peri-acp-types/src/meta_harness.rs::SECTION_IDS` 为准：

`01_intro`、`02_system`、`03_doing_tasks`、`04_actions`、`05_using_tools`、
`06_tone_style`、`07_runtime`、`10_hitl`、`11_subagent`、`12_ask_user`、
`13_skills`、`persona`、`language`

- `persona` / `language` 是渲染生成段，可经 `.peri/meta/persona.md` /
  `.peri/meta/language.md` 覆盖（同样经 workspace 资源面读取）；
- 覆盖全文**整段替换**内置段落，段落渲染顺序（位置 + 段内序号）不变；
- 覆盖为**空串**时段落整体消失；空白串原样渲染（不 trim）。

## 能力关闭键清单

`false` key 有两张表，**已知键集合是两者的并集**（`peri-acp/src/provider/config.rs`
消费，ARC-CAPABILITY-CLOSURE-001）：

1. **链槽位名**：使用装配面 middleware 的 `name()` 返回值；权威清单以
   `peri-acp-types/src/meta_harness.rs::MIDDLEWARE_NAMES` 为准。常用项包括：

`DefaultSystemPromptMiddleware`、`LangMiddleware`、`AgentsMdMiddleware`、
`PluginMiddleware`、`SkillsMiddleware`、
`SkillPreloadMiddleware`、`AtMentionMiddleware`、`ImageMiddleware`、
`GitAttributionMiddleware`、
`TodoMiddleware`、`HookMiddleware`、
`PermissionMiddleware`、`HumanInTheLoopMiddleware`、`SubAgentMiddleware`、
`McpMiddleware`、`WorkflowMiddleware`、`ToolSearch`、`GoalMiddleware`

> `FilesystemMiddleware` / `TerminalMiddleware` 已不是链槽位名（v4-part-4 wave 3）：
> 7 个文件/终端工具迁为由 builtin `workspace` 实例提供，这两个键不再是**已知键**，
> 配置里继续写它们会按未知键 warn 后丢弃。`AgentDefineMiddleware` 也已退役，定义改由 MCP
> 资源提供。关闭下面的 `WorkspaceMiddleware` 会关闭整个 workspace 能力面，不等于旧单项关闭。
>
> `GitWatchMiddleware` 同样已不是已知键（v4 wave 4）：git ref 变化改由 builtin
> `workspace` 实例的 `workspace://git/ref` 资源 + MCP 2026-07-28 订阅回传，链上不再有
> 该槽位。关闭办法 = `WorkspaceMiddleware: false`（关实例，同时跳过订阅建立）或实例配置
> 的 `subscriptions` 覆盖（显式空配置 ⇒ 不订阅）。见
> [design/git-watch-middleware.md](design/git-watch-middleware.md) §0.1。

2. **builtin MCP 实例策略键**：`WebMiddleware` / `ArtifactMiddleware` /
   `CronMiddleware` / `WorkspaceMiddleware`
   （`BUILTIN_INSTANCE_POLICY_KEYS`）。v4-part-2 起 Web / Artifact、v4-part-3 起
   Cron、v4-part-4 起 `workspace`（7 个文件/终端工具：`Read` / `Write` /
   `Edit` / `Glob` / `Grep` / `folder_operations` / `Bash`，模型面名字
   原始名）不再是链槽位——能力由同进程 builtin 实例（`web` /
   `artifact` / `cron` / `workspace`）提供，这四个键是该实例的关闭键；映射唯一来源是声明表
   `peri-acp-types/src/builtin_mcp.rs` 的 `policy_key`，**不按 `mcp__` 前缀或实例名
   硬编码过滤**。实例的另外两条关闭路径是配置片段
   `{"<实例>": {"disabled": true}}` 与进程级环境开关（`PERI_MCP_BUILTIN=off`），
   详见 [MCP 生态参考](reference/mcp-ecosystem.md)。

关闭语义：

- 关闭 = 该能力提供者退出注入面：链槽位 middleware 不进链（工具、钩子、提示词
  贡献一并消失）；builtin MCP 实例的策略键关闭实例工具面（实例仍在 MCP 面板可见，
  按配置路径另见上文第 2 条）；
- **策略关闭不是物理销毁**：四个维度分开看——工具可见性、cron tick、readiness 与
  物理生命周期互不连坐。关闭 `cron` 实例后 1s tick 由该代 builtin 监督者继续运行、
  实例保持 1R ready，也不停 handler；
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
- 未知 key（非段落 ID、非链槽位名、非 builtin 实例策略键、非 `BuiltInSubagents`）：解析期 warn + 忽略，
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

关闭全部提示词段落持有者后，系统提示词为空；覆盖不能创建没有持有者的段落。

## MCP 为主的生态如何管理

MetaHarness 是**会话内能力使用策略**，不是 MCP server 的安装、连接或生命周期管理器：

| 管理对象 | 配置入口 | 边界 |
| --- | --- | --- |
| 宿主消费与编排 adapter | `meta_harness` 中的链槽位键 | 关闭该 adapter 的工具、钩子与段落贡献，不等于停掉资源 provider |
| 内置 MCP 能力 | `meta_harness` 中的 builtin 实例策略键 | 注册表 `policy_key` 映射到实例关闭集；关闭工具注入与继承，workspace 还关闭资源面 |
| 外部 MCP server | MCP server 配置、连接/授权管理 | 没有任意 server 名对应的 MetaHarness 键；不要发明 `SomeServerMiddleware` |
| Agent 的工具权限 | session-local 工具视图与 allowlist/disallowlist、运行时审批 | provider 可连接不等于工具可执行；提示词覆盖也不授予权限 |

全局与项目配置逐 key 合并后冻结到会话，再由装配与工具视图消费。
`WebMiddleware: false` 等 builtin 策略不会销毁 pool 连接；如需配置层禁用实例，
使用对应 MCP 配置片段 `{"web": {"disabled": true}}`。
`McpMiddleware: false` 关闭 MCP 消费 adapter，但不是逐 server 的开关或 pool shutdown。
`ToolSearch: false` 关闭额外工具发现/执行入口，不等于关闭 MCP 连接或全部 direct 工具。

MCP 响应缓存由 MCP 配置顶层 `mcpCache` 与环境变量 `PERI_MCP_CACHE` 控制，
不增加 MetaHarness 键。任一来源关闭即关闭全部实例的响应缓存，但不关闭能力或连接。
配置合并、pool 生命周期与失效维护边界见 [MCP 缓存设计](design/mcp-cache.md)。

段落正文通过 workspace 的 `peri-meta://` 资源读取；同 scheme 的外部 server
不能成为覆盖来源。channel 已退役，`15_channel` 按未知键告警并忽略。

上述是现行职责与契约。关闭闭包、可选覆盖失败降级及跨 cwd 配置同源的已知缺口
由 `spec/history/2026-10.md`（2026-10-01 条目）记录，不能将契约表述当作全部已验收。

## 相关文档

- 设计文档（机制与决策单一事实源）：`docs/design/meta-harness.md`
- 文档站特性页：`peri-cool/src/content/docs/docs/features/meta-harness.mdx`
- System Prompt 冻结、缓存与安全边界：`docs/design/system-prompt.md`
