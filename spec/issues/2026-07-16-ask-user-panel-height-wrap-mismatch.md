# AskUserQuestion 面板：高度固定 18 行导致底部内容截断 + description 提前换行

**状态**：Open
**优先级**：中
**创建日期**：2026-07-16

## 问题描述

AskUserQuestion 内联面板有两个交互的症状：

1. **高度固定 18 行**：面板高度由 `PanelLayout::fixed(60, 18)` → `panel_overlay.rs:54` 的 `Constraint::Length(18)` 写死。当内容（Tab 行 + 分隔线 + 问题文本 + 选项 label/description + hint）超过 18 行时，底部内容被截断不可见。
2. **description 提前换行**：在宽终端（120+ 列）下，选项的 description 文本在面板约一半宽度处就折行，垂直空间被浪费，加剧了高度不足。

## 症状详情

| 项目 | 详情 |
|------|------|
| 触发条件 | 终端宽度 ≥120 列时必现；内容多（多选项含长 description）时更容易触发 |
| 渲染形式 | 内联 Panel（MessageArea 和 InputArea 之间），宽度动态 `Fill(1)`（跟随终端全宽） |
| 面板高度 | `Constraint::Length(18)` 固定，内容超 18 行后底部选项/hint 被截断 |
| description 换行 | 在面板约一半宽度处提前折行（`wrap_width = term_w - 2` 看起来是正确的，疑有其他因素导致） |
| 面板宽度 | 正确的动态宽度（`Fill(1)` = 全终端宽度），宽度层面本身没问题 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 在宽终端（120+ 列）中启动 peri-tui
  2. 与 agent 对话，让 agent 调用 AskUserQuestion 工具，问题的选项中带有 description 文本
  3. 观察内联面板：description 在约一半宽度处提前换行，面板整体过高，底部内容被截断
- **环境**：macOS，终端 ≥120 列

## 涉及文件

- `peri-tui/src/kit/panel_overlay.rs:47-54` —— `render_panel()` 中 height 使用 `panel_constraint(layout.height)`，从 `PanelLayout::fixed(60, 18)` 取 height=18，转为 `Constraint::Length(18)`。面板**宽度**是动态的 `Fill(1)`（没问题），但**高度**是固定的
- `peri-tui/src/kit/panels/ask_user.rs:47-48` —— `wrap_width = term_w - 2`，逻辑上应跟随终端宽度。description 实际折行位置与 `wrap_width` 计算值不符，需要进一步排查（可能是初始渲染时 `use_terminal_size()` 返回 0 导致 `wrap_width = 40`，或 ratatui ScrollView 内部宽度裁剪导致）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-16 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加）
