# 面板中按 Up 方向键导致 TUI 完全卡死

**状态**：Open
**优先级**：高
**创建日期**：2026-07-04

## 问题描述

在任一有列表导航的面板（Login、MCP、Memory、Cron、Model 等）中按下 Up 方向键，TUI 立刻完全卡死。所有按键无响应，只能强制退出进程。Down 键导航完全正常。

## 症状详情

| 现象 | Down 键 | Up 键 |
|------|---------|-------|
| 响应 | 正常导航，光标下移 | 立刻卡死 |
| 光标位置 | 任意位置均可 | 任意位置均触发 |
| 面板数量 | 无影响 | 无影响 |
| 项目数量 | 4 个时出现 | 任意数量均可 |

**关键特征**：
- Down 键的列表导航工作正常，光标可以移到任意位置
- Up 键一旦按下，无论光标在哪个位置（0、1、2、3），立即卡死
- 卡死后完全无响应，按 Esc 也无法恢复
- 多个面板均受影响：Login、MCP、Memory、Cron、Model

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 在 TUI 中输入 `/login` 打开 Login 面板（或其他带列表的面板如 `/mcp`、`/memory`）
  2. 按 Down 键 1-3 次（正常导航）
  3. 按 Up 键 1 次 → TUI 立即卡死
- **环境**：macOS，ratatui-kit 组件系统

## 涉及文件

- `peri-tui/src/kit/panels/login.rs` —— 受影响的 Login 面板
- `peri-tui/src/kit/panels/mcp.rs` —— 受影响的 MCP 面板
- `peri-tui/src/kit/panels/memory.rs` —— 受影响的 Memory 面板
- `peri-tui/src/kit/list_nav.rs` —— 共用的 `previous_selection()` 函数

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-04 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
