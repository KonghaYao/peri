# 状态栏第二行命令描述左对齐，应为右对齐

**状态**：Fixed
**优先级**：低
**创建日期**：2026-07-17

## 问题描述

状态栏第二行（`StatusBarRow2`）中，默认状态下的命令描述（`statusbar-hint-main`，如 `/: 命令 | Shift+Enter: 换行 | Shift+Tab: 模式`）显示为左对齐，设计上应为右对齐。

## 症状详情

| 项目 | 详情 |
|------|------|
| 触发条件 | 非 popup、非 @mention、非 / 斜杠 的默认状态 |
| 渲染位置 | 状态栏第二行 |
| 当前行为 | 命令描述左对齐显示 |
| 期望行为 | 命令描述右对齐显示 |

## 涉及文件

- `peri-tui/src/kit/status_bar.rs:228-237` —— `StatusBarRow2` 组件中默认 hints 的渲染分支

## 根因

ratatui-kit 的 `Text` 组件内部通过 `.alignment(props.alignment)` 覆盖 Paragraph 上设置的 `.right_aligned()`。`TextProps.alignment` 默认值为 `Alignment::Left`，因此直接在 Paragraph 上调用 `.right_aligned()` 无效。

## 修复

1. 导入 `ratatui::layout::Alignment`
2. 将 `Paragraph::new(hints).right_aligned()` 改为通过 `Text` 组件的 `alignment` prop 设置：`Text(text: Paragraph::new(hints), alignment: Alignment::Right)`

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|------|------|--------|------|
| 2026-07-17 | - | Fixed | agent | 修复：使用 Text 组件 alignment prop 替代 Paragraph.right_aligned() |
