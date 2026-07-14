# Loading 中按 Ctrl+C 会直接退出应用

**状态**：Open
**优先级**：高
**创建日期**：2026-07-07

## 问题描述

在 Peri TUI 的 agent loading 状态中，用户按 `Ctrl+C` 后应用会直接退出。用户期望 loading 状态下 `Ctrl+C` 的优先级应高于退出逻辑，用于中断当前 Agent，而不是触发应用退出。

该问题在 `/issue-create` loading 流程中被观察到，并经用户补充确认：只要处于 loading 状态，按 `Ctrl+C` 就会必现直接退出。

## 症状详情

| 场景 | 用户操作 | 实际表现 | 期望表现 |
|------|----------|----------|----------|
| Agent loading 中 | 按 `Ctrl+C` | TUI 直接退出 | 中断当前 Agent，应用保持运行 |
| `/issue-create` loading 中 | 按 `Ctrl+C` | TUI 直接退出 | 中断 `/issue-create` 当前执行/等待状态 |

用户表述："loading 状态中, ctrl + c 直接退出了, 说明我们的 ctrl + C 状态没有维持好优先级"。

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 Peri TUI。
  2. 发起任意会让 Agent 进入 loading 的请求（例如 `/issue-create`）。
  3. 在 loading 状态中按 `Ctrl+C`。
  4. 观察到应用直接退出，而不是中断 Agent。
- **环境**：macOS；Peri TUI ratatui-kit 单路径架构。

## 涉及文件

- `peri-tui/src/kit/event_handlers.rs` —— TUI 全局键盘事件处理，包含 `Ctrl+C` 行为分支与退出提示状态。
- `peri-tui/src/kit/focus_router.rs` —— 全局快捷键分类，将 `Ctrl+C` 识别为 `GlobalShortcut::Quit`。
- `TUI-PAGE.md` —— 文档中记录了预期行为：`Ctrl+C` 在 loading 中应打断 Agent，空闲时才双击退出。
- `peri-tui-refactor-manual-checklist.md` —— 手动检查清单记录了 `Ctrl+C` 的预期优先级：有文本时清空、loading 中打断 Agent、空闲双击退出。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-07 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
