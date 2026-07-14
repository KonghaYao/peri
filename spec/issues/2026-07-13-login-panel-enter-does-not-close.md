# Login 面板 Enter 选择 provider 后不关闭面板

**状态**：Open
**优先级**：中
**创建日期**：2026-07-13

## 问题描述

Login 面板中按 Enter 选择一个 provider 后，面板仍然保持打开状态，用户需要手动按 Esc 关闭。期望行为是 Enter 选择后自动关闭面板，同时刷新状态栏（类似 Model 面板的 Enter 行为）。

## 症状详情

| 时机 | 期望行为 | 实际行为 |
|------|---------|---------|
| Login 面板中按 Enter 选择 provider | 面板关闭 + 状态栏立即刷新为新 provider | 面板保持打开，只切换了 active_provider_id |

对比 Model 面板（`model.rs:129`）的 Enter 行为：选择 alias 后调用 `close_active_panel()` 自动关闭面板，并即时推送 `SERVICE_SNAPSHOT` + 触发闪烁动画。Login 面板缺少这两步。

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 打开 TUI → `/login` 打开 Login 面板
  2. ↑/↓ 移动光标选择一个与当前不同的 provider
  3. 按 Enter
  4. 观察：面板不关闭，需手动 Esc
- **环境**：macOS, TUI 模式

## 涉及文件

- `peri-tui/src/kit/panels/login.rs:63-75` —— Enter 事件处理：`tokio::spawn(activate_provider(...))` + `bump` 递增，但未调用 `close_active_panel()`
- `peri-tui/src/kit/panels/login.rs:189-220` —— `activate_provider()` 函数：写 `PERI_CONFIG_HANDLE` + 持久化，但未推送 `SERVICE_SNAPSHOT` / 触发关闭

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-13 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
