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
- `"WebMiddleware": false`：装配期关闭 Web 工具（WebFetch / WebSearch
  不进工具列表，其钩子全部失效）。

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

## middleware 名清单

`false` key 使用装配面 middleware 的 `name()` 返回值；权威清单以
`peri-acp-types/src/meta_harness.rs::MIDDLEWARE_NAMES` 为准。常用可关闭项包括：

`DefaultSystemPromptMiddleware`、`LangMiddleware`、`AgentsMdMiddleware`、
`AgentDefineMiddleware`、`PluginMiddleware`、`SkillsMiddleware`、
`SkillPreloadMiddleware`、`AtMentionMiddleware`、`ImageMiddleware`、
`FilesystemMiddleware`、`GitAttributionMiddleware`、`TerminalMiddleware`、
`WebMiddleware`、`TodoMiddleware`、`CronMiddleware`、`HookMiddleware`、
`PermissionMiddleware`、`HumanInTheLoopMiddleware`、`SubAgentMiddleware`、
`McpMiddleware`、`WorkflowMiddleware`、`ToolSearch`、`ArtifactMiddleware`、
`LspMiddleware`、`GoalMiddleware`

关闭语义：

- 关闭 = middleware 实例不进链：工具、钩子、提示词贡献一并消失；
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
- 未知 key（非段落 ID 非 middleware 名）：解析期 warn + 忽略，不 fail。

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
