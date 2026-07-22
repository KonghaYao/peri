> 归档于 2026-07-18，原路径 spec/issues/2026-07-07-ask-user-popup-never-appears.md
# AskUserQuestion 弹窗不出现，agent 卡死

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-07

## 问题描述

agent 在处理任务时调用 `AskUserQuestion` 工具后，TUI 界面没有弹出预期的问答窗口，agent 也停止响应不再继续执行（卡死）。该问题必现——只要 agent 触发 AskUserQuestion 工具就会复现。

## 症状详情

| 项目 | 详情 |
|------|------|
| 触发方式 | agent 自然调用 AskUserQuestion 工具（非手动触发） |
| 复现频率 | 必现 |
| 期望行为 | 弹窗展示 agent 提出的问题列表，用户可以用 ↑↓/1-9 选择选项，Enter 提交回答 |
| 实际行为 | 不出现任何弹窗，agent 陷入等待不再输出新内容 |
| 影响范围 | AskUserQuestion 功能完全不可用，需手动中断 agent 才能恢复 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 在 TUI 中给 agent 一个需要用户提供选项的任务（如 "帮我选择一个技术方案"）
  2. agent 调用 AskUserQuestion 工具
  3. 观察 TUI 界面：无弹窗出现，agent 停止响应

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-07 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
