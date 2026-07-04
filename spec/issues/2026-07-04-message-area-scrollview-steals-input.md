# 主输入框无法输入——MessageArea ScrollView 事件处理器消费所有键盘事件

**状态**：Open
**优先级**：高
**创建日期**：2026-07-04

## 问题描述

最近几个 git commit 之后，打开 Peri TUI 后主输入框（InputArea）在任何输入状态下都无法输入任何字符。键盘事件全部被 MessageArea 的 ScrollView 重构引入的事件处理器消费，InputArea 收不到任何按键。

## 症状详情

| 项目 | 内容 |
|------|------|
| 触发条件 | 必现，打开 TUI 后在任何输入状态下都无法输入 |
| 期望行为 | 在 InputArea 中输入字符、命令、Ctrl 组合键等正常响应 |
| 实际行为 | 没有任何输入响应，所有按键均无效 |
| 影响范围 | 所有键盘输入——普通字符、快捷键、方向键等均被拦截 |

**问题机制**（现象层面）：

1. MessageArea 的事件处理器注册了 `EventScope::Global, EventPriority::High`
2. 该处理器对所有 `Event::Key(_)` 和 `Event::Mouse(_)` 事件返回 `EventResult::Consumed`
3. 由于优先级为 High 且全局作用域，所有键盘事件在到达 InputArea 之前被 MessageArea 消费
4. 旧代码只消费 Ctrl+↑/↓/Home/End 和鼠标滚轮事件，其余事件返回 Ignored（允许事件继续传递到 InputArea）
5. 修复已在进行中（消息区事件处理器过滤逻辑已修正），但需要记录此 bug

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 Peri TUI
  2. 焦点在主输入框（默认状态）
  3. 尝试输入任意字符 → 无响应
- **环境**：macOS，ratatui-kit 0.7.2

## 涉及文件

- `peri-tui/src/kit/message_area.rs` —— MessageArea 的 ScrollView 重构在 commit `64033e3a` 中引入了事件处理器 bug

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-04 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
