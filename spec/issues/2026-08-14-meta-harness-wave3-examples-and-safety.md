# MetaHarness 波 3——示例文档与安全段落覆盖风险提示

**状态**：Closed
**优先级**：中（文档交付物，无代码行为变更）
**类型**：文档
**创建日期**：2026-08-14
**来源**：设计文档 `docs/design/meta-harness-design.md` §2.9 波 3（"示例文档与
安全段落覆盖风险提示（2.4 边界）"）+ 实施质量审查 P2-4

## 任务定义

波 3 交付两件内容（设计定案时未落盘的文档产物）：

1. **示例文档**：`.peri/meta/` 用法示例（settings.json 配置 + 覆盖 md 文件
   形态），供用户上手 MetaHarness 段落覆盖能力。
2. **安全段落覆盖风险提示**：覆盖 `01_intro` / `10_hitl` 等含防御性安全
   内容与敏感工具列表的段落时，用户会**完全移除**这些防御信息，必须提示。

## 交付物（正文草稿，供文档站接入时搬运）

### 1. 示例文档

**settings.json 配置示例**：

```json
{
  "meta_harness": {
    "01_intro": true,
    "05_using_tools": true,
    "WebMiddleware": false
  }
}
```

- `true` key = 段落 ID → 覆盖系统提示词段落（需 `.peri/meta/<ID>.md` 存在）；
- `false` key = middleware 名 → 装配期关闭该 middleware（无需 md 文件）；
- 段落 ID 清单 = `peri-acp/prompts/sections/` 文件名去 `.md`（`01_intro`、
  `02_system`、`03_doing_tasks`、`04_actions`、`05_using_tools`、
  `06_tone_style`、`07_runtime`、`10_hitl`、`11_subagent`、`13_skills`、
  `15_channel`；另有渲染生成段 `persona` / `language`——均可经
  `.peri/meta/persona.md` / `.peri/meta/language.md` 覆盖；`07_env` 与
  `14_system_reminder` 已合并为 `07_runtime`，`16_workflow` 已整段删除，
  C2 迁移后段落 ID 集合以 `peri-acp-types` `SECTION_IDS` 为事实源）；
- middleware 名清单 = 装配面 middleware 的 `name()`（`WebMiddleware`、
  `FilesystemMiddleware`、`TerminalMiddleware`、`McpMiddleware`、…）。

**`.peri/meta/05_using_tools.md` 示例**（md 全文 = 替换体，整段替换）：

```markdown
# Tool usage policy (custom)

Use tools in batches when possible. Always follow the file-write discipline:
Read before Edit, prefer targeted edits over full-file rewrites.
```

变更下次会话生效（会话内冻结，不中途重读）。

### 2. 安全段落覆盖风险提示（正文草稿）

> **覆盖含防御性安全内容的段落会移除对应防护信息**。以下段落包含运行时
> 安全语义，完全替换后这些信息将不再出现在系统提示词中：
>
> - `01_intro`（角色定义 + 防御性安全）：内置版本含「仅处理防御性安全
>   任务」IMPORTANT 块与 URL 纪律。覆盖后该约束消失——模型不再被提示
>   拒绝攻击性/绕过权限的请求，且 URL 引用纪律由模型自觉维持。
> - `10_hitl`（HITL 审批 + **sensitive 工具列表**）：C3 后列表由
>   `HumanInTheLoopMiddleware` 按代码事实（`default_requires_approval`）
>   动态生成，但覆盖 `10_hitl` 段仍会整体移除该列表（覆盖 = 替换持有者
>   段落贡献）——模型失去工具敏感度认知，实际审批行为仍由运行时
>   `default_requires_approval` 强制执行（权限判定不依赖提示词），但模型
>   不再具备"调用前预期审批"的自觉，可能因意外审批打断而反复重试。
> - `05_using_tools`（工具使用纪律）：覆盖后批处理/Bash 纪律依赖自拟内容。
> - `11_subagent`（SubAgent 委托 + Agent Selection Guide）：C3 后 Selection
>   Guide 已重构为通用选择原则（无具体 agent 名映射，仓库级调度建议由
>   catalog 承载）；覆盖后委托授权边界说明消失（运行时边界仍由机制强制）。
>
> **建议**：自定义内容时保留原段落中的安全相关行（防御性安全约束、
> sensitive 列表、授权边界），或在覆盖文件中重新声明同等语义；删除
> `/` 移除对应 key（及 md 文件）即可恢复内置段落。

## 非目标

- 不改设计文档（§2.9 波 3 已声明任务；正文落本 issue，待文档站
  peri-cool 接入时搬运）。
- 不提供 `.peri/meta/` 默认模板文件（用户配置目录，非仓库内容）。

## 涉及文件

- `spec/issues/2026-08-14-meta-harness-wave3-examples-and-safety.md` — 本文件。
- 后续搬运目标：文档站（`peri-cool/`）使用指南。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-14 | — | Open | agent | 波 3 交付物落盘：示例文档 + 安全覆盖风险提示正文草稿 |
| 2026-08-14 | Open | Closed | agent | C4 落地：正文搬运至文档站 `peri-cool/src/content/docs/docs/features/meta-harness.mdx` 并接入 `astro.config.mjs` 导航（「Agent 能力」分组）；段落 ID 清单按 C3 后现状同步（13 项，见 `SECTION_IDS`）；本文件保留为正文来源记录（A2 落盘说明） |
