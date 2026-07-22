# 实现 Skill Core Tool——LLM 可主动调用 skill 加载 SKILL.md 内容

**状态**：Open
**优先级**：中
**创建日期**：2026-07-22
**类型**：新功能

## 问题描述

目前 Perihelion 的 Skill 系统工作方式是：

1. `SkillsMiddleware` 在对话开始时扫描所有 SKILL.md，生成摘要注入 system prompt
2. LLM 只能**建议**用户使用某个 skill（如"你可以用 `/auto-issue-fixer`"）
3. 用户必须手动输入 `/skill-name`，由 `SkillPreloadMiddleware` 检测并注入 SKILL.md 内容

**LLM 无法自己主动调用 skill**——缺少一个类似 Claude Code `SkillTool` 的 Core Tool。Claude Code 的做法是：Skill 工具默认 **inline 模式**，当 LLM 调用时，工具按名称查找 skill，展开 SKILL.md 内容返回给 LLM，LLM 直接按 skill 指导执行。

**期望行为**：LLM 看到系统 prompt 中的 skill 摘要后，可以调用 `Skill(skill="auto-issue-fixer")` 工具，获取该 skill 的完整 SKILL.md 内容并立即执行。

## 症状详情

| 维度 | 当前行为 | 期望行为 |
|------|----------|----------|
| skill 调用方式 | 只能用户输入 `/skill-name` | LLM 可调用 Skill 工具 |
| skill 内容加载 | 用户输入触发 SkillPreloadMiddleware | LLM 调用 Skill 工具，返回 SKILL.md 内容 |
| 对 system prompt 的影响 | Skills 摘要纯信息展示 | Skills 摘要变为"可执行指令列表" |
| LLM 能否自行选择 skill | 否 | 是 |

**参考 Claude Code SkillTool**：`/Users/konghayao/code/ai/claude-code` 的 `packages/builtin-tools/src/tools/SkillTool/SkillTool.ts`

## 涉及文件

- `peri-middlewares/src/tools/` — 新增 `skill.rs` 工具实现
- `peri-middlewares/src/tool_search/core_tools.rs:38-60` — CORE_TOOLS 白名单新增 TOOL_SKILL
- `peri-middlewares/src/skills/loader.rs:40-49` — SkillMetadata（按名称查找的数据源）
- `peri-middlewares/src/skills/builtin/mod.rs:23-36` — BUILTIN_SKILLS（内置 skill 内容）
- `peri-middlewares/src/tools/mod.rs:1-6` — 模块声明
- `peri-middlewares/src/middleware/filesystem.rs`（或独立 middleware）— 工具注册

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-22 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
