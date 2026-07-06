# TUI / 前端领域

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

## Issue 经验附录

### issue_2026-07-03-tui-double-slash-cpu-spike
**摘要:** TUI 输入区输入 // 导致 CPU 持续高负载
**状态:** Verified
**归档日期:** 2026-07-06
**关键词:** render 自激循环, slash popup, atom 写入副作用
**问题本质:** SlashCompletion 组件 render body 中写入 `SLASH_SELECTED_INDEX` atom，在 `slash_active` 从 true→false 过渡时（输入 `//` 触发）与组件卸载生命周期交互，引发级联重渲染——每次 render 都写 atom → 触发下一帧 render → 再次写入，形成无限循环。
**通用模式:** ratatui-kit render 期间只能读 atom，禁止写入——render body 中的 atom 写入会与组件生命周期交互形成 render → state write → render 自激回路。事件处理器负责所有状态变更，render 仅做只读展示。
**架构影响:** 这条原则与「hook 调用顺序」共同构成了 ratatui-kit 状态管理的两条铁律：① hook 调用顺序在任何控制流下保持不变 ② render body 禁止写 atom。
**涉及文件:** peri-tui/src/kit/slash_completion.rs
**CLAUDE.md 链接:** true

### issue_2026-07-06-enter-hello-cpu-spike
**摘要:** TUI 输入 hello 并 Enter 后 CPU 100%
**状态:** Verified
**归档日期:** 2026-07-06
**关键词:** loading spinner 自激, hook state 稳态写入, AgentDone → TurnDone
**问题本质:** (1) `build_footer_lines` 在 loading 稳态下每次 render 都无条件写 hook state（`was_loading`/`load_start`/`spinner_state`），形成 render → state write → render 自激循环。(2) `AcpNotification::AgentDone` 在 `acp_notifier` 中被丢弃，未转发为 `TurnDone`，导致输出结束后 `ACP_STATE.is_loading` 残留，spinner 永不清除。
**通用模式:** (1) hook state 只在状态真正变化时写入，稳态 render 不得触发 state mutation。(2) ACP 通知生命周期中的所有边界事件（包括结束类事件）必须完整转发到桥接层，遗漏会导致状态残留和 UI 不一致。(3) 诊断 CPU 100% 问题先从「哪些 render 帧在写状态」入手——自激循环是最常见的根因。
**架构影响:** `acp_notifier` → `acp_bridge` + `render_bridge` 的转发覆盖度直接影响 TUI 可靠性。任何遗漏的 ACP 通知类型都会表现为 UI 状态卡死或残留。
**涉及文件:** peri-tui/src/kit/message_area.rs, peri-tui/src/kit/acp_notifier.rs
**CLAUDE.md 链接:** true

### issue_2026-07-05-paste-newline-triggers-submit
**摘要:** 输入框粘贴含换行文本时直接触发 Enter 提交
**状态:** Fixed
**归档日期:** 2026-07-06
**关键词:** Bracketed Paste, Event::Paste, 终端模式
**问题本质:** TUI 全屏模式下未启用 `EnableBracketedPaste`，crossterm 无法将粘贴内容合并为单个 `Event::Paste`，而是逐字符生成 `Event::Key(Enter)`——粘贴内容中的换行符被解析为 Enter 提交。
**通用模式:** 终端应用在进入 raw mode 时必须同步启用 Bracketed Paste；退出时反向关闭。这是「终端特性 → crossterm 事件 → 应用行为」链条的基础前置条件，缺失会导致 Key/Paste 事件分派错误。
**涉及文件:** peri-tui/src/kit/entry.rs, peri-tui/src/kit/input_area.rs
**CLAUDE.md 链接:** true

### issue_2026-07-05-mouse-move-cpu-spike
**摘要:** 鼠标晃动导致 CPU 暴涨
**状态:** Fixed
**归档日期:** 2026-07-06
**关键词:** MouseMove 高频事件, ScrollView 事件过滤, Global scope
**问题本质:** message_area 注册了 `EventScope::Global` 的 mouse 事件处理器，每人次 MouseMove 都获取 scroll_state 写锁并执行 auto_scroll.set(false)，高频 MouseMove 事件（终端每秒数百次）放大了锁竞争和 state 写入开销。
**通用模式:** `EventScope::Global` 下消费鼠标事件时，`MouseEventKind::Moved` 必须在入口处提前忽略（返回 Ignored），不走任何 state 读写路径。这是高频事件过滤的通用模式。
**涉及文件:** peri-tui/src/kit/message_area.rs
**CLAUDE.md 链接:** false

### issue_2026-07-05-message-flow-render-sync-freeze
**摘要:** 消息流渲染同步问题——提交后用户输入不显示、loading 卡死、history 恢复异常
**状态:** Fixed
**归档日期:** 2026-07-06
**关键词:** 本地提交 vs ACP 事件, RENDER_CACHE 同步, loading 事实源, history 草稿
**问题本质:** (1) 本地提交(InputArea.submit_text)直接写 VIEW_MODELS atom，但 render_bridge 只在收到 ACP 事件时刷新 RENDER_CACHE——路径不一致导致用户输入不可见。(2) `is_loading` 被 InputArea 和 acp_bridge 多方写入，prompt 失败时无人清 loading，缺少唯一事实源。(3) history 草稿保存逻辑不完善（空串不保存），回到底部时无法恢复空输入。(4) loading 中提交的输入只入队不回显，等 TurnDone 才渲染。
**通用模式:** (1) 所有视图更新必须走统一事件渠道（本地提交也需触发渲染刷新），不可旁路。(2) 状态字段必须有唯一写入者，多方写入导致不一致。(3) 用户即刻操作（粘贴/提交/history）应即时回显，不与后端异步流程耦合。
**架构影响:** 这个 issue 的多重修复奠定了「ACP 事件 → acp_notifier → acp_bridge → render_bridge → RENDER_CACHE → message_area」的单向数据流纪律，任何视图更新不得旁路 RENDER_CACHE。
**涉及文件:** peri-tui/src/kit/input_area.rs, peri-tui/src/kit/input_history.rs, peri-tui/src/kit/submit_consumer.rs, peri-tui/src/kit/render_bridge.rs
**CLAUDE.md 链接:** true

### issue_2026-07-05-message-area-crashes-and-rendering
**摘要:** 消息区多场景崩溃/白屏/滚动异常
**状态:** Fixed
**归档日期:** 2026-07-06
**关键词:** u16 overflow, ScrollView 双重滚动, arboard 剪贴板线程
**问题本质:** (1) 视口坐标计算使用裸 `+` 运算符，在 scroll_y + vis_height 超出 u16 范围时 panic。(2) Paragraph.scroll 与 ScrollView 双重滚动导致白屏。(3) arboard 剪贴板操作在 UI 事件处理器主线程同步阻塞，某些终端环境导致阻塞/崩溃。
**通用模式:** (1) 所有涉及 u16 的坐标偏移计算必须使用 `saturating_add`/`saturating_sub`，禁止裸 `+`/`-`。(2) 单一滚动机制——ScrollView 和 Paragraph 内置滚动不能叠加。(3) 剪贴板写入等可能阻塞的系统调用必须 `std::thread::spawn` 到独立线程。
**涉及文件:** peri-tui/src/kit/message_area.rs, peri-tui/src/kit/render_bridge.rs
**CLAUDE.md 链接:** true

### issue_2026-07-05-enter-clear-hook-mismatch-panic
**摘要:** Enter 提交 & /clear 清屏触发 Hook type mismatch panic 导致 TUI 崩溃
**状态:** Fixed
**归档日期:** 2026-07-06
**关键词:** Hook type mismatch, 条件早退, use_state 调用顺序
**问题本质:** `build_footer_lines` 函数内 `hooks.use_state` 调用在 `if empty` / `if render_sticky` 条件早退分支之后，Enter 或 /clear 后 ViewModels 为空时空消息状态触发条件早退，`build_footer_lines` 中的 hook 调用被跳过，ratatui-kit 按调用顺序索引 hook 时检测到数量不一致 → panic。
**通用模式:** ratatui-kit `#[component]` 函数内部，所有 `hooks.use_*` 调用必须放在任何 `if`/`match`/`return` 之前。hook 调用顺序和数量在每一帧渲染中必须完全一致——这是 ratatui-kit 的硬约束，违反则 panic。
**架构影响:** 与 issue_2026-07-03-tui-double-slash-cpu-spike 共同构成 ratatui-kit 状态管理两条铁律：(1) hook 调用顺序不变 (2) render body 禁止写 atom。
**涉及文件:** peri-tui/src/kit/message_area.rs, peri-tui/src/kit/submit_consumer.rs
**CLAUDE.md 链接:** true

### issue_2026-07-05-hide-empty-threads-in-history-panel
**摘要:** ThreadBrowser 面板应隐藏 message_count 为 0 的空线程
**状态:** Fixed
**归档日期:** 2026-07-06
**关键词:** ThreadBrowser, message_count 过滤, 空线程
**问题本质:** message_count == 0 的空线程（新创建但未发送消息）在 ThreadBrowser 列表中占据空间，造成视觉噪音。
**通用模式:** 列表组件应在数据源层过滤无意义的空项，而非依赖 UI 层选择性渲染。可在数据获取映射处（service_snapshot.rs）或 UI 组件渲染前做过滤。
**涉及文件:** peri-tui/src/kit/panels/thread_browser.rs, peri-tui/src/kit/service_snapshot.rs, peri-tui/src/kit/atoms.rs
**CLAUDE.md 链接:** false
