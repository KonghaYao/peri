# TUI 工具调用未以用户语义呈现 skill 与任务更新

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-29

## 问题描述

在 TUI 消息流中，`SkillTool` 和 `TodoWrite` 沿用通用 ToolCard 的原始参数与输出展示方式。使用 skill 时会显示 YAML/frontmatter 和全文行数；更新任务时会显示 `+[0],[1]` 一类内部数组索引。用户无法在不展开详情的情况下理解正在使用哪个 skill、任务有哪些状态变化，以及当前计划的完成进度。

期望将这两类调用改为用户语义优先的卡片：成功态提供紧凑、高价值的摘要，详情按需展开；失败态保留可操作的安全诊断信息。

## 现状

- `SkillTool` 的默认结果泄露 `SKILL.md` 的 YAML 和正文行数，缺少“使用哪个 skill、用于什么”的一行摘要。
- `TodoWrite` 接收完整任务快照后，当前摘要退化为数组下标，未展示新增、进行中、完成、移除等状态变化。
- 成功调用与失败调用未按信息需求区分默认展开策略，低价值日志会挤占消息流。
- 已确认的交互策略、结构化载荷和验收要求记录于仓库根目录 `TUI-TOOLCALL.md`。

## 期望改进方向

- 为 ToolCard 增加可选的、面向 UI 的语义摘要，并对不支持该摘要的调用保留通用渲染兜底。
- 将 `SkillTool` 呈现为“使用 skill · <名称>”，成功时显示已加载及安全的一行用途，默认隐藏 YAML 和正文。
- 将 `TodoWrite` 呈现为任务快照差分：显示完成/总数、此次状态变化和当前进行中项，不显示数组索引。
- 成功卡片默认折叠、错误自动展开；所有新文案同步中英文 i18n，按 Unicode 显示宽度处理窄终端。
- 通过 ACP 的结构化 DTO/事件载荷传递 skill 与 Todo 语义，TUI 不解析 YAML 或日志文本推断业务含义。

## 涉及文件

- `TUI-TOOLCALL.md` —— 已确认的语义卡片设计、迁移策略和验收标准。
- `peri-acp-types/` —— 后续承载 `SkillSummary`、`TodoSnapshot` 等跨客户端结构化 DTO。
- `peri-acp/src/event/` —— 后续生成与转发语义载荷的 ACP 事件映射。
- `peri-tui/src/kit/acp_events/` —— 消费标准 `ToolCall.rawInput`、维护会话内 Todo 快照和差分状态。
- `peri-tui/src/kit/message_area/` —— 渲染 Skill/Todo 专属卡片并保留其他工具的通用 ToolCard 兜底。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-29 | — | Open | agent | 创建：记录本次 TUI 工具调用语义卡片设计与实现需求 |
| 2026-07-29 | Open | Fixed | agent | 复用标准 `ToolCall.rawInput` 实现 Skill/Todo 语义卡片、会话内 Todo 差分和回放安全呈现 |

## 修复记录

### 修复 #1（2026-07-29）

- **操作人**：agent
- **用户原意**：让 `SkillTool` 与 `TodoWrite` 从 YAML、全文和数组索引等内部输出，改为可扫描的用户语义展示。
- **修复内容**：在 TUI 内部保留标准 `ToolCall.rawInput`，为实时、回放及子 agent 调用生成 `Skill`/`Todo` presentation；Skill 无论输入是否完整或调用是否失败均不显示 `SKILL.md` 原始输出。Todo 显示完成进度和相对最近成功快照的变更；调用按启动序号推进基线，失败、重复及乱序结束均不回退。结束事件仅接受 `completed`/`failed`，session reset 清空基线与待处理调用；极窄终端（含子 agent 嵌套卡片）按可用显示宽度退化。
- **涉及 commit**：未提交
- **验证状态**：已验证（`cargo fmt --check`、`cargo check -p peri-tui`、`cargo test -p peri-tui --lib`：605 passed、`git diff --check`）
