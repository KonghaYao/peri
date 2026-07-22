# SubAgent 展开区工具调用全部显示且有空行，应截断到后 5 个并移除区内空行


> 归档于 2026-07-20，原路径 spec/issues/2026-07-05-subagent-toolcard-truncate-and-nospacing.md
**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-05

## 问题描述

SubAgent 展开后，内部嵌套消息（主要是工具调用 ToolCard）目前**全部显示**且**每条之间有空行间隔**。在工具调用较多的场景下（如 10+ 个 Read/Grep），占位过大、视觉密度过高，与 TUI 信息密度设计哲学不一致。

期望改为：仅显示**最后 5 个工具调用**，且 SubAgent 展开区内的所有嵌套消息之间**不留空行**。

## 症状详情

### 现象 1：工具调用全部显示

| 场景 | 当前行为 | 期望行为 |
|------|----------|----------|
| SubAgent 有 3 个工具调用 | 全部 3 个显示 | 全部显示（不足 5 个时不截断） |
| SubAgent 有 10 个工具调用 | 全部 10 个显示 | 只显示最后 5 个，其余省略（显示 `… N more tools` 汇总行） |
| SubAgent 有 5 个工具调用 | 全部 5 个显示 | 全部显示 |

### 现象 2：SubAgent 区内嵌套消息有空行间隔

当前 SubAgent 展开区中，每条嵌套 ToolCard 都被 `with_message_spacing()` 包裹，导致两个工具调用之间出现空行。TUI-STYLE.md 第 107 行的 "SubAgent 展开时用空行分隔嵌套消息与结果区" 是指 SubAgent 区整体与结果区之间的分隔，不是嵌套消息之间。

期望：SubAgent 展开区内的所有嵌套消息（ToolCard、AssistantBubble 等）之间不留空行。SubAgent 整体区域与外层消息之间仍保留空行（由 `with_message_spacing` 在 `render_subagent_group` 末尾处理即可）。

## 复现条件

- **复现频率**：必现（当前默认行为）
- **触发步骤**：
  1. 启动 Peri TUI
  2. 提交一个包含 SubAgent 工具调用的 prompt（如"帮我读一下 src 下所有 rs 文件"）
  3. 展开任意 SubAgent 卡片查看内部工具调用
- **环境**：macOS，当前 perihelion 代码库

## 涉及文件

- `peri-tui/src/kit/view_render.rs` —— `render_subagent_group()` 函数（第 547-676 行），负责 SubAgent 展开区的渲染。包含子内容的遍历循环和 `with_message_spacing()` 调用。
- `TUI-STYLE.md` —— 第 107 行的间距规则在此次改动后需要更新为新的语义。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-05 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
