# Plugin 面板 ←/→ 切换 Tab 导致 UI 卡死

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-13

## 问题描述

Plugin 面板 v2 Phase 1 新增了 4 个 Tab（Installed / Discover / Marketplaces / Errors），用 ←/→ 键切换。按下 ← 或 → 后，整个 TUI 界面卡死，不再响应任何键盘输入，必须强制退出进程。

## 症状详情

| 维度 | 观察 |
|------|------|
| 触发操作 | 在 Plugin 面板按下 ← 或 → 键 |
| 触发频率 | 必现（每次 ←/→ 都触发） |
| 表现 | TUI 完全冻结，无任何键盘响应 |
| 影响范围 | 整个 peri-tui 进程卡死 |
| 对比 | ↑/↓ 导航在 Installed Tab 内正常工作；其他面板（Model/Status/Config）的 ←/→ 均正常 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 peri-tui
  2. 打开 Plugin 面板（`/plugin` 命令）
  3. 按 ← 或 → 键
  4. 界面卡死
- **环境**：macOS，任意模型/配置

## 涉及文件

- `peri-tui/src/kit/panels/plugin.rs` —— Plugin 面板组件，←/→ 事件处理逻辑所在

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-13 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
