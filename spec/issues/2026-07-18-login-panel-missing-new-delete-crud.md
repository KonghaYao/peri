# Login 面板缺少新建/删除功能，CRUD 不完善

**状态**：Open
**优先级**：中
**创建日期**：2026-07-18

## 问题描述

`/login` 面板当前仅支持 **浏览列表**（Browse）和 **编辑已有 Provider**（Edit）两种模式，缺少 **新建 Provider**（Create）和 **删除 Provider**（Delete）操作入口。用户无法在面板内完成 Provider 的完整生命周期管理，CRUD 只有 RU 两环。

## 症状详情

| 操作 | Browse 模式快捷键提示 | 实际支持 |
|------|----------------------|---------|
| 浏览列表 | ↑/↓ 导航 | ✅ 已实现 |
| 激活 Provider | Enter | ✅ 已实现 |
| 编辑 Provider | E | ✅ 已实现 |
| 新建 Provider | —（无） | ❌ 缺失 |
| 删除 Provider | —（无） | ❌ 缺失 |

- Browse 模式的快捷键提示行只显示 `↑/↓ :导航  Enter :激活  E :编辑  Esc :关闭`，不包含新建和删除
- `LoginPanelMode` 枚举仅有两个变体：`Browse`、`Edit`，无 `New` / `Delete` / `ConfirmDelete`
- i18n key 已提前定义好但未使用：`login-panel-title-new`、`login-panel-title-confirm-delete`、`login-key-new`、`login-key-delete`、`hint-login-new`、`hint-login-delete`、`login-empty-hint`、`login-confirm-delete-label`、`login-confirm-delete-question`

## 期望交互

- **新建**：Browse 模式下按 `Ctrl+N`，进入新建表单（类似 Edit 模式，但字段为空/默认值，保存后追加到 providers 列表）
- **删除**：Browse 模式下按 `Ctrl+D`（或面板底部提示的快捷键），弹出确认对话框，确认后从 providers 列表移除并保存

## 涉及文件

- `peri-tui/src/kit/panels/login.rs` —— Login 面板主组件，`LoginPanelMode` 枚举仅 `Browse`/`Edit`，Browse 键盘处理缺少 `Ctrl+N`/`Ctrl+D`
- `peri-tui/locales/en/main.ftl` / `locales/zh-CN/main.ftl` —— i18n key 已定义但未使用（约 15 个 key）
- `peri-tui/src/acp_client/client.rs:480` —— `update_config()` 已支持完整 PeriConfig 替换，ACP 层无阻塞

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-18 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
