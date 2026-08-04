# TUI 领域文档 — 总索引

## 领域综述

Peri TUI 的前端渲染与交互系统，基于 ratatui-kit 0.10 框架。负责终端界面渲染、用户输入处理、消息展示、面板管理。核心数据流为 ACP 事件 → acp_notifier → acp_bridge → VIEW_MODELS atom → MessageArea（vm_to_lines_cached + wrap_map_cache 视口裁剪）。

## 核心流程

- **渲染管道**：ACP notification → `kit/acp_notifier.rs`（解码）→ bridge_tx → `kit/acp_bridge.rs`（`spawn_acp_bridge` 维护 `BridgeState`）→ `kit/acp_events/` 子模块（`dispatch_and_notify`，含 agent/compact/render/streaming/subagent/system/tool/turn 8 个子模块）→ 写入 VIEW_MODELS / ACP_STATE / TODO_ITEMS / BG_DISPLAY 等 atom → `kit/message_area/` 直接消费 VIEW_MODELS：`vm_to_lines_cached`（按 VM content_hash 分片缓存）→ `wrap_map_cache` 视口裁剪（二分查找）→ 渲染。v2 事件另有 `kit/v2_bridge.rs` 直连通道（消费 Render/State/Observe 事件映射为 AcpEventData，Phase A 与 ACP 路径双轨运行）
- **输入处理**：crossterm 事件 → ratatui-kit EventScope（`kit/event_handlers.rs` Global/Root 层）→ `kit/focus_router.rs` 焦点优先级分发 → InputArea（键盘/粘贴/历史）→ SUBMIT_TX → submit_consumer → ACP session
- **状态管理**：ratatui-kit hooks（use_state/use_atom） + 全局 AtomStatic 状态（`kit/atoms.rs`：VIEW_MODELS / ACP_STATE / SERVICE_SNAPSHOT / TODO_ITEMS / BG_DISPLAY / RENDER_HEARTBEAT 等）
- **面板系统**：`kit/panels/` 16 个 `#[component]` 面板（`kit/panel_registry.rs` PANELS 单点注册）+ `kit/popups/` 7 类弹窗 + PanelOverlay / PopupOverlay 容器

## 技术方案总结

| 维度 | 选型 |
|------|------|
| UI 框架 | ratatui-kit 0.10.2（React-style components, hooks, element! macro） |
| 终端后端 | crossterm（raw mode, Alt screen, mouse capture, Bracketed Paste；经 `ratatui_kit::crossterm` re-export 使用，ratatui 0.30 / ratatui-kit 传递依赖） |
| 消息渲染 | ratatui-kit-markdown 0.3（parse_markdown + ParsedBlock）→ `kit/message_area/render.rs` vm_to_lines_cached（kit/markdown/ 自行实现 ParsedBlock → Line 转换） |
| 状态共享 | ratatui-kit 内置 Atom（AtomStatic/AtomState，非独立 crate）+ ratatui-kit hooks（组件本地状态） |
| 视口裁剪 | 自持 ScrollPos（usize 偏移，替代 ScrollView）+ wrap_map_cache 二分查找 + 可见行切片 |
| 剪贴板 | arboard 3（spawn 独立线程避免主线程阻塞） |

---

## 子文档索引

| 文件 | 内容概要 |
|------|---------|
| [tui-rendering.md](tui-rendering.md) | 渲染系统：AppShell 根页面、MessageArea 消息区（Welcome/流式/工具卡片/CollapsedGroup/SystemNote）、StatusBar 状态栏、BgTaskArea 后台任务区、相关 Issue 经验 |
| [tui-events.md](tui-events.md) | ACP 事件系统：事件分派管线、acp_bridge/acp_events 桥接、VIEW_MODELS 原子状态、LocalEvents 管道 |
| [tui-input.md](tui-input.md) | 输入系统：InputArea 输入区（多行编辑/历史/@mention/slash/软换行/视口跟随/占位符） |
| [tui-panels.md](tui-panels.md) | 面板系统：PanelOverlay 容器、16 个 Panel（Model/Login/Config/Agent/Hooks/MCP/Plugin/Cron/Tasks/Workflow/Status/Memory/Betas/ThreadBrowser/AskUser/Theme） |
| [tui-popups.md](tui-popups.md) | 弹窗系统：PopupOverlay 容器、HITL 审批、AskUser 问答、OAuth 授权、SetupWizard 向导 |


---

## 面板导航与互斥关系

面板元数据（快捷键、slash command、互斥组、尺寸）单点定义在 `kit/panel_registry.rs` 的 `PANELS` 数组，`PanelKind` 枚举位于 `src/app/panel_types.rs`（16 变体）。

| 组 | 面板 | 语义 |
|----|------|------|
| Settings | Model / Login / Config / Theme | 模型、Provider、运行配置、主题互斥 |
| Agent | Agent / Hooks | Agent 观测与 hook 观测互斥 |
| Tools | MCP / Plugin / Cron / Tasks / Workflow | 外部工具与自动化能力互斥 |
| Info | Status / Memory / Betas | 运行信息、记忆和实验能力互斥 |
| Thread | ThreadBrowser | 会话切换独占 |
| AskUser | AskUser | Agent 提问（自动打开，无快捷键/slash） |

## 快捷键设计规范

快捷键是 TUI 的公共 API，v2 之后必须稳定、可发现、可组合。新增或修改快捷键必须先更新本章节，再落地实现。

### 禁止规则

- **禁止 `Shift+字母`**：终端对大小写和 Shift 修饰的兼容性不稳定，也会与普通输入混淆。
- **禁止裸单字母局部快捷键**：Panel / Popup 内也禁止用 `j/k/q` 做导航或关闭；统一使用方向键导航、`Esc` 关闭。
- **禁止在输入区抢占普通字符**：当 focus 在 InputArea 时，未带 `Ctrl/Alt` 的字符都应进入文本编辑。
- **禁止 PageUp/PageDown**：遵循项目约束（`message_area/scroll.rs` 无翻页分支），滚动使用鼠标滚轮或显式组合键。
- **禁止同一快捷键多处隐式分发**：所有全局快捷键必须能从注册表追踪到唯一能力。

### 全局快捷键（focus_router.rs `classify_global_shortcut`）

| 快捷键 | 能力 |
|--------|------|
| `Ctrl+C` | 三级优先级链：中断（loading 中）→ 双击退出 |
| `Shift+Tab`（BackTab） | 权限模式循环 |
| `Ctrl+O` | diff 显示切换 |
| `Ctrl+T` / macOS `Alt+M` | 循环模型 |
| `Ctrl+Shift+T` / macOS `Alt+Shift+M` | 循环 Provider |
| `Esc` | 关闭 popup / 面板 / mention / slash（双击触发 Rewind popup） |

### 推荐规则

- **面板通过 slash command / 统一入口打开**：输入区键入 `/model` 等并提交 → `panel_for_slash_command` → `open_panel`；AskUser 由 ACP 事件自动打开（`SubmitRequest::OpenPanel`）。`panel_registry.rs` PANELS 的 `shortcut_letter` 字段为预留元数据（当前无消费方，勿依赖 Ctrl+字母 打开面板）。
- **slash command 与面板一一对应**：`/model` `/login` `/agent` `/hooks` `/config` `/threads`（别名 `/history` `/resume` `/his`）`/mcp` `/plugin` `/cron` `/status` `/memory` `/tasks` `/betas` `/workflows` `/theme`。
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

规则：同一个按键事件只允许被最高优先级的激活层消费；消费后不再下传（`focus_router.rs` `active_layer()`）。

## 设计落地注意事项

- 根级 overlay 空态必须返回零尺寸 `Positioned`，避免白屏或挤压布局。
- Panel 空态必须显式使用 `Constraint::Length(0)` 或等价零尺寸约束；不要依赖普通 `View()` 默认 flex 行为。
- 面板打开时应隐藏输入区（`layout.rs` SessionColumn 将 `hidden: panel_open` 传给 InputArea），避免输入焦点与面板事件处理冲突。
- 消息区只处理鼠标滚轮；键盘导航优先留给输入区或当前面板（`focus_router.rs` `message_accepts_key` 仅放行 `Ctrl+↑↓HomeEnd`）。
- 新增面板的落地路径：`src/app/panel_types.rs` 加 `PanelKind` 变体 → `kit/panel_registry.rs` PANELS 注册（快捷键/slash/互斥组/尺寸/render 函数）→ 面板组件放入 `kit/panels/` → 同步 slash command 映射（`panel_for_slash_command`）和本文档。Panel Registry 2.0 单点注册已完成，无需再同步 `panel_overlay.rs` 的分发逻辑（按 `panel_registry::render(kind)` 渲染）。

---

> *本索引文件是 TUI 领域文档的总入口。各子域的详细设计、渲染规范、事件模型、面板/弹窗设计请参见上方子文档索引。*
