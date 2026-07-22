> 归档于 2026-07-06，原路径 spec/issues/2026-07-05-paste-newline-triggers-submit.md

# 输入框粘贴含换行文本时直接触发 Enter 提交

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-05

## 问题描述

在 TUI 输入框中粘贴含换行符的文本（如 "请使用 subagent say hello"）时，粘贴内容未正常插入输入框，而是直接触发了 Enter 提交动作，导致消息被发送到 agent。

## 症状详情

- 复制一段含换行的文本粘贴到 Peri TUI 输入框
- 期望行为：文本完整粘贴到输入框中，光标停在粘贴内容末尾，用户可以继续编辑后再手动按 Enter 提交
- 实际行为：粘贴瞬间消息就被提交给 agent（如同按下了 Enter），用户无法在粘贴后编辑

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 Peri TUI
  2. 在输入框中粘贴一段含换行符的文本（如多行代码片段、从其他文档复制的段落）
  3. 观察输入框行为
- **环境**：macOS（影响所有平台），未启用 Bracketed Paste 的终端

## 涉及文件

- `peri-tui/src/kit/entry.rs` —— TUI 入口，负责设置/清除终端模式（Raw mode、Alt screen、Mouse capture、Bracketed Paste）
- `peri-tui/src/kit/input_area.rs` —— 输入框组件，正确实现了 `Event::Paste` 处理（552-571 行），但因终端未启用 Bracketed Paste，crossterm 不会将粘贴内容合并为 `Event::Paste` 事件

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-05 | — | Open | agent | 创建 |
| 2026-07-05 | Open | Fixed | agent | 修复已应用 |

## 修复记录

### 修复 #1（2026-07-05）

- **操作人**：agent
- **用户原意**：粘贴到输入框的内容不应自动提交，应保持可编辑状态
- **修复内容**：在 `entry.rs` 中为全屏模式启用 `EnableBracketedPaste`，退出时 `DisableBracketedPaste`。启用 Bracketed Paste 后，crossterm 会将粘贴内容合并为单个 `Event::Paste` 事件，`InputArea` 的 paste handler 正确处理该事件而不会触发 Enter 提交。
- **涉及 commit**：待 commit
- **验证状态**：待验证
