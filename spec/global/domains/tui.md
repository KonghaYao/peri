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
