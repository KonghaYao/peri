# Setup 向导 Form 不支持粘贴，Login 面板不支持编辑 Provider

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-15

## 问题描述

两个面板的输入交互不完整：(1) Setup Wizard 的 Form 编辑模式中，文本框（API Key、Base URL 等）无法粘贴，用户只能逐字符输入；(2) Login 面板仅提供只读列表浏览 + Enter 激活，无 New/Edit/Delete 操作入口，用户想编辑已有 Provider 时无处下手。

## 症状详情

### 现象 1：Setup Wizard Form 编辑模式不支持粘贴

| 维度 | 表现 |
|------|------|
| 场景 | 在 Setup Wizard 的 Choose 步骤选择一个 source 进入 Form → 按 `Ctrl+E` 进入某个 provider 的编辑模式 → 在文本框（如 API Key）尝试粘贴 |
| 期望 | 粘贴（Cmd+V / Ctrl+V）将剪贴板内容插入文本框光标位置 |
| 实际 | 粘贴无反应，文本内容不变 |

**技术背景**：`handle_wizard_event`（`peri-tui/src/kit/setup_wizard.rs:738-740`）通过 `let Event::Key(key) = event` 过滤了所有非按键事件，crossterm 的 `Event::Paste` 被直接丢弃。下游 `handle_text_input`（:957-1049）也只处理 `Char`、`Backspace`、`Delete`、`Left/Right`、`Home/End`、`Ctrl+W`，没有 paste 逻辑。TUI 入口已启用 `BracketedPaste`（`entry.rs:313`），所以系统层面 paste 事件是能发出的——只是向导没消费它。

### 现象 2：Login 面板不支持编辑 Provider

| 维度 | 表现 |
|------|------|
| 场景 | 打开 `/login` 面板 → 看到一个已配置的 Provider → 想编辑它的 API Key、Base URL 或模型别名 |
| 期望 | 有入口进入编辑模式，直接修改 Provider 字段 |
| 实际 | 面板只显示 `↑/↓::navigate  Enter::select  Esc::close`，无编辑、新建、删除入口 |

**技术背景**：`peri-tui/src/kit/panels/login.rs:7-8` 注释明确说明"简化设计：只读列表 + Enter 激活；不提供 New/Edit/Delete UI（这些操作通过 Setup Wizard 完成）"。问题是：
1. Login 面板没有跳转 Setup Wizard 的入口（如 `Ctrl+E` 编辑当前 provider）
2. 空态提示只建议"Run setup wizard or edit ~/.peri/settings.json"（手动编辑 JSON），无快捷启动 wizard 的方式

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 现象 1：启动 Peri → 触发 Setup Wizard → 进入 Form 编辑模式 → 复制一段 API Key → 尝试粘贴到输入框
  2. 现象 2：启动 Peri（已有 provider）→ 输入 `/login` → 看到 provider 列表 → 无编辑入口

## 涉及文件

- `peri-tui/src/kit/setup_wizard.rs` —— 向导事件处理（738-756 行的 `handle_wizard_event`、957-1049 行的 `handle_text_input`），应增加 paste 支持
- `peri-tui/src/kit/panels/login.rs` —— Login 面板，当前纯只读，需增加编辑/新建入口（或至少增加跳转 Setup Wizard 的快捷键）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-15 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
