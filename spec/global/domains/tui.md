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

### issue_2026-07-08-tui-drop-acp-messageid-boundary
**摘要:** TUI 丢弃 ACP agent_message_chunk 的 messageId，消息边界靠推断而非协议字段
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** ACP messageId, 消息边界推断, ContentChunk, session/update
**问题本质:** TUI 在解码 ACP session/update 事件时丢弃了协议自带的 `messageId`，靠 ContentSegment 变体切换推断消息边界——推断规则比协议字段脆弱，ACPP 事件到达顺序异常时失效。
**通用模式:** 消费 ACP 协议字段时应逐字段透传而非选择性提取——协议层已有的结构信息（messageId）不应在 TUI 层重新推断。协议字段是唯一事实源，推断是冗余逻辑且必然引入偏离。
**涉及文件:** peri-tui/src/kit/stream_data.rs, peri-tui/src/kit/acp_notifier.rs, peri-tui/src/kit/acp_types.rs
**CLAUDE.md 链接:** false

### issue_2026-07-09-history-session-switch-loading-freeze
**摘要:** History 面板切换 session 后 loading 永久卡死，界面完全无响应
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** _meta 序列化, is_loading 卡死, session replay, periReplay
**问题本质:** ACP SDK 将 `meta` 序列化为 `_meta`（含下划线），但 `acp_notifier.rs` 的 `is_session_replay` 提取链只用 `"meta"`（无下划线），导致 replay 事件的 `periReplay=true` 标记从未被检测到。replay 事件走流式路径（ToolStarted/TextChunk 设 `phase=PromptRunning`），无 TurnDone 兜底 → loading 永久卡 true。
**通用模式:** 消费第三方 SDK 序列化字段时，必须验证实际 JSON key——`#[serde(rename = "_meta")]` 等注解会改变 wire format。不要假设 struct 字段名等于 JSON key。对关键分支标记做四级 fallback 是防御性编程的合理实践。
**涉及文件:** peri-tui/src/kit/acp_notifier.rs, peri-tui/src/kit/acp_events.rs
**CLAUDE.md 链接:** true

### issue_2026-07-09-message-area-periodic-white-flash-streaming
**摘要:** 消息区在 agent 流式回复中周期性闪白（每 2-5 秒）
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** render↔effect 自激回路, use_effect, 吸底滚动, scroll_to_bottom
**问题本质:** `use_effect` 的 deps `(entries_len, raw_ch)` 在流式期间每个 chunk 都递增 → effect 高频触发 → `scroll_to_bottom()` 写入 `ScrollViewState` atom → ratatui-kit 检测变更 → 重渲染 → 重新计算 `total_visual_rows` → `scroll_y` 仍小于新 `max_scroll` → 下一帧 effect 再次触发。形成 render → effect → state write → render 紧耦合环路，每秒几十帧。
**通用模式:** `use_effect` 中写入 atom 与 render body 写 atom 等效危险——同样形成自激回路。增量门控（`last_scrolled_at` + 底 guard）是打破环路的有效手段：先在状态中判断"是否真的需要写入"，仅在值变化时触发 atom 变更。
**技术决策:** 用 `last_scrolled_at` 增量门控代替距离阈值门控——已在底部的跳写 guard（`scroll_y >= max_scroll`）远大于内容增长量，大幅减少 atom 写入次数。
**涉及文件:** peri-tui/src/kit/message_area.rs
**CLAUDE.md 链接:** true

### issue_2026-07-06-message-area-copy-complex-content-crash
**摘要:** Message Area 复制操作导致 TUI 崩溃/卡死
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** render body 写 atom, 自激回路, COPY_MESSAGE_UNTIL, arboard 线程
**问题本质:** `status_bar.rs` 在 render body 中写入 `COPY_MESSAGE_UNTIL` atom（`else { *copy_until.write() = None }`），复制后 2s 过期时 status_bar 检测到 `now >= until` 进入 else 分支 → 写 atom → 触发 wake → render → 再次检测 → 再次写入 → 自激回路。这是第三次出现完全相同的模式（前两次：issue_2026-07-03 double-slash、issue_2026-07-06 enter-hello）。
**通用模式:** ratatui-kit render body 中禁止写 atom——这是铁律。render 期间任何 atom 写入都会与组件生命周期交互形成 render → state write → render 自激回路。此模式已三次出现，应成为 ratatui-kit 编码的零容忍规则。
**技术决策:** 渲染层只读判断 `now < until` 决定是否显示提示，原子清理留给下次 `mark_copy_message` 自然覆盖。arboard 剪贴板调用必须 `std::thread::spawn` 到独立线程。
**涉及文件:** peri-tui/src/kit/status_bar.rs, peri-tui/src/kit/message_area.rs
**CLAUDE.md 链接:** true

### issue_2026-07-04-message-area-scrollview-steals-input
**摘要:** 主输入框无法输入——MessageArea ScrollView 事件处理器消费所有键盘事件
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** ScrollView 事件拦截, EventScope, active=false, 键盘事件路由
**问题本质:** MessageArea 的 Global/High 事件处理器对所有键盘事件返回 Consumed（初版 bug），后修复 Global/High 层后 ScrollView 内置 Current/Normal handler 仍消费 j/k/h/l/↑/↓/PageUp/PageDown/Home/End。两层拦截叠加导致 InputArea 收不到这些键。
**通用模式:** ratatui-kit 事件分发是多层多优先级的：Global/High > Global/Normal > Current/High > Current/Normal。每层都可能独立拦截事件。排查"事件去哪了"时需要逐层验证，不能只修一层。ScrollView 传 `active: false` 可关闭其内置键盘/鼠标 handler，滚动由外部显式 handler 接管。
**涉及文件:** peri-tui/src/kit/message_area.rs, ratatui-kit scroll_view
**CLAUDE.md 链接:** true

### issue_2026-07-09-agent-prefix-triggers-command-without-slash
**摘要:** 输入 "agent " 开头触发 OpenPanel 命令，无需 / 前缀
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** trim_start_matches, slash 前缀检查, 命令路由, panel_for_slash_command
**问题本质:** `panel_for_slash_command` 使用 `trim_start_matches('/')` 做归一化——无 `/` 前缀时是 no-op，导致 `"agent"` 直接匹配 `slash_command: "agent"`。`parse_submit_request` 未做 `/` 前缀检查就调用此函数。
**通用模式:** 字符串"归一化"函数（`trim_start_matches`、`trim_end_matches`）在无目标字符时是 no-op 而非报错——这是静默陷阱。调用此类函数的代码必须在调用前验证前置条件（需要 `/` 前缀则先 `starts_with('/')`），或将归一化逻辑下沉到函数签名中。
**涉及文件:** peri-tui/src/kit/submit_request.rs, peri-tui/src/kit/panel_registry.rs
**CLAUDE.md 链接:** false

### issue_2026-07-07-inputarea-mouse-click-cursor-positioning
**摘要:** InputArea 鼠标点击光标快速定位——功能缺失
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** Arc 重建, AreaTracker 值拷贝, MouseDown cursor, ratatui-kit hook
**问题本质:** `AreaTracker` hook 的 `area_rect` 每帧通过 `Arc::new(Mutex::new(None))` 重新创建，但 hook 的 `pre_component_draw` 写入的是第一帧的原始 Arc。从第 2 帧开始 mouse handler 读到的新 Arc 永远为 None。
**通用模式:** ratatui-kit hooks 中共享给事件闭包的状态必须通过值拷贝（`Copy` 类型或 `clone()`）传递，不能依赖 `Arc<Mutex<>>` 跨帧共享——hooks 每帧重新创建 `Arc`，旧引用失效。仿照 `MsgAreaTracker` 模式：每帧从 hook 取出副本后释放 `&mut hooks` 借用，闭包按值捕获副本。
**涉及文件:** peri-tui/src/kit/input_area.rs, peri-tui/src/kit/message_area.rs
**CLAUDE.md 链接:** false

### issue_2026-07-08-history-replay-missing-tool-interactions
**摘要:** History 面板恢复的对话历史缺少工具调用和工具结果
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** session replay, tool_call 重放, _meta 序列化, BaseMessage 存储格式
**问题本质:** 旧的 replay 路径使用 `ReplayAssistantBubble`/`ReplayUserBubble` 硬编码旁路，不经过 `current_turn` 流式路径，工具调用事件（`tool_call`/`tool_call_update`）在 replay 期间被吞掉。修复涉及 6 个坑：`_meta` 序列化 key 不匹配、`BaseMessage::Tool` 是 `Text(String)` 非 `Blocks`、AI 消息有两个工具调用来源需去重、空工具输出被跳过、`content_hash` 不一致、`agent_thought_chunk` 缺少 replay 处理。
**通用模式:** (1) replay 路径必须复用正常流式路径的数据结构（`CommittedAssistantText`/`ReplayToolStarted`/`ReplayToolEnded`），不要发明"专用于 replay"的 DTO 变体——那会绕过后端逻辑。(2) 修复跨多个数据格式差异的 bug 时需逐层验证：JSON wire format → Rust 反序列化 → TUI 类型转换 → UI 渲染。(3) ACP 层 session/load 数据已完善时，TUI 层只应做格式适配，不应做语义变换。
**涉及文件:** peri-tui/src/kit/acp_types.rs, peri-tui/src/kit/acp_events.rs, peri-tui/src/kit/acp_notifier.rs, peri-acp/src/dispatch/session_replay.rs
**CLAUDE.md 链接:** true

### issue_2026-07-05-input-unicode-cursor-misalignment
**摘要:** 输入框 Unicode 字符删除时光标估算错误，出现多个白色光标残影
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** CJK 光标, unicode-width, CjkGhostFix, AlwaysUpdate, 续接 cell
**问题本质:** (1) `render_multiline_with_cursor` 以字符索引而非显示列定位光标，CJK 字符 1 char = 2 显示列导致光标错位。(2) CJK 双宽字符的第 2 个 cell（续接 cell）在 ratatui diff 中总是 `reset()` 到 `Cell::EMPTY`，两帧续接 cell 相同时 diff 不发送 SGR → 第 1 个 cell bg 的视觉扩展残留在第 2 个 cell 中。
**通用模式:** (1) 终端 UI 中所有光标/坐标计算必须使用 `unicode_width` 做显示列换算，字符索引不等于显示列。(2) ratatui 的 cell diff 优化对 CJK 续接 cell 存在遗漏——两个相同的 `Cell::EMPTY` 不会触发重绘，导致前帧的视觉残留。修复方案是通过 `post_component_draw` hook 标记续接 cell 为 `AlwaysUpdate`，强制 diff 发送 SGR 49（reset bg），在不改变 bg/fg 值的前提下清除残留。
**涉及文件:** peri-widgets/src/textarea/render.rs, peri-tui/src/kit/input_area.rs
**CLAUDE.md 链接:** false

### issue_2026-07-06-panels-selection-no-scroll-follow
**摘要:** 面板选中项超出可见行后看不到（缺 scroll 跟随）
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** scroll_start_for_selected, 选中项跟随, 面板滚动, ratatui-kit Select
**问题本质:** 侧栏面板列表项 Up/Down 切换选中项时，选中行移出可见区域后 `>` 光标消失，用户无法判断当前位置。ratatui-kit `Select` 组件有硬编码边框和整行 highlight，视觉风格不符合需求。
**通用模式:** 当框架组件（如 ratatui-kit `Select`）在视觉风格上不兼容，但数据流逻辑（`selected` index + scroll offset）是普适的时，提取纯算法辅助函数（`scroll_start_for_selected(selected, item_count, visible_items) -> usize`）在各面板复用，保留原有渲染逻辑不变。算法复用的成本远低于组件替换。
**涉及文件:** peri-tui/src/kit/list_nav.rs, peri-tui/src/kit/panels/ (6 panels)
**CLAUDE.md 链接:** false

### issue_2026-07-15-terminal-rapid-shrink-width-crash
**摘要:** 快速缩小终端宽度到极小值时程序直接退出崩溃
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** Buffer越界, 表格渲染, 列宽redistribution, 极窄终端
**问题本质:** table_data_to_lines 中 buffer 宽度与列宽不匹配——compute_table_col_widths 在 available=0 时返回 [1,1,...]，但 buffer 被 max_width 钳制而列宽 wa 未被同步缩减，render 按原始列宽写 buffer → 越界 panic。
**通用模式:** 所有渲染路径必须在极端输入（宽度 0/1）下做防御性检查。buffer 分配宽度与内容写入宽度必须严格一致，涉及列宽 redistribution 的代码需要通过二次归一化确保 sum(column_widths) ≤ buffer_width。
**涉及文件:** peri-tui/src/kit/markdown/table.rs, peri-tui/src/kit/message_area/render.rs
**CLAUDE.md 链接:** false

### issue_2026-07-11-message-area-mouse-selection-regression
**摘要:** 消息区鼠标拖拽选中复制功能因重构回归 + 鼠标拖拽 CPU 暴涨
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** 鼠标拖拽, ratatui-kit迁移, 文本选中, CPU暴涨, Drag事件过滤
**问题本质:** commit 3bfb9fff 删除了 render_bridge 管线，text_selection 的数据结构依赖被切断。同时鼠标事件处理器仅过滤 Moved，Drag(Left) 穿透后触发组件重渲染 → 每帧 clone 数千行 Line → CPU 飙升。
**通用模式:** (1) 大规模重构时必须明确标记依赖关系，迁移文档中的"后续补回"需有追踪机制。(2) 所有 Drag 类鼠标事件必须在入口处及早 return Ignored，否则高频 Drag 事件触发 state 读写 → 重渲染 → CPU 暴涨。(3) 使用 write_no_update 替代 write 避免 render body 中的 state 写入形成自激回路。
**涉及文件:** peri-tui/src/kit/message_area.rs, peri-tui/src/kit/text_selection.rs, peri-tui/src/kit/atoms.rs
**CLAUDE.md 链接:** true

### issue_2026-07-15-setup-wizard-no-paste-login-no-edit
**摘要:** Setup 向导 Form 不支持粘贴，Login 面板不支持编辑 Provider
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** Paste事件, BracketedPaste, 向导表单, Event过滤
**问题本质:** handle_wizard_event 用 `let Event::Key(key) = event` 过滤了所有非按键事件，crossterm 的 Event::Paste 被直接丢弃。TUI 入口已启用 BracketedPaste，但向导未消费 Paste 事件。
**通用模式:** 文本输入组件必须消费 Event::Paste（当 BracketedPaste 启用时）。事件过滤不应使用 `if let Event::Key` 独占模式——应同时匹配 Paste 事件。这是 BracketedPaste 基础设施就绪后应用层消费的通用模式。
**涉及文件:** peri-tui/src/kit/setup_wizard.rs, peri-tui/src/kit/panels/login.rs
**CLAUDE.md 链接:** true

### issue_2026-07-13-config-language-switch-no-effect
**摘要:** Config 面板语言切换无效，始终显示英文
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** i18n, 语言切换, bundle key不匹配, 错误静默丢弃
**问题本质:** LANGUAGE_OPTS 使用 "zh"，但 i18n bundle 注册的 key 是 "zh-CN"，switch() 返回 Err 被 `let _ =` 丢弃，LANG_VERSION 递增但实际 bundle 未切换。
**通用模式:** i18n bundle key 与 UI 选项值必须严格一致。所有可能失败的 Result 必须消费（match/log/返回），禁止 `let _ =` 丢弃——尤其当后续逻辑依赖其副作用时。
**涉及文件:** peri-tui/src/kit/panels/config.rs, peri-tui/src/i18n/mod.rs
**CLAUDE.md 链接:** false

### issue_2026-07-16-system-note-cache-warning-position-wrong
**摘要:** Cache 命中率警告 SystemNote 在消息流中位置错位
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** SystemNote, committed, current_turn flush, 消息时序, flush-then-push
**问题本质:** SystemNotification 直接 push 到 committed（持久化队列），绕过 current_turn 的 TurnSegment 分段系统。push_view_models 按 committed + current_turn.view_models() 拼接，SystemNote 永远排在所有 current_turn 内容之前。
**通用模式:** 所有需要时序定位的消息应先 flush current_turn → committed 再 push 新消息。遵循 "flush-then-push" 模式（与 BgCallbackBubble 一致）。直接 push committed 的消息无法与 current_turn 内容正确排序。
**涉及文件:** peri-tui/src/kit/acp_events.rs
**CLAUDE.md 链接:** true

### issue_2026-07-11-final-ai-reply-disappear-after-turn-done
**摘要:** 工具调用吞掉前面 AI 消息文本的显示
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** HASH_DIFF_APPEND, assistant bubble, 文本截断, VmKey::Item
**问题本质:** render_bridge 的 HASH_DIFF_APPEND 策略按 entry 数量截断，同一 assistant bubble 可产出 2+ 个 RenderedEntry（reasoning+text），按 entry 数量截断会丢弃同 item 内的后续 entry。
**通用模式:** 截断/分页逻辑必须在逻辑边界（如 item/fragment）而非物理边界（如 entry 数量）进行。（render_bridge 已随重构删除，本认知保留为历史参考）
**涉及文件:** peri-tui/src/kit/render_bridge.rs
**CLAUDE.md 链接:** false

### issue_2026-07-13-config-panel-save-silently-discards-errors
**摘要:** Config 面板保存失败时无错误提示、UI 仍显示修改成功
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** 保存失败, 静默丢弃, NOTIFICATION, 错误提示
**问题本质:** 5 处 config::save() 用 `let _ =` 丢弃错误——文件权限、磁盘满等场景下保存失败但 UI 无任何反馈，重启后配置回退。
**通用模式:** 所有用户发起的 I/O 操作必须有结果反馈：成功→通知，失败→警告+错误详情。NOTIFICATION atom 是标准反馈通道。
**涉及文件:** peri-tui/src/kit/panels/config.rs, peri-tui/src/kit/panels/login.rs
**CLAUDE.md 链接:** false

### issue_2026-07-12-message-area-scrollbar-not-reaching-bottom
**摘要:** 消息区滚动不到最末尾 + 宽度变化后滚动失效
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** 滚动条, vis_width对齐, line_count估算, 智能跟随, resize clamp
**问题本质:** vis_width 与实际 Paragraph 渲染宽度不一致导致 content_length 估算偏大；is_loading 分支无条件 scroll_to_bottom() 忽略用户主动上滚。
**通用模式:** (1) 任何宽度估算必须与实际渲染宽度严格一致。(2) 智能跟随需距离阈值门控，loading 和非 loading 状态共用同一阈值逻辑。
**涉及文件:** peri-tui/src/kit/message_area/mod.rs, peri-tui/src/kit/message_area/scroll.rs
**CLAUDE.md 链接:** false

### issue_2026-07-12-message-area-copy-unicode-misalignment
**摘要:** 消息区拖拽复制时 Unicode 字符后段错位
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** Unicode宽度, CJK偏移, wrap_byte_starts, visual_col, Paragraph::wrap
**问题本质:** 折行偏移公式用 width×k 估算，未对齐 ratatui 实际 WordWrapper 行为——CJK 被当作无空格长 word，超宽不拆分，每视觉行占列数不固定。
**通用模式:** Unicode 文本的视觉坐标→逻辑偏移转换必须使用与渲染引擎一致的计算方式。ratatui 场景下用 Paragraph::wrap 渲染到 offscreen Buffer 后按 cell 流匹配是唯一 100% 复刻实际 wrap 行为的方法。双宽字符要区分左半/右半边界。
**涉及文件:** peri-tui/src/kit/message_area/selection.rs, peri-tui/src/kit/text_selection.rs
**CLAUDE.md 链接:** false

### issue_2026-07-15-markdown-table-raw-text-streaming
**摘要:** Markdown 表格流式输出时显示为原始 pipe 格式
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** 增量缓存, pulldown-cmark, 表格header, can_reuse, block类型变更
**问题本质:** 流式输出时表格 header 先到→pulldown-cmark 解析为 Paragraph→增量缓存持久化。分隔符+数据行到达后 block 从 Paragraph 翻转为 Table，但 can_reuse 仅检查 block 数量（1≤1），未检测到类型变更→Table 被跳过，旧 Paragraph 中的 pipe 文本残留。
**通用模式:** 增量缓存的 can_reuse 条件必须覆盖 block 类型变更场景——当输入文本前缀可能导致 pulldown-cmark 重新解析出不同 block 类型时，缓存必须失效全量重跑。对"可能是某种特殊语法的段落"标记潜在类型，纳入缓存失效条件。
**涉及文件:** peri-tui/src/kit/markdown/convert.rs, peri-tui/src/kit/markdown/mod.rs
**CLAUDE.md 链接:** true

### issue_2026-07-10-brewed-summary-missing-in-empty-state
**摘要:** MessageArea 空态时不显示 Brewed 总结行
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** Brewed总结, 单帧延迟, footer耦合, 空态Welcome
**问题本质:** has_summary 检查在 mutation block 之前→loading 结束那帧读到旧值早退；footer 内嵌 ScrollView 内容中→空态 Welcome 早退分支丢弃 footer。
**通用模式:** 状态读取应在 mutation 之后而非之前，避免单帧延迟。footer 应从 ScrollView 内容中解耦——空态时独立渲染 footer。
**涉及文件:** peri-tui/src/kit/message_area.rs
**CLAUDE.md 链接:** false

### issue_2026-07-13-model-login-panel-persistence-lost
**摘要:** Model/Login 面板切换后重启配置丢失 + 状态栏更新延迟
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** 持久化丢失, config::save(), 状态栏延迟, SERVICE_SNAPSHOT, PROVIDER_LIST
**问题本质:** model 面板切换后未调用 config::save()，login 面板过快同步写导致 PROVIDER_LIST.is_active 未同步更新。
**通用模式:** 任何修改全局配置的面板操作后必须同时：① 更新内存 config handle ② 调 save() 持久化 ③ 更新 SERVICE_SNAPSHOT ④ 推送 update_config 到 agent。
**涉及文件:** peri-tui/src/kit/panels/model.rs, peri-tui/src/kit/panels/login.rs
**CLAUDE.md 链接:** false

### issue_2026-07-11-cancel-no-rollback-no-restore
**摘要:** Ctrl+C 取消后未回滚用户消息、未恢复文本到输入框
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** Ctrl+C回滚, TurnInterrupted, 零产出, 文本恢复, v1回归
**问题本质:** kit 单路径迁移丢失 v1 的回滚逻辑。TurnInterrupted 处理器缺少零产出分支：移除用户气泡 + 恢复文本到输入框。
**通用模式:** 架构迁移需做功能回归矩阵——迁移文档中的"后续补回"标记不能替代回归测试。取消操作的零产出回滚是标准 UX 模式：无 AI 产出时 undo 用户操作。
**涉及文件:** peri-tui/src/kit/acp_events.rs, peri-tui/src/kit/atoms.rs, peri-tui/src/kit/input_area.rs
**CLAUDE.md 链接:** false

### issue_2026-07-16-model-login-switch-not-effective-until-restart
**摘要:** /login 和 /model 面板切换后不立即生效，需重启
**状态:** Fixed
**归档日期:** 2026-07-17
**关键词:** 配置推送, update_config, AgentPool, WorkflowAgentContext, provider共享
**问题本质:** (1) 面板写了 PERI_CONFIG_HANDLE 但 agent 的 ctx.provider 是独立 RwLock，pool 缓存未失效。(2) WorkflowAgentContext.provider 是裸值复制，非共享 Arc，配置变更后不可见。
**通用模式:** 配置变更必须同时：更新持久化存储 → client.update_config() 推送 → invalidate agent_pool 缓存 → 重建 provider。对 workflow/SubAgent，provider 必须共享 Arc<RwLock<>> 而非裸值复制。
**涉及文件:** peri-tui/src/kit/panels/login.rs, peri-tui/src/kit/panels/model.rs, peri-acp/src/agent/workflow_agent.rs, peri-tui/src/acp_server/, peri-tui/src/acp_stdio/
**CLAUDE.md 链接:** true

### issue_2026-07-13-statusbar-context-cache-display-regression
**摘要:** 状态栏上下文消耗显示 + 消息流缓存命中率警告，ratatui-kit 迁移后全部丢失
**状态:** Done
**归档日期:** 2026-07-18
**关键词:** ratatui-kit 迁移, 功能回归, 状态栏, 缓存命中率
**问题本质:** ratatui-kit 迁移时状态栏直接读取 session_tracker 的旧代码被替换，新架构未接入上下文使用率和缓存命中率的数据通道
**通用模式:** UI 框架迁移需要系统性的功能回归清单——每个旧实现的 UI 元素需确认在新架构中有对应数据通道和渲染路径
**涉及文件:** peri-tui/src/kit/status_bar.rs, peri-tui/src/kit/acp_notifier.rs
**CLAUDE.md 链接:** true

### issue_2026-07-13-submit-no-scroll-to-bottom
**摘要:** 用户发送 prompt 后消息区不自动跳转到最底部
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** scroll_to_bottom, UserBubble, 提交交互
**问题本质:** 提交后 `is_loading=true` 但 VIEW_MODELS 尚未包含 UserBubble（RPC 飞行中），use_effect 触发时 items_len 未变，proximity guard 阻止滚底
**通用模式:** 用户主动操作（提交）应触发"强制滚底"而非依赖 proximity 检测；时序问题需要明确的"强制滚底"信号而非间接条件
**涉及文件:** peri-tui/src/kit/message_area/scroll.rs, peri-tui/src/kit/submit_consumer.rs
**CLAUDE.md 链接:** false

### issue_2026-07-18-duplicate-streaming-text-and-tool-cards
**摘要:** 流式输出时文本和工具调用卡片重复显示
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** 流式输出, 双轨扇出, 重复渲染, forwarder
**问题本质:** render 事件→ExecutorEvent 映射存在双轨扇出（两个路径同时 push 同一内容到 VIEW_MODELS）
**通用模式:** 事件扇出时需要确保每条内容只走一个路径进入最终数据模型——双轨扇出必然导致重复
**涉及文件:** peri-acp/src/event/forwarder.rs, peri-tui/src/kit/acp_events.rs
**CLAUDE.md 链接:** false

### issue_2026-07-07-ask-user-popup-never-appears
**摘要:** AskUserQuestion 弹窗不出现，agent 卡死
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** AskUserQuestion, 弹窗, HITL, TUI 交互
**问题本质:** agent 调用 AskUserQuestion 后 TUI 未弹出问答窗口，agent 陷入等待
**通用模式:** 无可提炼认知——TUI 交互 bug，修复后功能正常
**涉及文件:** peri-tui/src/kit/panels/ask_user.rs
**CLAUDE.md 链接:** false

### issue_2026-07-09-textarea-no-soft-wrap
**摘要:** textarea 缺少软换行（soft wrapping），长行被截断且视口跟随异常
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** 软换行, textarea, 视口跟随, CJK 折行
**问题本质:** textarea 渲染只按 `\n` 拆分逻辑行，不支持按终端宽度自动折行
**通用模式:** 终端 textarea 需要浏览器 textarea 式的软换行体验——折行在渲染层做（纯视觉），不存储到状态层。光标移动需基于视觉行而非逻辑行
**技术决策:** 折行策略 `overflow-wrap: break-word`（任意字符处断行），与浏览器默认一致
**涉及文件:** peri-widgets/src/textarea/render.rs, peri-widgets/src/textarea/state.rs
**CLAUDE.md 链接:** false

### issue_2026-07-13-clear-scrollbar-persists-at-welcome
**摘要:** /clear 后回到 Welcome 页面，滚动条仍然可见
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** /clear, 滚动条, Welcome 页, ScrollbarFields 重置
**问题本质:** Welcome 分支提前 return 跳过了 scrollbar_fields 更新，旧值保留导致僵尸滚动条
**通用模式:** 组件提前 return 的分支需确保所有副作用状态已重置
**涉及文件:** peri-tui/src/kit/message_area/mod.rs, peri-tui/src/kit/message_area/props.rs
**CLAUDE.md 链接:** false

### issue_2026-07-17-system-note-level-color-not-rendered
**摘要:** SystemNote 的 Warning/Error 等级字体颜色未区分，全部显示为灰色
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** SystemNote, TuiNoteLevel, 颜色渲染, 启发式判断
**问题本质:** 渲染函数忽略 `data.level` 字段，改用文本关键词启发式判断颜色，默认 fallback 为 muted
**通用模式:** 数据结构已有枚举字段时应直接使用而非通过文本关键词反推——后者不可靠且维护成本高
**涉及文件:** peri-tui/src/kit/message_area/render.rs, peri-tui/src/kit/tui_render_unit.rs
**CLAUDE.md 链接:** false

### issue_2026-07-07-message-area-scroll-proximity-follow
**摘要:** 消息区自动吸底应基于滚动位置就近判断，而非二元开关
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** 自动吸底, proximity 跟随, 二元开关, distance_to_bottom
**问题本质:** 二元 `auto_scroll` flag 导致用户任何滚动都失去跟随能力——改为一滚就永久不回
**通用模式:** 自动跟随应基于当前位置与底部的距离（proximity）决定，而非记住"上次是否滚过"。距离≤阈值→吸底；距离>阈值→不抢
**技术决策:** 阈值取 `max(vis_height/2, 5)`，底部半屏内跟随
**涉及文件:** peri-tui/src/kit/message_area/mod.rs
**CLAUDE.md 链接:** false

### issue_2026-07-13-login-panel-enter-does-not-close
**摘要:** Login 面板 Enter 选择 provider 后不关闭面板
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** Login 面板, Enter 关闭, close_active_panel, 状态刷新
**问题本质:** Enter handler 只切换 active_provider_id，未调用 `close_active_panel()` + 推送 SERVICE_SNAPSHOT
**通用模式:** 面板选择操作后的标准步骤：close_active_panel() + 推送状态 snapshot + invalidate pool
**涉及文件:** peri-tui/src/kit/panels/login.rs
**CLAUDE.md 链接:** false

### issue_2026-07-13-main-agent-done-loading-persists-bg-still-running
**摘要:** 主 agent 完成回复后 loading 不退，因后台 agent 仍在运行
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** loading 生命周期, bg agent, SubagentStopped, phase=PromptRunning
**问题本质:** `SubagentStopped` 无条件设 `phase=PromptRunning`→`is_loading=true`——覆盖了之前的 TurnDone/TurnSuspended 清除的 loading
**通用模式:** loading 状态应由"是否有活跃的流式 agent"而非"是否有 SubagentStopped 事件"决定。bg agent 完成时不应重新设 loading
**涉及文件:** peri-tui/src/kit/acp_events.rs, peri-agent/src/agent/stages/mod.rs
**CLAUDE.md 链接:** true

### issue_2026-07-13-workflow-tool-error-task-stuck-and-panel-freeze
**摘要:** Workflow Tool 快速失败后，BgTaskArea 任务条目永久卡在黄色
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** workflow, 快速失败, BgTaskRegistry, complete_workflow
**问题本质:** 快速失败检测 `return Err()` 跳过了通知任务 spawn，`complete_workflow()` 永不调用
**通用模式:** 早期返回路径必须覆盖清理/通知逻辑——把 `complete_workflow()` 移到快速失败检测之前或在错误路径中也调用
**涉及文件:** peri-workflow/src/tool.rs, peri-middlewares/src/workflow/mod.rs
**CLAUDE.md 链接:** false

### issue_2026-07-15-ask-user-panel-layout-wrong-wide-terminal
**摘要:** AskUserQuestion 面板：宽终端下布局混乱
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** 宽终端, 固定宽度, 分隔线, 文本折行
**问题本质:** `WRAP_WIDTH=80` 固定换行和 `"─".repeat(60)` 固定分隔线在 120+ 列终端中与面板实际宽度不匹配
**通用模式:** 布局元素应从组件实际可用宽度动态计算（Constraint::Fill），禁止硬编码固定列宽
**涉及文件:** peri-tui/src/kit/panels/ask_user.rs
**CLAUDE.md 链接:** false

### issue_2026-07-05-scroll-performance-lag
**摘要:** 长数据高速滚动时刷新卡顿/掉帧
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** 滚动性能, write_no_update, ScrollThrottle, tmux
**问题本质:** 每个滚轮事件内多次 `scroll_state.write()` 触发多次原子通知→render loop 多次 draw。tmux 下 PTY 开销放大
**通用模式:** 高频事件 handler 内合并 state 修改为单次 `write_no_update()`，不触发原子通知；增加帧间隔节流（16ms≈60fps）
**技术决策:** ScrollThrottle 累积增量，仅 elapsed≥16ms 时一次性 flush；render loop 强制渲染总能读到最新 atom
**涉及文件:** peri-tui/src/kit/message_area/mod.rs
**CLAUDE.md 链接:** true

### issue_2026-07-13-plugin-panel-left-right-freeze
**摘要:** Plugin 面板 ←/→ 切换 Tab 导致 UI 卡死
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** Plugin 面板, Tab 切换, UI 卡死, use_state 自激
**问题本质:** Tab 切换触发 state 更新→重渲染→handler 重新注册→又触发事件→循环卡死
**通用模式:** Tab 切换时避免在 render body 或 handler 中触发副作用导致自激循环
**涉及文件:** peri-tui/src/kit/panels/plugin.rs
**CLAUDE.md 链接:** false

### issue_2026-07-13-ask-user-esc-freeze-reject
**摘要:** AskUserQuestion 面板 ESC 退出后 TUI 界面卡死
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** ESC 卡死, 事件优先级, EventPriority::High, cancel_ask_user
**问题本质:** 全局 ESC handler（Normal 优先级，注册更早）在面板 handler（同优先级，注册更晚）之前消费 ESC 并截断——`cancel_ask_user()` 从未调用→agent 永久挂起
**通用模式:** 面板/弹窗的 ESC 处理应使用 `EventPriority::High`，确保先于全局 handler 执行，参考 HITL Popup 的优先模式
**涉及文件:** peri-tui/src/kit/panels/ask_user.rs, peri-tui/src/kit/event_handlers.rs
**CLAUDE.md 链接:** true

### issue_2026-07-15-theme-panel-not-refreshed-after-download
**摘要:** 下载完成后 Theme 面板未刷新新增主题
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** Theme 面板, 下载, 列表刷新, 状态同步
**问题本质:** 下载完成后的回调未触发 Theme 面板主题列表重新加载
**通用模式:** 异步操作（下载）完成后需通过 atom/event 机制通知相关面板刷新数据
**涉及文件:** peri-tui/src/kit/panels/theme.rs, peri-tui/src/kit/popups/download_progress.rs
**CLAUDE.md 链接:** false

### issue_2026-07-17-login-panel-missing-provider-type-field
**摘要:** Login 面板缺少 Provider 类型编辑字段
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** Login 面板, ProviderType, 编辑模式, 字段枚举
**问题本质:** LoginEditField 枚举缺少 ProviderType 变体，编辑模式下无法修改 provider 类型
**通用模式:** 编辑面板的字段枚举需与数据模型完整对齐——参考 Setup Wizard 的 FormField 覆盖范围
**涉及文件:** peri-tui/src/kit/panels/login.rs
**CLAUDE.md 链接:** false

### issue_2026-07-14-ask-user-multiselect-tui-support
**摘要:** AskUserQuestion 面板：多选交互缺失 + 文本超长不换行
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** 多选, AskUserQuestion, TUI 交互, 文本折行
**问题本质:** JSON Schema / ACP Broker 已支持 multiSelect，TUI 面板只实现了单选；文本使用 Line::from() 不折行
**通用模式:** 多层协议栈（JSON Schema → ACP Broker → TUI 面板）需逐层验证功能支持——上游支持不代表下游已实现
**涉及文件:** peri-tui/src/kit/panels/ask_user.rs
**CLAUDE.md 链接:** false

### issue_2026-07-14-inline-code-no-color
**摘要:** Markdown 行内代码无颜色渲染
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** Markdown, 行内代码, 主题颜色, Modifier::DIM 哨兵
**问题本质:** span_style 通过 `Modifier::DIM` 哨兵判断行内代码，但 ratatui-kit-markdown 0.3.0 不设置此修饰符
**通用模式:** 基于第三方库修饰符/标记的启发式检测脆弱——上游版本升级可能改变行为。应用更稳健的检测方式
**涉及文件:** peri-tui/src/kit/markdown/span_style.rs
**CLAUDE.md 链接:** false

### issue_2026-07-17-spinner-tick-decouple-from-acp-bridge
**摘要:** Spinner 帧推进绑定 acp_bridge 1s tick，应改为 TUI 独立 tick
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** spinner, tick 解耦, 动画帧率, TUI 独立循环
**问题本质:** spinner 帧推进挂在 acp_bridge 的 1s interval 上——~100ms/帧的动画实际每 1s 才跳一帧
**通用模式:** 动画/渲染循环应与业务事件流解耦——TUI 侧独立高频 tick 驱动视觉更新。spinner 帧计算基于壁钟，只需足够频繁的渲染触发
**涉及文件:** peri-tui/src/kit/acp_bridge.rs, peri-tui/src/kit/entry.rs
**CLAUDE.md 链接:** false

### issue_2026-07-11-history-replay-scroll-too-early
**摘要:** History 恢复会话时 scroll_to_bottom 过早，布局未就绪
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** history 恢复, scroll_to_bottom, 布局时序, 20帧强制窗口
**问题本质:** replay 第一批只有 1 条消息→prev==0 触发 scroll_to_bottom→此时 ScrollViewState.size=None→offset 无效。后续大批次因 proximity guard 永不滚底
**通用模式:** 初始加载/恢复场景需要"强制吸底窗口"（如 20 帧=333ms），覆盖所有批次到达。判断基于帧数而非消息数——因为消息是分批到达的
**技术决策:** prev==0 时启动 20 帧强制窗口，每帧 set_offset(0, u16::MAX)，不依赖前帧渲染值
**涉及文件:** peri-tui/src/kit/message_area/scroll.rs, peri-tui/src/kit/message_area/mod.rs
**CLAUDE.md 链接:** true

### issue_2026-07-10-bg-subagent-tool-count-always-zero
**摘要:** 后台 subagent 完成通知中的"工具调用"计数始终为 0
**状态:** Fixed
**归档日期:** 2026-07-18
**关键词:** bg subagent, tool_calls_count, 硬编码, BackgroundTaskResult
**问题本质:** 4 处构造 `BackgroundTaskResult` 的位置全部硬编码 `tool_calls_count: 0`，未从实际执行的 subagent 获取工具调用计数
**通用模式:** 数据字段需要从实际来源获取而非硬编码默认值。构造点数量越多，遗漏风险越大——考虑工厂函数统一构造
**涉及文件:** peri-middlewares/src/subagent/tool/execute_bg.rs, peri-middlewares/src/subagent/spawner.rs
**CLAUDE.md 链接:** false

### issue_2026-07-18-duplicate-streaming-tool-cards
**摘要:** 流式输出时工具调用卡片重复显示——render 事件双轨扇出
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** 工具卡片重复, 流式渲染, 去重, tool_id
**问题本质:** start_tool 事件可能被多个 render 路径触发，导致同一个 tool_id 的工具卡片被重复推入 VIEW_MODELS
**通用模式:** 增量推入类事件（非幂等覆盖）必须做去重——`tool_id` 去重是低成本方案。同步追踪已处理的 tool_id 集合，避免同一卡片多次创建
**涉及文件:** peri-tui/src/kit/acp_events.rs
**CLAUDE.md 链接:** false

### issue_2026-07-19-streaming-mode-config-not-effective
**摘要:** streaming_mode 配置切换无效——用户切换模型后流式模式不跟随
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** streaming_mode, 配置切换, 模式同步, BridgeState
**问题本质:** streaming_mode 在模型切换时未通过配置通道同步到 BridgeState，TUI 端用旧的模式值处理流式事件
**通用模式:** 运行时配置变更需双端同步——ACP 服务端和 TUI 前端各维护一份状态，任一端未更新都会导致行为不一致。配置变更应走统一通道（如 update_config push）
**涉及文件:** peri-tui/src/kit/acp_bridge.rs, peri-tui/src/kit/acp_events.rs
**CLAUDE.md 链接:** false

### issue_2026-07-18-login-panel-missing-new-delete-crud
**摘要:** Login 面板缺少新建/删除功能，CRUD 不完整
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** Login 面板, CRUD, 新建, 删除, Provider 管理
**问题本质:** LoginPanelMode 枚举仅 Browse/Edit 两个变体，缺少 New/Delete/ConfirmDelete——用户无法在面板内完成 Provider 完整生命周期
**通用模式:** 数据管理面板应覆盖完整 CRUD 而非仅 RU 两环。i18n key 提前定义但代码未用，说明设计意图与实现存在 gap——需要补齐
**涉及文件:** peri-tui/src/kit/panels/login.rs
**CLAUDE.md 链接:** false

### issue_2026-07-17-loading-state-split-brain
**摘要:** TUI Loading 状态由三个写入源造成分裂——submit_consumer 乐观写入、acp_bridge phase 派生、TurnDone 手动兜底
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** loading 状态, split-brain, phase 派生, PromptSubmitted 事件, 状态统一
**问题本质:** ACP_STATE.is_loading 由三条路径分别写入，形成三层 workaround 互相修补的脆弱结构——submit_consumer 绕开 bridge 直接写 atom，push_acp_state 的防御性 phase 提升 hack，7 处事件手动清 atom
**通用模式:** 全局 UI 状态应由单一数据源派生——submit 改为发 PromptSubmitted 事件→bridge 统一设 phase→push_acp_state 派生 is_loading。删除防御性 hack 和手动 atom 写入。多写源状态管理 = split-brain bug 工厂
**技术决策:** 采用事件驱动（PromptSubmitted）替代直接 atom 写入，bridge 统一处理所有 loading 状态变更
**涉及文件:** peri-tui/src/kit/acp_events.rs, peri-tui/src/kit/submit_consumer.rs, peri-tui/src/kit/acp_bridge.rs
**CLAUDE.md 链接:** false

### issue_2026-07-18-workflow-panel-agent-token-tool-display-zero
**摘要:** Workflow Panel Agent 进度列（token 消耗/工具调用数）始终显示 0，列未对齐
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** Workflow Panel, token 计数, tool 计数, agent_progress, 列对齐
**问题本质:** 4 个独立 bug——agent 运行期间无 agent_progress 事件发送、AgentDone handler 不读 token_count、tool_count 硬编码 None、列标题缺失+对齐用 char 而非终端列宽
**通用模式:** 进度面板的数据流需要持续推进事件（agent_progress）而非仅依赖最终事件（AgentDone）——用户需要实时反馈。面板列对齐应使用 unicode_width 而非 Rust char 计数
**涉及文件:** peri-tui/src/kit/panels/workflow.rs, peri-acp/src/agent/workflow_agent.rs, peri-workflow/src/progress.rs
**CLAUDE.md 链接:** false

### issue_2026-07-08-viewmodels-flatten-refactor
**摘要:** ViewModels 扁平化重构——单层 im::Vector 替代 committed/current_turn 分裂
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** VIEW_MODELS, committed/current_turn, 冻结线消除, 单层架构
**问题本质:** TUI 渲染层维护 committed（已确认）和 current_turn（流式中）两条 im::Vector 路径—写入点混乱、冻结线概念泄漏到 UI 层
**通用模式:** 单层 im::Vector 替代双轨——冻结线是 Agent 层概念，UI 不需要。所有写入走同一入口，消除旁路和死字段。VIEW_MODELS 是唯一数据源
**涉及文件:** peri-tui/src/kit/acp_events.rs, peri-tui/src/kit/acp_bridge.rs
**CLAUDE.md 链接:** false

### issue_2026-07-07-slash-clear-messages-reappear-after-1s
**摘要:** /clear 后旧消息 1s 后自动恢复
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** /clear, ViewCommit, FIFO 管道, BRIDGE_RESET_COUNTER
**问题本质:** /clear 的多个侧信道 hack（BRIDGE_RESET_COUNTER 等）不能可靠清除状态——旧消息在下次事件到达时重新渲染
**通用模式:** 用标准事件 + FIFO 管道清状态——session/new 后推送空 ViewCommit，利用消息管道自然排列覆盖旧数据。不要发明 BRIDGE_RESET_COUNTER 等侧信道机制
**涉及文件:** peri-tui/src/kit/acp_bridge.rs, peri-tui/src/kit/acp_events.rs
**CLAUDE.md 链接:** false

### issue_2026-07-03-slash-popup-missing-skills
**摘要:** Slash 弹窗缺少 skills——ACP 已推送但 TUI 未消费
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** slash popup, skills 显示, ACP→TUI 数据通道
**问题本质:** ACP 服务端已通过事件推送 skill 列表，但 TUI 端未订阅对应的事件通道→skill 从未出现在弹窗中
**通用模式:** ACP 层数据推送后必须验证 TUI 层订阅了对应事件通道——单向推送不等于显示。每新增 AgentEvent 变体需同步检查 acp_notifier→acp_events→VIEW_MODELS 完整链路
**涉及文件:** peri-tui/src/kit/acp_notifier.rs, peri-tui/src/kit/panels/slash.rs
**CLAUDE.md 链接:** false

### issue_2026-07-06-slash-popup-duplicate-commands
**摘要:** Slash 弹窗命令重复——PANELS + AVAILABLE_SLASH_COMMANDS 双源未去重
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** slash commands, 双源合并, 去重, 硬编码残留
**问题本质:** PANELS 和 AVAILABLE_SLASH_COMMANDS 两个来源合并时未去重→8 条 panel 命令出现两次。ACP 侧硬编码 panel 列表的残留代码与动态来源形成双源
**通用模式:** 双源合并必须去重（按命令名）。确定唯一规范来源后（如动态 panel 列表），删除另一方的硬编码残留——避免"改一处漏一处"
**涉及文件:** peri-tui/src/kit/panels/slash.rs, peri-acp/src/
**CLAUDE.md 链接:** false

### issue_2026-07-06-history-session-switch-data-mix
**摘要:** History 切换 session 后新旧数据混合显示
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** session 切换, 数据混合, ViewCommit, BRIDGE_RESET_COUNTER
**问题本质:** 空 session 不发送 ViewCommit→TUI 读到旧 atom 值未清除。状态重置链（BRIDGE_RESET_COUNTER→加载→ViewCommit）有空 session 未处理的断环
**通用模式:** 状态重置链的每环必须验证——空 session 也需要 emit ViewCommit 清空旧数据。fallback 读旧 atom 值是静默数据污染的常见来源
**涉及文件:** peri-tui/src/kit/acp_bridge.rs, peri-acp/src/session/
**CLAUDE.md 链接:** false

### issue_2026-07-12-agent-nested-toolcall-misplaced-into-history
**摘要:** Agent 子工具卡片渲染位置错误——在 Agent 卡片上方而非嵌套内
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** 工具卡片, 嵌套位置, items 平面列表, 事件时序
**问题本质:** 消息区用平面 im::Vector 存储所有 TuiRenderUnit——工具事件在 Agent turn 内到达，按时间戳排在 items 末尾，而非嵌套在 AgentGroup 下面
**通用模式:** 平面列表结构需要渲染层的分组逻辑来模拟嵌套——事件到达时序不等于视觉层级。render 阶段需要按 agent_id/group 重新编排 items 的呈现位置
**涉及文件:** peri-tui/src/kit/acp_events.rs, peri-tui/src/kit/message_area/render.rs
**CLAUDE.md 链接:** false

### issue_2026-07-09-system-reminder-condensed-rendering
**摘要:** system-reminder 消息 LLM 通道和 TUI 通道格式不一致
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** system-reminder, 跨通道格式, LLM 包裹标签, TUI 渲染
**问题本质:** system-reminder 走 LLM 通道（包裹 `<system-reminder>` 标签）和 TUI 通道（无标签）两条独立路径，格式不一致→用户看到的内容和 LLM 理解的内容对不齐
**通用模式:** 跨通道消息格式必须维护唯一事实源——LLM 上下文和 TUI 渲染的 packaging 策略需同步。考虑用统一的结构化 message 类型，各通道自行提取所需字段
**涉及文件:** peri-agent/src/messages/, peri-tui/src/kit/acp_events.rs
**CLAUDE.md 链接:** false

### issue_2026-07-08-loading-indicator-never-displays
**摘要:** ViewModel 消除重构后 loading spinner 不再显示
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** loading spinner, 重构回归, 传播链验证
**问题本质:** ViewModels 扁平化重构中 loading 传播链断裂——spinner 渲染依赖 submit→bridge→ACP_STATE atom→footer render 的完整链路，重构后中间环断开
**通用模式:** 大规模重构后必须逐环验证 loading 传播链是否完整。loading 是跨模块的全局状态，增加覆盖测试确认各路径的终态消费
**涉及文件:** peri-tui/src/kit/acp_bridge.rs, peri-tui/src/kit/message_area/footer.rs
**CLAUDE.md 链接:** false

### issue_2026-07-04-model-panel-enter-statusbar-delay
**摘要:** Model 面板切换后状态栏延迟 1-2s 才更新
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** 状态栏延迟, SERVICE_SNAPSHOT, 轮询, 面板配置
**问题本质:** 状态栏的 model 信息由 SERVICE_SNAPSHOT atom 驱动——该 atom 由 2s 轮询 task 更新。面板切换 model 后轮询周期未到，新 model 名不显示
**通用模式:** 面板修改全局配置后必须主动推送更新所有依赖 atom，不能依赖异步轮询。交互式面板操作→即时 atom 写→即时 UI 反馈，轮询仅用于外部变更检测
**涉及文件:** peri-tui/src/kit/panels/model.rs, peri-tui/src/kit/atoms.rs
**CLAUDE.md 链接:** false

### issue_2026-07-07-ctrl-c-exits-during-loading
**摘要:** Ctrl+C 在 loading 状态中直接退出应用而非中断 agent
**状态:** Fixed
**归档日期:** 2026-07-20
**关键词:** Ctrl+C, loading 上下文, 事件优先级, 中断 vs 退出
**问题本质:** Ctrl+C 处理器不区分上下文——loading 中应中断 agent，空闲时应退出应用（或双击退出）。统一处理导致 loading 中误退出
**通用模式:** 全局快捷键处理器必须上下文感知。Ctrl+C 的行为根据应用状态动态调整：loading→中断 agent，空闲→退出确认。事件优先级需根据当前模态动态调整
**涉及文件:** peri-tui/src/kit/event_handlers.rs
**CLAUDE.md 链接:** false
