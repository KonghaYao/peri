# TUI 领域文档 — 总索引

## 领域综述

Peri TUI 的前端渲染与交互系统，基于 ratatui-kit 框架。负责终端界面渲染、用户输入处理、消息展示、面板管理。核心数据流为 ACP 事件 → acp_bridge → VIEW_MODELS atom → render_bridge → RENDER_CACHE → message_area。

## 核心流程

- **渲染管道**：ACP 事件 → acp_notifier → acp_bridge → dispatch_and_notify → VIEW_MODELS atom → render_bridge（预计算 Line + wrap_map）→ RENDER_CACHE atom → message_area（ScrollView + 视口裁剪）
- **输入处理**：crossterm 事件 → ratatui-kit EventScope → InputArea（键盘/粘贴/历史）→ submit_consumer → ACP session
- **状态管理**：ratatui-kit hooks（use_state/use_atom） + 全局 atom 状态（VIEW_MODELS/RENDER_CACHE/ACP_STATE）
- **面板系统**：ThreadBrowser / ModelPanel / SlashCompletion / 各 Popup 组件

## 技术方案总结

| 维度 | 选型 |
|------|------|
| UI 框架 | ratatui-kit（React-style components, hooks, element! macro） |
| 终端后端 | crossterm（raw mode, Alt screen, mouse capture, Bracketed Paste） |
| 消息渲染 | pulldown-cmark → ratatui Line/Span（view_render.rs + render_bridge） |
| 状态共享 | atom（跨组件原子状态）+ ratatui-kit hooks（组件本地状态） |
| 视口裁剪 | ScrollView + wrap_map 二分查找 + 可见行切片 |
| 剪贴板 | arboard（spawn 独立线程避免主线程阻塞） |

---

## 子文档索引

| 文件 | 内容概要 |
|------|---------|
| [tui-rendering.md](tui-rendering.md) | 渲染系统：AppShell 根页面、MessageArea 消息区（Welcome/流式/工具卡片/CollapsedGroup/SystemNote）、StatusBar 状态栏、BgTaskArea 后台任务区 |
| [tui-events.md](tui-events.md) | ACP 事件系统：事件分派管线、acp_bridge/acp_events 桥接、VIEW_MODELS 原子状态、LocalEvents 管道 |
| [tui-input.md](tui-input.md) | 输入系统：InputArea 输入区（多行编辑/历史/@mention/slash/软换行/视口跟随/占位符） |
| [tui-panels.md](tui-panels.md) | 面板系统：PanelOverlay 容器、16 个 Panel 设计（Model/Login/Config/Agent/Hooks/MCP/Plugin/Cron/Tasks/Workflow/Status/Memory/Betas/ThreadBrowser） |
| [tui-popups.md](tui-popups.md) | 弹窗系统：PopupOverlay 容器、HITL 审批、AskUser 问答、OAuth 授权、SetupWizard 向导 |


---

## 面板导航与互斥关系

| 组 | 面板 | 语义 |
|----|------|------|
| Settings | Model / Login / Config | 模型、Provider、运行配置互斥 |
| Agent | Agent / Hooks | Agent 观测与 hook 观测互斥 |
| Tools | MCP / Plugin / Cron / Tasks / Workflow | 外部工具与自动化能力互斥 |
| Info | Status / Memory / Betas | 运行信息、记忆和实验能力互斥 |
| Thread | ThreadBrowser | 会话切换独占 |

## 快捷键设计规范

快捷键是 TUI 的公共 API，v2 之后必须稳定、可发现、可组合。新增或修改快捷键必须先更新本章节，再落地实现。

### 禁止规则

- **禁止 `Shift+字母`**：终端对大小写和 Shift 修饰的兼容性不稳定，也会与普通输入混淆。
- **禁止裸单字母局部快捷键**：Panel / Popup 内也禁止用 `j/k/q` 做导航或关闭；统一使用方向键导航、`Esc` 关闭。
- **禁止在输入区抢占普通字符**：当 focus 在 InputArea 时，未带 `Ctrl/Alt` 的字符都应进入文本编辑。
- **禁止 PageUp/PageDown**：遵循项目约束，滚动使用鼠标滚轮或显式组合键。
- **禁止同一快捷键多处隐式分发**：所有全局快捷键必须能从注册表追踪到唯一能力。

### 推荐规则

- **面板打开不占用专用全局快捷键**：Model、Agent、Plugin 等 panel 只能通过 slash command、command palette 或统一入口打开；禁止为单个 panel 分配 `Ctrl+字母`。
- **局部导航使用方向键**：Panel / Popup 已获得 focus 时，使用 `↑/↓/←/→` 导航；不使用 `j/k/h/l`。
- **确认/取消语义固定**：`Enter` 确认，`Esc` 取消或关闭当前最高优先级 UI。
- **Tab 仅用于候选项/焦点遍历**：不得用于提交危险动作。
- **所有快捷键必须以 `detail` 形式展示**：StatusBar hints、面板底部提示和设计图说明统一使用这个格式。

### 焦点优先级

```text
PopupOverlay
  > InlineCompletion（@mention / slash）
  > PanelOverlay
  > InputArea
  > MessageArea
```

规则：同一个按键事件只允许被最高优先级的激活层消费；消费后不再下传。

## 设计落地注意事项

- 根级 overlay 空态必须返回零尺寸 `Positioned`，避免白屏或挤压布局。
- Panel 空态必须显式使用 `Constraint::Length(0)` 或等价零尺寸约束；不要依赖普通 `View()` 默认 flex 行为。
- 面板打开时应隐藏输入区，避免输入焦点与面板事件处理冲突。
- 消息区只处理鼠标滚轮；键盘导航优先留给输入区或当前面板。
- 新增 PanelKind 的目标形态是 Panel Registry 2.0 单点注册；在迁移完成前，临时实现需同步现有 `panel_types.rs`、`panel_registry.rs`、`panel_overlay.rs`、slash command 和本文档。

---

> *本索引文件是 TUI 领域文档的总入口。各子域的详细设计、渲染规范、事件模型、面板/弹窗设计请参见上方子文档索引。*
