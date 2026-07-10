# CLAUDE.md

## v2 架构状态（2026-07-01）

**当前状态**：v2 stages 单路径架构。v1 `ReActAgent` / `executor/` / `State` trait / `CompactMiddleware` / v1 `MessageQueue` 已物理删除。所有执行路径（main/SubAgent/Hook/Workflow）统一走 v2 `run_react_loop`。TUI 已迁移至 ratatui-kit 单路径（S1–S13 删除 ~18000 行 legacy 代码），详见下文。Phase 2.6 step 7e.9 已完成——`view_messages` 字段退役。

**已完成**：P1–P5 + ultracode Workflow A/B1/B2/B3 + Workflow C（面板重写）+ Workflow D（类型隔离）+ Phase 2.6（全部）。
**全部完成**：S1–S13 kit 单路径迁移（净减 ~18000 行）、I14–I23 生产级增量开发。

### v2 单路径架构

所有路径 → `build_and_execute_agent_v2` → `run_react_loop`。ReAct 循环：每轮 `Compact → Receive → Reason → Act → End`。Compact 阈值 0.70/0.85；Reason 走 LLM + middleware hooks；Act 3 阶段工具分发；End 检查 queue 是否续跑。

**[TRAP]** ACP executor 末尾禁止加 `drain_for_end` 循环——消息物理丢失。idle 续跑由 TUI 负责。
**[TRAP]** `run_session_loop` 末尾禁止加 `await_wake`——stdio/IDE 收不到完成响应。

### TUI 当前架构（ratatui-kit 单路径）

S1–S13 已删除全部 legacy 路径（`runtime/main_loop`、`state_machine/`、`command/`、`ui/`、`render/`、`event/`、`agent_ops/`），TUI 现在纯 kit 路径，详见 `peri-tui/CLAUDE.md`。

核心数据流：`ViewModelsSnapshot { committed: Arc<[ViewModel]>, current_turn: Arc<[ViewModel]> }` 单一数据源。`RENDER_CACHE` atom 存储 `render_bridge` 预计算的 `Vec<Line<'static>>` + `wrap_map`（WrappedLineInfo 视觉行映射），message_area 基于 `wrap_map` 二分查找做视口裁剪。`content_hash` 增量检测避免全量重建。

**[TRAP]** `RENDER_CACHE` 与 `VIEW_MODELS` 分离：render_bridge 异步预计算 Line，message_area 仅从 RENDER_CACHE 取用——渲染层不直接 touch ViewModel。

### TUI 渲染管道

```
ACP 事件 → acp_notifier → acp_bridge → dispatch_and_notify → VIEW_MODELS atom 写入
                                                                    ↓
                                        render_bridge (独立 tokio task) → RENDER_CACHE atom
                                                                    ↓
                              message_area (ratatui-kit ScrollView + 视口裁剪)
```

render_bridge 监听 ACP 事件 + 宽度变化，预计算每条 ViewModel 的 `Vec<Line<'static>>` + `WrappedLineInfo`（视觉行映射），写入 `RENDER_CACHE`。message_area 基于 `wrap_map` 做二分查找视口裁剪，只渲染可见行。

**[TRAP]** `/clear` 命令必须同步重置 `RENDER_CACHE`，否则旧缓存残留。所有视图更新必须走统一事件渠道（本地提交也需触发渲染刷新），禁止旁路 RENDER_CACHE。（详见 spec/global/domains/tui.md#issue_2026-07-05-message-flow-render-sync-freeze）
**[TRAP]** ratatui-kit 事件边界：消息区只处理鼠标滚轮 `Event::Mouse(MouseEventKind::Scroll*)`，编辑区只处理键盘编辑/导航事件。`InputArea` Up/Down 应先做多行光标移动，只在首/末行时才 history fallback。排查"事件去哪了"需逐层验证 Global/High → Global/Normal → Current/High → Current/Normal，每层都可能独立拦截事件。ScrollView 传 `active: false` 可关闭其内置键盘 handler。（详见 spec/global/domains/tui.md#issue_2026-07-04-message-area-scrollview-steals-input）

### 跨 task 共享状态排障铁律（2026-07-05 双 bug 教训）

两个 `/clear`/history 切换卡死 bug 暴露同一模式：见症状治症状，没追踪到唯一事实源。

**铁律 1：先画数据写入者链，再动手。** 看到 atom 值不对时，不要直接改该 atom 的写入点。先往回追问：
1. 这个 atom 被谁写？（列出所有 `*.state().write()` 调用点）
2. 这些写入者从哪读数据？（追踪到内存中的 struct 字段）
3. 那个 struct 字段是谁在维护？（找到**唯一的真实数据源**）
4. **改真实数据源**，而不是在每个 consumer 端做 defensive reset。

典型案例：`VIEW_MODELS` atom 的 committed 值不对 → 查到 `push_view_models` 从 `BridgeState.committed` 读取 → `BridgeState` 是唯一事实源 → 修复是清 `state.committed`（`BRIDGE_RESET_COUNTER`），而不是在 submit_consumer 里重复重置 atom。

**铁律 2：ratatui-kit hook 修了就要逐行枚举。** 任何 `hooks.use_*` 调用点变更后，必须列出该组件中**每一个 hook 调用**（行号 + 类型），对比场景 A/场景 B 两帧的调用列表是否完全一致。肉眼扫一眼不可靠——`build_footer_lines` 就是漏了第 5 个 `use_state(once)`。

**[TRAP]** 涉及 ratatui-kit `#[component]` 或接收 `&mut Hooks` 的函数内部，**所有** `hooks.use_*` 调用必须在任何 `if`/`match`/`return` 之前——ratatui-kit 按调用顺序索引 hook，顺序/数量变化会触发 `"Hook type mismatch"` panic 或状态数据错位。（详见 spec/global/domains/tui.md#issue_2026-07-05-enter-clear-hook-mismatch-panic）

**[TRAP]** ratatui-kit render body 中禁止写 atom——render 期间任何 atom 写入会与组件生命周期交互形成 render → state write → render 自激回路。事件处理器负责所有状态变更，render 仅做只读展示。**`use_effect` 中写 atom 等效危险**——流式期间每个 chunk 都触发 effect → scroll_to_bottom() 写 atom → 形成同款紧耦合环路。（详见 spec/global/domains/tui.md#issue_2026-07-03-tui-double-slash-cpu-spike、#issue_2026-07-06-message-area-copy-complex-content-crash、#issue_2026-07-09-message-area-periodic-white-flash-streaming）
**[TRAP]** /clear 和 thread 切换时必须递增 `BRIDGE_RESET_COUNTER`，acp_bridge 在下次事件处理前检测到变更会自动清空 `committed`/`has_view_commit`/`current_turn`/`is_loading`。仅在 atom 层面重置不足以清除旧 session 残留。`BRIDGE_RESET_COUNTER` 是跨 session 的桥梁状态重置，/clear 或 thread 切换前必须先 +1。

### ratatui-kit overlay 白屏教训

`AppShell` 根层把主内容、`PanelOverlay`、`PopupOverlay` 作为兄弟节点渲染；overlay 空态绝不能返回普通 `View()`，也不要用 `Fragment` 当函数组件根空态。ratatui-kit 函数组件布局透明，会继承返回根节点的 layout；`View()` 会参与父级 flex，`Fragment` 空根在该场景也会退化成会挤布局的默认节点，表现为 `/ratatui-kit` 整屏白/主界面被挤没。

**[TRAP]** 根级 overlay 空态必须返回显式零尺寸 `Positioned(x: 0u16, y: 0u16, width: 0u16, height: 0u16, clear: false)`，不要返回 `View()` / `Fragment`。active overlay 才用实际尺寸的 `Positioned`。排查白屏时先做根层早退诊断：`AppShell` 最小 Text → `SessionColumn` → `StatusBar` → `PanelOverlay` → `PopupOverlay`，逐个恢复定位。

### 关键架构点

- `builder::build_agent` → `AgentComponents`（llm/chain/tools/registry/prompt/budget）。
- `builder_v2::build_stage_context` → 消费 `AgentComponents`，`chain.collect_tools(cwd)` 注入 shared_tools。
- `Session::new_with_cancel`：linked `CancellationToken`。
- EventBus 3 层：render/state/observe → `ExecutorEvent` → event_tx。
- Compact 由 `stages/compact.rs` 统一处理（`/compact` 命令也复用）。
- v2 `MessageQueue` 作为统一异步消息通道：cron/channel/workflow/bg_results push → polling drain_for_end → submit_message 续跑。
- **[TRAP]** ACP 通知覆盖度：`acp_notifier` 生命周期中的所有事件（包括结束类 AgentDone→TurnDone）必须完整转发，遗漏导致 UI 状态残留（loading 卡死等）。（详见 spec/global/domains/tui.md#issue_2026-07-06-enter-hello-cpu-spike）

## Workflow 故障排查

Workflow 出现 "0 agents, 0 tool calls" 时：
1. `which peri-workflow` 可用；不存在则 `cd npm-packages/@peri-workflow && npm install && npm run build`
2. `cargo build -p peri-workflow -p peri-acp` 无错
3. 重启 Peri TUI

## Crate 总览

| Crate | 职责 |
|-------|------|
| `peri-agent` | ReAct 循环、Middleware trait、LLM 适配器、工具系统 |
| `peri-middlewares` | 18 个中间件（FS/终端/HITL/SubAgent/Skills/Todo/Cron/MCP/Hooks/LSP） |
| `peri-widgets` | ratatui + pulldown-cmark 组件库 |
| `peri-acp` | ACP 服务层（MpscTransport/StdioTransport） |
| `peri-tui` | TUI 应用（纯 ACP client） |
| `langfuse-client` / `peri-lsp` / `peri-web-pty` | 独立基础库 |
| `agm` | Agent Package Manager |

依赖方向：`peri-tui` → `peri-acp` → `peri-agent`/`peri-middlewares`。

### subagent + worktree 排障铁律（2026-07-07 ACP 升级双坑）

使用 Agent 工具派发 coding 子任务到 worktree 时，以下两点已验证会出问题：

**铁律 1：coder subagent 不遵守 cwd。** 即使 `Agent(cwd=worktree_path)` 已指定工作目录，coder 的 `Read`/`Edit`/`Write` 仍可能落在主工作区。**强约束**：
- coding subagent 的 prompt 中必须用绝对路径前缀指定所有文件（如 `Write(file_path="/abs/path/to/worktree/peri-acp/Cargo.toml")`）
- push 代码前必须 `git diff --stat` 在 worktree 目录确认变更确实落在了 worktree
- 主工作区若被误改，立即 `git reset --hard HEAD && git clean -fd` 恢复

**铁律 2：coder 天生越权。** coder 在机械性任务中仍有概率夹带无关变更。**强约束**：
- coding prompt 必须显式列出允许修改的**文件白名单**
- 用 `DO NOT modify:` 逐条写明禁止触碰的目录/文件
- commit 前用 `git diff --stat` 核对文件清单，不在白名单内的立即回退

**[TRAP]** rustfmt import 排序规则：同一 crate 下，`use crate::module::*` 通配导入排在 `use crate::module::SpecificType` 单类型导入之前。跨 crate 同理：`use foo::v1::{...}` 排在 `use foo::ProtocolVersion` 之前。如果 formatter 报错但肉眼看起来一样，交换两组 import 的顺序即可。

## 开发命令

- `cargo build --workspace`：全量构建
- `cargo test --workspace`：运行所有测试
- `cargo run -p peri-tui -- -a`：HITL 审批
- `scripts/start-tui.sh`：启动（RELAY_PORT=3001）
- `lefthook run pre-commit`：fmt/check/clippy
- `cargo test -p <crate> --lib -- <test_name>`：单测

## 核心文件树

```
peri-agent/src/agent/stages/  # v2 ReAct 循环（reason/act/compact/receive/end/tool_dispatch）
peri-agent/src/agent/compact_v2.rs  # v2 compact 入口
peri-agent/src/llm/{openai,anthropic}/invoke.rs  # Provider 特定处理 + System hoist
peri-agent/src/messages/    # BaseMessage / ContentBlock（含 Reasoning）
peri-middlewares/src/tool_search/  # Core(12)/Meta(2)/Deferred 工具
peri-middlewares/src/subagent/     # SubAgent 中间件 + 构建器
peri-middlewares/src/agents_md/    # CLAUDE.md 加载（frozen 透传终点）
peri-acp/src/session/executor.rs  # execute_prompt() 统一入口
peri-acp/src/prompt/mod.rs        # build_system_prompt() + __SYSTEM_PROMPT_DYNAMIC_BOUNDARY__
peri-acp/src/event/{router,mapper,view_mapper}.rs  # ACP 事件 → ViewModel 映射
peri-widgets/src/         # ratatui + pulldown-cmark 组件库（ToolCard/Markdown/Diff/Spinner）
peri-tui/src/kit/entry.rs  # TUI 入口（单路径 run_kit_fullscreen）
peri-tui/src/kit/acp_bridge.rs  # BridgeState → Atom 写入
peri-tui/src/kit/render_bridge.rs  # 渲染预计算（Vec<Line> + wrap_map）
peri-tui/src/kit/view_render.rs  # ViewModel → ratatui Line 转换
peri-tui/src/kit/message_area.rs  # 消息区（ScrollView + 视口裁剪 + Todo）
peri-acp/prompts/sections/        # 14 个系统提示词段落（已从 peri-tui 迁入，归属 ACP 层）
```

## 架构要点

**ReAct 循环**：`before_agent → loop(500) { before_model → LLM → after_model → [工具分发] | [回答] }`。TUI cover `max_iterations(500)`。
**消息类型**：`BaseMessage`（Human/Ai/System/Tool），`ContentBlock`（Text/Image/Document/ToolUse/ToolResult/Reasoning/Unknown）。
**系统提示词**：`build_system_prompt()` 在 session/new 调用，`__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 分隔静态/动态。

**[TRAP]** 中间件 `before_tool`/`after_tool`/`on_error` 不读 `state.messages()`——`tool_dispatch.rs` 延迟写入。
**[TRAP]** 新增 `AgentEvent` 变体需同步 TUI 侧 `map_executor_event` 映射。
**[TRAP]** `Interrupted`/`Error` 与 `Done` 互斥：前者 `request_rebuild()`+设 `reconcile_already_done=true`。

## 上下文缓存（第一优先）

会话开始后 SP 不可变更——变化导致 Prompt Cache 失效。动态区域（boundary 之后）占位符值可变，但结构不变。

**[TRAP]** 中途纠正消息必须 `BaseMessage::human(...)`（`<system-reminder>` 或 `<goal-message>`），禁止 `BaseMessage::system(...)`——invoke.rs 会把 System hoist 到顶层污染 frozen prompt。
**[TRAP]** SubAgent 必须复用 main agent frozen 的 CLAUDE.md/Skills，禁止重新读盘。
**[TRAP]** 非 System 消息用 `add_message`（尾部追加），禁止 `prepend_message`。

## Tool Search

三层：Core（12，始终可见）、Meta（2，SearchExtraTools/ExecuteExtraTool）、Deferred（Cron/MCP/LspTool 等）。新增/删除 Core 工具需同步 7 处（`core_tools.rs` → `05_using_tools.md` → `hitl/mod.rs` → `event/mapper.rs` → `tool_display.rs` → `core_tools_test.rs` → `GitAttributionMiddleware`）。

## 中间件链

19 个中间件固定顺序，末尾 `with_system_prompt()` prepend。详见 `peri-middlewares/CLAUDE.md`。

## 错误感知建议层

工具错误前通过 `ErrorSuggestRegistry` 注入建议。集成在 `tool_dispatch.rs::collect_tool_results`。

## ACP/TUI 分层

TUI 纯 ACP client，通过 `MpscTransport` 通信。Frozen Data Flow：`frozen_date → frozen_claude_md → frozen_skill_summary → frozen_system_prompt`。

**[TRAP]** `PromptFeatures::detect()` 每轮读 `YOLO_MODE`/`is_git_repo`，未 frozen。
**[TRAP]** Immediate 命令绕过 agent event pump，必须手动 `sink.push_done()`。
**[TRAP]** Agent 构建统一走 `execute_prompt()`，禁止 TUI 直连。

### ACP 自定义事件原则（2026-07-07 决策）

**标准有的走标准，真没有的才自定义。** 不要在 `peri/unstable-event` 中重复定义 ACP `session/update` 已有的能力。判断流程：

1. 先查 ACP v1 `SessionUpdate` 是否有对应 tag
2. 有 → 走标准 `session/update`，不发明自定义事件
3. 没有 → `peri/unstable-event` 自定义事件

**已知违例待清理**：§4.1 流式四事件（`text-chunk`/`reasoning-chunk`/`tool-started`/`tool-ended`）与 `session/update` 的 `agent_message_chunk`/`agent_thought_chunk`/`tool_call`/`tool_call_update` 完全重复。（详见 `docs/design/decisions/2026-07-07-acp-reuse-first.md`）

## 上下文压缩

`stages/compact.rs` 每轮检查 `ContextBudget`：0.70→micro，0.85→full。`/compact` 命令复用 `compact_v2::run_compact(force=true)`。

## 环境变量

`~/.peri/settings.json` env 字段注入。Provider：`ANTHROPIC_*`/`OPENAI_*`。行为：`YOLO_MODE`/`DISABLE_COMPACT`。日志：`RUST_LOG`。遥测：`LANGFUSE_*`。

## 编码规范

- Rust 2021 + async-trait；库 `thiserror`，应用 `anyhow`。库 `tracing`，禁止 `println!`。
- 测试分离 `_test.rs`（≥30 行）。每模块一目录 `mod.rs`。resolver = "2"。
- 字符串截断用 `chars().take(N)`（`&s[..N]` 对 CJK panic）。终端列宽用 `unicode-width`。
- 快捷键：禁止 `Shift+字母`；优先 `Ctrl+字母`；不用 PageUp/Down。
- 禁止 `ℹ`（U+2139）和 `[i]` 前缀。
- `Event::Paste` 独立于 key event 链。需启用 BracketedPaste 确保粘贴内容合并为单个 Event::Paste，缺失会导致换行符被解析为 Enter 提交。（详见 spec/global/domains/tui.md#issue_2026-07-05-paste-newline-triggers-submit）

## 测试风格

命名 `test_<对象>_<场景>`；注释/断言用中文。Arrange-Act-Assert 无空行。Mock `make_` 前缀，不用 Mock struct。

## 开发注意事项

- 测试隔离：`App::save_config(cfg, self.config_path_override.as_deref())`
- `std::sync::RwLockReadGuard` 不是 Send → async 跨 `.await` 用 `parking_lot::RwLock`
- App 状态拆分：`ServiceRegistry`（跨会话）+ `GlobalUiState`（UI 临时）
- **[TRAP]** u16 坐标偏移计算必须使用 `saturating_add`/`saturating_sub`，禁止裸 `+`/`-`；单一滚动机制，ScrollView 与 Paragraph 内置滚动不能叠加；剪贴板等阻塞系统 I/O 用 `std::thread::spawn` 独立线程。（详见 spec/global/domains/tui.md#issue_2026-07-05-message-area-crashes-and-rendering）

## bg callback 气泡五次修复全记录（2026-07-09）

**问题描述**：bg agent 完成后注入的合成 user message 不在 TUI 中渲染用户气泡，导致 AI 消息重叠。后续修复尝试中气泡在错误位置出现（position 2 或末尾）。

**根因约束**：TUI 的 `BridgeState.committed` 和 `current_turn` 是二分结构。`TurnDone`（对应 ACP `push_done`）是 Turn 的**唯一提交点**，发生在 ReAct 循环退出时。同一轮 TurnDone 的 AI 内容是一个不可分割的整体——合成消息只能位于整个块之前或之后，无法插入中间。

### 失败路径

| # | 方案 | 结果 | 根因 |
|---|------|------|------|
| 1 | executor registry pump `push_session_update("user_message_chunk")` | position 2 | committed 只有 user 输入，无 AI 内容 |
| 2 | `BridgeState.pending_bg_message` → TurnDone 后插入 | 末尾 | `route_bg_result` 同步唤醒 agent，unstable event 异步传输→锚定到后续 TurnDone |
| 3 | 无条件立即 push committed | position 2 | bg agent 在 immediate command 期间完成，committed 无 AI 内容 |
| 4 | agent End 阶段 EventBus → `SessionUpdate::UserMessageChunk` | position 2 | **日志确证**：Agent 循环内 post-wake drain 后 emit，此时 TurnDone 尚未发生，TurnDone 一次性归档全部 AI 内容 |
| 5 | ✅ **双通道 flush-then-push** | 正确 | 见下文 |

### 正确方案：双通道 flush-then-push

**架构**：
```
agent End 阶段 MQ drain → emit SyntheticUserMessage
  → EventBus → forwarder → mapper_v2 → ExecutorEvent::MessageAdded
  → event pump:
      ① push_unstable_event("bg-callback-user-message") → TUI: BgCallbackBubble → FLUSH current_turn → committed
      ② push_event → mapper → SessionUpdate::UserMessageChunk → TUI: LocalUserBubble → PUSH 气泡
```

**关键点**：
- **`BgCallbackBubble` 只 flush，不 push 自身**（避免重复）。flush 把当前 current_turn 归档到 committed。
- **`LocalUserBubble` 由标准 session/update 通道推送**，复用了 session replay 已有的 handler（`acp_notifier.rs:483-491`，早已 unguarded）。
- **事件泵先发 unstable 再发 push_event**，保证 TUI bridge 先收到 flush 再收到气泡。
- **emit 点在 agent 的 `run_react_loop` 内部**（`stages/mod.rs` 两处：End 阶段 `should_continue` + post-wake `drain_for_end`），消除了 registry event pump 的异步竞争窗口。

**涉及文件**：9 files in 4 crates（peri-agent: events_v2, events_v2_mapper, stages/mod; peri-acp: mapper, mapper_test, executor_helpers, event_sink; peri-tui: acp_types, acp_events, acp_bridge）

**测试**：1309 pass（616 + 278 + 415）

**[TRAP]** 在 TUI bridge 的 `dispatch_and_notify` 中，`committed` 是已归档的 turn 内容，`current_turn` 是当前未归档的 streaming 内容。**TurnDone 是唯一提交点**——发生在 ReAct 循环退出时。任何需要在 Turn 中间插入的合成消息，都必须通过 **flush current_turn → committed** 先行切分 visual turn，再 push 自身。单通道方案无论时序如何调整都无法解决此约束。

**[TRAP]** 不要从 registry event pump（独立 tokio task）发送 TUI 气泡——时序不可控。必须在 agent 的 consumer 侧（MQ drain 点）emit，让 agent 内部状态作为时序锚点。

**[TRAP]** 用 `[CLEAR_DEBUG]` 日志诊断 TUI 渲染顺序问题：`committed_before/after` + `current_turn_before/after` 精确记录了每次 dispatch 的 committed 和 current_turn 长度变化，是定位时序问题的唯一可靠依据。

**[TRAP]** ACP SDK `meta` 字段标注 `#[serde(rename = "_meta")]`（含下划线），序列化后的 JSON key 是 `"_meta"` 而非 `"meta"`。`is_session_replay` 等关键分支检测必须用四级 fallback（`_meta` → `meta` → `content._meta` → `content.meta`），否则 replay 事件 `periReplay=true` 标记永远检测不到，走流式路径设 `is_loading=true` 后无 `TurnDone` 兜底，loading 永久卡死。（详见 spec/global/domains/tui.md#issue_2026-07-09-history-session-switch-loading-freeze）

**[TRAP]** session replay 必须复用正常流式路径的数据结构（`CommittedAssistantText`/`ReplayToolStarted`/`ReplayToolEnded`），不要发明 replay 专用 DTO 变体——那会绕过后端逻辑。6 个已验证坑：`_meta` 序列化 key 不匹配、`BaseMessage::Tool` 是 `Text(String)` 非 `Blocks`、AI 消息两个工具调用来源需去重、空工具输出被跳过、`content_hash` 不一致、`agent_thought_chunk` 缺 replay 处理。（详见 spec/global/domains/tui.md#issue_2026-07-08-history-replay-missing-tool-interactions）
