# CLAUDE.md

## v2 架构状态（2026-07-01）

**当前状态**：v2 stages 单路径架构。v1 `ReActAgent` / `executor/` / `State` trait / `CompactMiddleware` / v1 `MessageQueue` 已物理删除。所有执行路径（main/SubAgent/Hook/Workflow）统一走 v2 `run_react_loop`。TUI MessagePipeline 为 `transcript + Option<PartialAiMessage>` 单一数据源，`commit_iteration` 替换语义。异步事件回路：push v2 queue → stages/end 或 TUI polling 续跑。TUI 双路径分发：state machine 纯函数路径 + keyboard legacy 兜底。B3 Cutover 已完成（`thin_handle` 物理删除）。Phase 2.6 step 7e.9 已完成——`view_messages` 字段退役。

**已完成**：P1–P5 + ultracode Workflow A/B1/B2/B3 + Workflow C（面板重写）+ Workflow D（类型隔离）+ Phase 2.6（全部）。
⏳ **未完成**：Workflow E（P5 渲染重写，risk high）、B3 MigrateInput。

### v2 单路径架构

所有路径 → `build_and_execute_agent_v2` → `run_react_loop`。ReAct 循环：每轮 `Compact → Receive → Reason → Act → End`。Compact 阈值 0.70/0.85；Reason 走 LLM + middleware hooks；Act 3 阶段工具分发；End 检查 queue 是否续跑。

**[TRAP]** ACP executor 末尾禁止加 `drain_for_end` 循环——消息物理丢失。idle 续跑由 TUI 负责。
**[TRAP]** `run_session_loop` 末尾禁止加 `await_wake`——stdio/IDE 收不到完成响应。

### TUI MessagePipeline

`transcript + Option<PartialAiMessage>` 单一数据源。`commit_iteration` 用替换语义（全量快照，非 extend）。`build_tail_vms` 纯函数派生。

**[TRAP]** `commit_iteration` / `restore_completed` 必须用替换语义——v2 的 `finalized_messages` 是全量快照，extend 会让 transcript 翻倍累积。
**[TRAP]** `TurnCommitted` 在 `in_subagent() == true` 时必须直接返回 None——子 Agent 提交不污染父 Agent transcript。

### TUI 双路径事件分发

```
TuiEvent → state_machine::handle (纯函数) → (State, Vec<Effect>)
        → keyboard::handle_key_event (兜底)
        → 合并去重 → 执行 Effects → 渲染
```

Effect 枚举 26 变体（Render/SubmitMessage/PollAgent/AdvanceSpinner/Scroll/MouseTextarea*/SendToAcp/CopyToClipboard/PasteText/ShowNotification/UpdateConfig/SwitchSession/OpenPanel/ClosePanel/CycleModel/CycleProvider/CyclePermissionMode/FocusBgBar/ToggleDiff/PollWorkflow/ClearTextSelection/PushSystemNote/MemoryPanelOpenEditor/Quit）。

**[TRAP]** `is_sm_handled_shortcut()` 必须与 `idle.rs` 的 Ctrl+Char 分支保持同步。
**[TRAP]** `keyboard_collector.rs` 禁止 `tokio::select!` 竞态 `spawn_blocking` 和 tick——事件永久丢失。修复：持久化 task → mpsc → `recv()`。
**[TRAP]** ratatui-kit 事件边界：消息区只处理鼠标滚轮 `Event::Mouse(MouseEventKind::Scroll*)`，编辑区只处理键盘编辑/导航事件。不要让消息区消费 `KeyCode::Up/Down` 来模拟滚动；滚轮必须靠 `EnableMouseCapture` 进入 Mouse 通道。`InputArea` 是手写 textarea，Up/Down 应先做多行光标上下移动，只有在首/末行时才考虑 history fallback。

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

## Workflow 故障排查

Workflow 出现 "0 agents, 0 tool calls" 时：
1. `which peri-workflow` 可用；不存在则 `cd npm-packages/@peri-workflow && npm install && npm run build`
2. `cargo build -p peri-workflow -p peri-acp` 无错
3. 重启 Peri TUI

## Crate 总览

| Crate | 职责 |
|-------|------|
| `peri-agent` | ReAct 循环、Middleware trait、LLM 适配器、工具系统 |
| `peri-middlewares` | 19 个中间件（FS/终端/HITL/SubAgent/Skills/Todo/Cron/MCP/Hooks/Plugin/LSP） |
| `peri-widgets` | ratatui + pulldown-cmark 组件库 |
| `peri-acp` | ACP 服务层（MpscTransport/StdioTransport） |
| `peri-tui` | TUI 应用（纯 ACP client） |
| `langfuse-client` / `peri-lsp` / `peri-web-pty` | 独立基础库 |
| `agm` | Agent Package Manager |

依赖方向：`peri-tui` → `peri-acp` → `peri-agent`/`peri-middlewares`。

## 开发命令

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
peri-tui/src/runtime/main_loop.rs  # TUI 主循环（双路径分发）
peri-tui/src/state_machine/       # State 4 变体 + ViewStore + CurrentTurn + InputState
peri-tui/src/app/agent_ops/       # Agent 事件处理
peri-tui/src/render/view_render.rs  # V2 ViewModel 渲染
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
- `Event::Paste` 独立于 key event 链。

## 测试风格

命名 `test_<对象>_<场景>`；注释/断言用中文。Arrange-Act-Assert 无空行。Mock `make_` 前缀，不用 Mock struct。

## 开发注意事项

- 测试隔离：`App::save_config(cfg, self.config_path_override.as_deref())`
- `std::sync::RwLockReadGuard` 不是 Send → async 跨 `.await` 用 `parking_lot::RwLock`
- App 状态拆分：`ServiceRegistry`（跨会话）+ `GlobalUiState`（UI 临时）
