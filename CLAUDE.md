# CLAUDE.md

## v2 架构状态（2026-06-30）

**当前状态**：**v2 stages 单路径架构（完全清理 + 异步事件回路打通 + TUI MessagePipeline 单一数据源 + ACP/TUI 三层契约就绪 + TUI 状态机骨架就位 + B3 Cutover 完成 + 流式输出/复制修复）**。v1 `ReActAgent` / `executor/` 目录 / `State` trait / `CompactMiddleware` / v1 `MessageQueue` 已物理删除，所有执行路径（main agent / SubAgent / Hook / Workflow）统一通过 v2 `run_react_loop` 驱动。异步事件（cron/channel/workflow/bg_results）通过共享 v2 MessageQueue + TUI polling 接收方主动续跑形成完整回路。TUI MessagePipeline 重构为 `transcript + Option<PartialAiMessage>` 单一数据源架构，v2 stages 在迭代边界 emit `TurnCompleted` 携带全量 transcript 快照，commit_iteration 用替换语义吸收——修复多迭代场景下文本渲染在工具之前的顺序 bug（详见 `docs/design/peri-tui-message-pipeline-v2.md`）。

### 已完成（ultracode v2 重构批次：Workflow A/B1/B2/B3）

通过 5 个并行 workflow 完成 ACP/TUI 三层契约 + TUI 状态机骨架。**3049 测试全过，0 失败，19 ignored**。

- ✅ **Workflow A（P0 + P4a foundation）**：创建 `peri-acp-types` 纯 DTO crate（仅依赖 serde）—— 7 变体 ViewModel + 22 个 event_data 结构 + 16 个 summary 类型；ACP `event/router.rs`（740 行，16+ ExecutorEvent 变体映射到 `{event, data}`）+ `event/view_mapper.rs`（1114 行，含增量缓存 + DiffBlock Hunk/HunkLine 细粒度渲染）；`dispatch/execute_command.rs` + `dispatch/prompt.rs` slash 命令单入口。DTO 重导出兼容层保留至 P4b 完整切换。
- ✅ **Workflow B1（P1 event loop skeleton）**：TUI `runtime/` 模块（event_channel + keyboard_collector + acp_notifier + main_loop + effect + apply_context）—— 单一 unbounded channel 合并 5 类输入源（Key/Mouse/Paste/Resize/Tick/AcpEvent/AcpDisconnected/SessionLoaded/Shutdown），双后台 task（crossterm poll + ACP 通知）→ main_loop；`main.rs` 启动序列切换至 `runtime::main_loop::run`；polling.rs 从 9 队列删至 1（仅保留 ACP notification 接收）。
- ✅ **Workflow B2（P1.5 async migration）**：Agent-owned async—— `SessionInbox` + `await_wake`（**非破坏性**，仅 wake.notified + has_wake_up 重检，不 drain）+ `CronOwner` / `ChannelOwner` / `AsyncRouter`；Session 持有 inbox + owners；ACP executor 末尾**不**加 drain_for_end 循环（S6 trap 规避）。TUI 侧 `drain_for_end` / `channel_notification_rx` / `pending_continuation` 全部删除（grep 0 结果）。
- ✅ **Workflow B3（P2 state machine 核心）**：TUI `state_machine/` 模块 4576 行—— State 4 变体（Idle/Streaming/Modal/Switching）+ Event 9 变体（含 AcpEventData 22 子变体 + Unknown 兜底 + decode 函数）+ ViewStore（**替换语义** commit，非 extend）+ CurrentTurn（流式累积 text/reasoning/tool_cards/spinner）+ InputState（buffer+cursor+history+at_mention+slash+attachments）+ 4 个 transitions（idle/streaming/modal/switching）+ Handler trait + 4 个 handler stubs（hitl/ask_user/rewind/oauth）+ PanelState/PanelEffect/PanelReadContext trait 基础设施。**84 个 state_machine 测试全过**，纯函数 `(State, Event) → (State, Vec<Effect>)` 零 I/O。

### 已完成（P1–P5 + 完全清理 + P2-B + P2-C）

**当前状态**：**v2 stages 单路径架构（完全清理 + 异步事件回路打通 + TUI MessagePipeline 单一数据源）**。v1 `ReActAgent` / `executor/` 目录 / `State` trait / `CompactMiddleware` / v1 `MessageQueue` 已物理删除，所有执行路径（main agent / SubAgent / Hook / Workflow）统一通过 v2 `run_react_loop` 驱动。异步事件（cron/channel/workflow/bg_results）通过共享 v2 MessageQueue + TUI polling 接收方主动续跑形成完整回路。TUI MessagePipeline 重构为 `transcript + Option<PartialAiMessage>` 单一数据源架构，v2 stages 在迭代边界 emit `TurnCompleted` 携带全量 transcript 快照，commit_iteration 用替换语义吸收——修复多迭代场景下文本渲染在工具之前的顺序 bug（详见 `docs/design/peri-tui-message-pipeline-v2.md`）。

### 已完成（P1–P5 + 完全清理 + P2-B + P2-C）

- ✅ **P1**：`trait Middleware` 移除 `<S: State>` 泛型，改为 `MiddlewareContext` / `MiddlewareContextMut`（commit 98626062）
- ✅ **P2**：v2 stages 真实化——`reason/act/compact/receive/end` 全部接入 LLM + 工具分发 + middleware 钩子（commit 177cc517）
- ✅ **P3**：ACP executor v2 路径默认开启（`build_and_execute_agent_v2` + 9 个 Phase）
- ✅ **P4**：ACP DTO 层（`CompactFileInfoDto` / `WorkflowProgressDto` / `TokenUsageDto` / `TodoItemDto` 等），TUI 完全使用 DTO
- ✅ **P5.1**：SubAgent 4 文件迁移 v2 stages（`define / execute_bg / execute_fork / spawner` + `v2_bridge`）
- ✅ **P5.2**：Hook executor 迁移 v2 stages
- ✅ **P5.3**：抽取 `AgentComponents`，让 v2 builder 与 v1 ReActAgent struct 解耦
- ✅ **P5.5a–e**：删除 `PERI_USE_V1` 双轨 + 物理删除 v1 `executor/` 目录（7 文件）+ 清理 ReActAgent 注释 + 迁移 v1 测试/examples + builder.rs 直接构造 MiddlewareChain（不再调 `into_parts()`）
- ✅ **完全清理**（commit c49db28b + 后续）：删除 `trait State` / `AgentState` 的 State trait 残留 / `CompactConfig::from_env` 死代码 / `AgentEvent → ExecutorEvent` 重命名 / v1 `MessageQueue` / `CompactMiddleware`（自动 compact 改由 `stages/compact.rs` 统一处理） / `compact/{full,micro,re_inject,invariant}.rs` v1 实现物理删除 / 注释残留全部反映 v2 单路径
- ✅ **P2-B（异步事件回路）**：S3 push v2 queue + polling.rs drain 形成完整回路。cron/channel/workflow/bg_results 等异步事件通过共享 v2 `MessageQueue` 注入；TUI 的 `poll_agent` 在 agent idle 时 `drain_for_end` 取出 Prompt/Defer 并 `submit_message` 发起新一轮（接收方主动续跑）。撤销了 S6 在 ACP executor 内引入的 drain-only 循环回归（该循环只 drain 不续跑，导致 idle 期间到达的消息被物理丢弃）
- ✅ **P2-C（TUI MessagePipeline 单一数据源）**（commit 42a60a1a）：删除 `completed: Vec<BaseMessage>` + 5 个 `current_ai_*` 字段双状态，重构为 `transcript + Option<PartialAiMessage>`。v2 stages 在 `act.rs` 双路径（工具路径 + 最终回答路径）emit `StateEvent::TurnCompleted` 携带 `finalized_messages: Arc<Vec<BaseMessage>>`，跨四层（peri-agent `ExecutorEvent::TurnCommitted` → peri-acp `AcpEvent::TurnCommitted` → peri-tui `AgentEvent::TurnCommitted`）透传。`commit_iteration` 用**替换**语义（非 extend）吸收全量快照，`build_tail_vms` 重构为纯函数——流式渲染与历史恢复走同一路径（`restore_completed` ≡ `commit_iteration`）。修复多迭代场景下文本渲染在所有工具之前的顺序 bug（详见 `docs/design/peri-tui-message-pipeline-v2.md`）
- ✅ **B3 Cutover（v2 事件分发单路径化）**：**`thin_handle` 函数物理删除**（原 376 行 glue code → 0 行）。`main_loop::run` 采用双路径架构：
  - **1a. 纯状态机路径**：`TuiEvent → SmEvent → state_machine::handle → (State, Vec<Effect>)`，处理所有可纯函数化的事件（Key 快捷键/BackTab/Enter/Esc、Mouse、Paste、Tick、Resize、AcpDisconnected、Shutdown）。
  - **1b. Legacy 键盘兜底**：`keyboard::handle_key_event` 仅处理状态机未覆盖的复杂 UI 交互（panel 分发、popup、setup wizard、textarea 输入、@mention/slash hint、history）。`is_sm_handled_shortcut()` 过滤 BackTab/Ctrl+T/Ctrl+Shift+T/Ctrl+B/Ctrl+O 防止双重执行。
  - **1c. 合并去重**：Render 效应去重后统一执行。**键盘模块已是最后剩余 `&mut App` 消费者**，隔离良好——完整 Effect-ization 需 50+ 新变体，暂不推进。
  - `Effect` 枚举当前 26 变体（`Render` / `SubmitMessage` / `PollAgent` / `AdvanceSpinner` / `Scroll` / `MouseTextareaClick` / `MouseTextareaDrag` / `MouseRelease` / `SendToAcp` / `CopyToClipboard` / `PasteText` / `ShowNotification` / `UpdateConfig` / `SwitchSession` / `OpenPanel` / `ClosePanel` / `CycleModel` / `CycleProvider` / `CyclePermissionMode` / `FocusBgBar` / `ToggleDiff` / `PollWorkflow` / `ClearTextSelection` / `PushSystemNote` / `MemoryPanelOpenEditor` / `Quit`）。
  - `idle.rs` 26 测试 + `streaming.rs` 8 测试 + `switching.rs` 5 测试 + `modal.rs` 22 测试。SM transitions 合计 162 测试全过。
  - `apply_context.rs` 防御性 no-op 分支随 Effect 扩展同步更新。
  - **Phase 1.3-1.4 v2 Interaction Handlers 就位**：4 个 Handler（hitl/ask_user/rewind/oauth）render + desired_height 实现（commit `da9bce2a`/`654d5999`/`daba127d`/`89738a7e`/`ee9fe41c`）。draw_now 中 v2_panel_height match 双分支（Panel/Interaction）。**[已知缺陷]** 当前 ACP 层不 emit HitlPending/AskUser/RewindPreview/OauthNeeded AcpEvent（mapper.rs:265 注释），HITL 走 v1 RequestPermission JSON-RPC；v2 路径作为前端预留接口等 ACP 层扩展后启用（详见 `docs/refactor/tui-v3-plan.md`）。
- ✅ **B3 Additional（Setup Wizard/Bar Focus/Popups/AcpEvent 完整保留）**：`setup_wizard.rs`（82 行）、`bar_focus.rs`（89 行）、`popups.rs`（138 行）、`handle_acp_event`（~120 行）全部保留——这些路径本质上是 UI 状态转换（非纯函数），通过 keyboard dispatch 或 direct call 执行，与状态机互不干扰。
- ✅ peri-tui 998 测试全过（含 v2 state_machine handlers 测试），`cargo build --workspace` 绿，`grep -r 'ReActAgent'` 零结果
- ✅ **Bug Fix：流式输出不显示（多轮迭代）**：v2 多轮 ReAct 循环中，`streaming_suppressed` 在 TurnCommitted 后设为 `true`，仅 TurnDone→Idle 时重置。但 ViewCommit 在迭代间会重置 `current_turn`，下一轮 TextChunk 到达时 `streaming_suppressed` 仍为 `true`，P5 桥接因此清空流式文本而非填充。**修复**：新增 `Effect::ResumeStreaming`，ViewCommit 处理器发出此 effect → main_loop 重置 `streaming_suppressed = false`（`streaming.rs` / `main_loop.rs` / `effect.rs` / `apply_context.rs`）。
- ✅ **Bug Fix：复制功能偶发失败（~50% 概率）**：`keyboard_collector.rs` 中 `tokio::select!` 竞态条件——`spawn_blocking`（crossterm poll 50ms）和 `tick_interval.tick()`（50ms）同时就绪时 `select!` 随机选一个。tick 分支获胜时 `JoinHandle` 被丢弃，但后台任务继续运行并调用 `event::read()` 消费了 crossterm 事件，结果无人接收 → 事件永久丢失。MouseUp 事件丢失导致 `copy_selection_to_clipboard` 从未被调用。**修复**：重构为持久化 `spawn_blocking` 任务（独立轮询 crossterm → mpsc channel），异步循环通过 `select!` 从 poll-based `recv()` 读取——不再有 detached task（`keyboard_collector.rs` 重写）。

### ultracode 未完成（按优先级）

- ✅ **B3 Cutover（最高优先级）**：**已完成** — `thin_handle` 物理删除，状态机 + keyboard 双路径架构就位。`is_sm_handled_shortcut()` 防止双重执行。键盘模块完整保留（6 文件，~1200 行）作为复杂 UI 交互兜底。
- ⏳ **B3 MigrateInput**：`UiState.textarea` 等输入字段未迁到 `State::Idle.input`。依赖 keyboard 模块 Effect-ization（文本框每击键都需 Effect 通路），风险高，延后至 P5 渲染重写阶段。
- ✅ **Workflow C（P3 面板重写 + Phase E 删除 + Phase C ServiceRegistrySnapshot）**：**14/14 PanelState 迁移完成**（commit 4cd631bd）。全部 14 个面板从 legacy `PanelComponent` trait 迁移到 v2 `PanelState` trait，完全脱离 `peri_middlewares::*` / `crate::config::*` 运行时依赖（改用本地 DTO + `acp_query_cache`）。`registry::create_panel()` 工厂 0 stub。**P3 Integration 已完成**（commit 0b69d271 + 50c5624c）：`Effect` 枚举扩展 3 个变体（ShowNotification / UpdateConfig / SwitchSession），`modal.rs::map_panel_effects` 全 6 个 PanelEffect 变体映射就绪；19 个集成测试覆盖 14 面板 × 6 事件全量回归 + 完整 PanelEffect→Effect 映射。**Phase E legacy 删除已完成**：~55 文件（`app/*_panel.rs` / `ui/main_ui/panels/*.rs` / `PanelComponent` trait / `PanelManager` / `event/keyboard/panels.rs` / `event/macros.rs`）物理删除，-16,812 行。**Slash 命令 → v2 面板端到端通路已打通**：`Command::execute` 返回 `Vec<Effect>` → `Effect::OpenPanel(PanelKind)` → main_loop `create_panel(kind, app)` → `State::Modal(Panel)` → `draw_now` overlay 渲染。8 个面板 `from_app(app)` 构造函数从 `ServiceRegistry` 提取真实数据（Model/Login/Config/Mcp/Plugin/Cron/Status/Workflow）。**Phase C ServiceRegistrySnapshot 真实化已完成**：`ServiceRegistrySnapshot` 新增 `cwd`/`model_alias`/`provider_name`/`permission_mode` 字段 + `from_app(&App)` 工厂；`build_v2_panel_read_context` 使用真实 ServiceRegistrySnapshot + State.view_models() 数据（PanelReadContext.view_models 不再为 `&[]`）。StatusPanel Cost/Context 标签页展示真实 provider/model 数据。
- ✅ **Workflow D（P4b 类型隔离）**：**已完成**。全部 14 个 v2 面板零 `peri_middlewares` 代码导入（仅注释残留）。dto_convert.rs 桥梁层就绪（MCP/MCP 运行时/OAuth/PermissionMode/SkillMetadata/InstallScope 全量 From/DTO 转换）。**剩余 26 处活跃导入全部分类为合理运行时依赖**：HITL/AskUser 通道（8 处，oneshot channel 桥接）、PermissionMode 运行时（2 处，SharedPermissionMode）、MCP 运行时对象（3 处，McpClientPool/McpInitStatus/OAuthFlowEvent）、CronScheduler（1 处，运行时调度器）+ CronTask（1 处，from_app 转换）、acp_stdio/acp_server 后端路径（10 处，合理保留）、oauth_prompt（1 处，parse_code_from_url）。
- ⏳ **Workflow E（P5 渲染重写）**：`message_pipeline/` 18KB + 82KB 测试未删；双线程渲染 + RenderCache + AdaptiveChunkingPolicy 未删；渲染入口未切换到 `State.view + current_turn`。工作量大，风险高。

### v2 单路径架构

**所有执行路径**：`run_session_loop` → `build_and_execute_agent_v2` → `run_react_loop`（v2 stages）
- main agent：ACP executor `build_and_execute_agent_v2`
- SubAgent（fork / background / define）：`build_v2_subagent_context` + `run_react_loop`
- Hook executor：v2 stages
- Workflow agent：`WorkflowAgentExecutor` + v2 stages

**ReAct 循环**（`peri-agent::agent::stages`）：每轮 `Compact → Receive → Reason → Act → End`：
- **Compact**：检查 `ContextBudget`，按 0.70 / 0.85 阈值触发 micro / full compact（`compact_v2::run_compact`）；Full Compact 后 reset token_tracker
- **Receive**：从 `MessageQueue` 取出 Prompt + Info 消息写入 `MessageTranscript`
- **Reason**：`before_model → LLM → after_model`，emit `LlmCallStart/End`
- **Act**：3 阶段工具分发（`before_tools_batch → 并发 invoke → after_tool × N → after_tools_batch`）
- **End**：检查 `MessageQueue` 是否有 Defer/Prompt，决定是否续跑下一轮

### 异步事件回路（P2-B）

异步事件触发 agent 续跑的两条路径：

- **Agent 运行期间**（loading=true）：异步事件 push 到 v2 queue → stages/end.rs `should_continue` → 下一轮 `drain_for_receive` 自动消费（同一 run_session_loop 内）。
- **Agent idle 期间**（loading=false）：异步事件 push 到 v2 queue → TUI `poll_agent` 在下一帧 `drain_for_end` 取出 → `submit_message` 发起新一轮 `run_session_loop`（接收方主动续跑，详见 `peri-tui/src/app/agent_ops/polling.rs`）。

**[TRAP]** ACP executor 末尾**禁止**加 `drain_for_end` 循环：`drain_for_end` 是 destructive，取出后若不续跑消息会物理丢失。idle 期续跑必须由 TUI 接收方负责（见 S6 回归修复）。

**[TRAP]** `run_session_loop` 末尾**禁止**加 `await_wake` 阻塞。ReAct 循环的 End 阶段已经在 queue 为空时正确退出（`stages/end.rs`），executor 拿到 `LoopResult` 后职责已结束，应立即 return `PromptResult`。加 `await_wake` 会导致 stdio/IDE 路径的 `responder.respond(PromptResponse)` 永远不执行，客户端（如 Zed）收不到完成响应而一直 loading（见 2026-06-30 await_wake 删除）。

### TUI MessagePipeline 单一数据源（P2-C）

TUI 渲染管线的核心架构。规范状态 `transcript: Vec<BaseMessage>` + 当前迭代增量 `partial: Option<PartialAiMessage>` 两类状态，视图派生是纯函数 `view = messages_to_view_models(transcript) ⊕ partial_bubble`。

**迭代边界显式提交**：v2 stages 在 `act.rs` 双路径（工具路径 `commit_staged` 后 / 最终回答路径 `append` 后）emit `StateEvent::TurnCompleted { finalized_messages: Arc<Vec<BaseMessage>>, .. }`，跨四层透传到 TUI 的 `AgentEvent::TurnCommitted`。`commit_iteration` 用**替换**语义（`self.transcript = msgs`）吸收全量快照——非 extend，避免多次 commit 让 transcript 翻倍。

**双路径统一**：流式渲染与历史恢复走同一路径。`restore_completed(msgs)` 与 `commit_iteration(msgs)` 同构，都执行 `transcript = msgs; partial = None;`。`build_tail_vms` 是纯函数，从 transcript 切片（round 起点之后）+ partial 派生 VMs——partial 内容天然追加在 transcript 之后，时序正确。

**[TRAP]** `commit_iteration` / `restore_completed` **必须用替换语义**：v2 的 `finalized_messages` 是全量快照而非增量，extend 会让 transcript 在多次 commit 后翻倍累积（旧 `set_completed` extend 语义在 v2 多迭代场景下污染历史）。

**[TRAP]** Pipeline `handle_event` **永远不返回 `RebuildAll`**——Pipeline 不持有 VM 索引维度（`round_start_vm_idx`）。重建由 `agent_ops` 通过 `build_rebuild_all(prefix_len)` 显式触发，避免 BaseMessage 维度与 VM 维度混淆。

**[TRAP]** `TurnCommitted` / `StateSnapshot` 在 `in_subagent() == true` 时**必须直接返回 None**——子 Agent 的迭代提交不应污染父 Agent 的 transcript（否则子 Agent 全部内部消息会混入父 Agent 历史）。

### TUI 双路径事件分发（B3 Cutover）

`main_loop::run` 的 3 阶段事件处理：

```
TuiEvent → 1a. state_machine::handle (纯函数) → (State, Vec<Effect>)
        → 1b. keyboard::handle_key_event (兜底) | handle_acp_event (桥接) → Vec<Effect>
        → 1c. 合并去重 (Render 唯一化)
        → 2. 执行 Effects (I/O + App mutation)
        → 3. 渲染 (Tick 节流 33ms, 其余立即绘制)
```

**1a. 状态机路径**（`state_machine::handle`）：纯函数 `(State, Event) → (State, Vec<Effect>)`。处理：
- 快捷键：BackTab/Ctrl+T/Ctrl+Shift+T/Ctrl+B/Ctrl+O → Effect::CyclePermissionMode/CycleModel/CycleProvider/FocusBgBar/ToggleDiff
- 文本操作：Ctrl+A/Ctrl+U/Ctrl+W → InputState 方法 → Effect::Render
- 输入：Enter（提交）→ Effect::SubmitMessage, Esc（Rewind）→ Effect::OpenRewindPrompt
- Mouse：Scroll → Effect::Scroll, Click/Drag/Release → Effect::MouseTextarea*
- Paste：→ Effect::PasteText
- Tick：→ Effect::AdvanceSpinner + PollAgent + PollWorkflow + Render
- Resize：→ Effect::ClearTextSelection + Render
- AcpDisconnected/Shutdown：→ Effect::PushSystemNote / Quit

**1b. Legacy 兜底**：`is_sm_handled_shortcut()` 过滤已由状态机覆盖的快捷键，剩余事件走：
- `keyboard::handle_key_event(app, key)`：6 文件 ~1200 行，处理 panel 分发/popup/setup wizard/textarea 输入/@mention/slash hint/history
- `handle_acp_event(app, event, data)`：JSON → AcpNotification 桥接，委托 `app.handle_acp_notification()`

**[TRAP]** `is_sm_handled_shortcut()` 必须与 `idle.rs` 的 Ctrl+Char 分支保持同步——增删快捷键时两边都要更新。

**[TRAP]** `keyboard_collector.rs` **禁止用 `tokio::select!` 同时竞态 `spawn_blocking`（crossterm poll）和 tick interval**。两者都是 50ms 就绪，`select!` 随机选一个。tick 分支获胜时 `JoinHandle` 被丢弃但后台任务继续运行——它调用 `event::read()` 消费事件，结果无人接收，事件永久丢失。修复方案：持久化 `spawn_blocking` 任务 → mpsc channel → `select!` 从 poll-based `recv()` 读取（见 2026-06-30 修复）。

### 关键架构点

- **`builder::build_agent`**：直接构造 `MiddlewareChain`（不再构造 ReActAgent），产出 `AgentComponents { llm, chain, shared_tools, error_suggest_registry, tool_registry_snapshot, system_prompt, context_budget, compact_config }`。
- **`builder_v2::build_stage_context`**：消费 `AgentComponents`，并显式调用 `chain.collect_tools(cwd)` 把 middleware 提供的工具 + `register_tool` 注册的 `AskUserQuestion` 注入到 `shared_tools`（替代 v1 `executor.execute()` 内部的每轮 clear + repopulate）。
- **`Session::new_with_cancel`**：v2 Session 持有 linked `CancellationToken`，父级 cancel 时传播。
- **EventBus 3 层事件**：`render_event_to_executor` / `state_event_to_executor` / `observe_event_to_executor` 将 v2 事件映射为 `ExecutorEvent`，转发到现有 event_tx。
- **TUI DTO 化**：TUI 仅消费 `AcpEvent` DTO，不再依赖 `peri_middlewares::tools::todo` 等运行时类型（`BaseMessage` 等类型依赖按 CLAUDE.md「类型依赖允许」保留）。
- **Compact 由 stages 处理**：v2 `stages/compact.rs` 在 `run_react_loop` 每轮开头检查 budget + 调 `compact_v2::run_compact`，不再经过 `CompactMiddleware`。`/compact` 命令路径也复用 `compact_v2::run_compact(force=true)`（见 `peri-acp/src/session/command/compact/pipeline.rs`）。
- **v2 queue 作为统一异步消息通道**：cron/channel/workflow/bg_results 等异步事件通过 `AcpSession::v2_queue_for(session_id)` 拿到共享 `MessageQueue` clone 并 push（Kind::Defer）。TUI polling 在 agent idle 时 drain_for_end 取出并 submit_message 续跑；agent 运行时则由 stages/end.rs → drain_for_receive 自动消费。两条路径通过 `loading` 状态互斥，无冲突。

### AgentCancellationToken 保留说明

v1 `executor/mod.rs` 删除后，`pub use tokio_util::sync::CancellationToken as AgentCancellationToken` 迁移到 `agent/mod.rs`。众多模块（ACP / SubAgent / Workflow）依赖此类型名，保留 alias 避免大规模 rename。


## Workflow 故障排查（优先检查）

Workflow 出现 "0 agents, 0 tool calls" 或启动即失败时，按顺序检查：

1. **peri-workflow binary 存在且可用**：`which peri-workflow` 能找到，`head -1 $(which peri-workflow)` 是 `#!/usr/bin/env node`
   - 不存在：`cd npm-packages/@peri-workflow && npm install && npm run build && npm install -g --prefix ~/.npm-global .`
   - 确保 `~/.npm-global/bin` 在 PATH 中（`export PATH="$HOME/.npm-global/bin:$PATH"` 加入 `~/.zshrc`）
2. **Rust 编译通过**：`cargo build -p peri-workflow -p peri-acp` 无错误，修改 `peri-workflow/src/tool.rs` 后尤其注意 `watch::channel` 的 `changed()` 需要 `&mut self`
3. **重启 Peri TUI** 使新 binary 生效

## Crate 总览

9 个 Workspace Crate：

| Crate | 职责 |
|-------|------|
| `peri-agent` | 核心：ReAct 循环、Middleware trait、LLM 适配器、工具系统、持久化 |
| `peri-middlewares` | 19 个中间件（FS/终端/HITL/SubAgent/Skills/Todo/Cron/MCP/Hooks/Plugin/LSP） |
| `peri-widgets` | Widget 组件库（ratatui + pulldown-cmark） |
| `peri-acp` | ACP 服务层：MpscTransport/StdioTransport 桥接 TUI/IDE 与 Agent |
| `peri-tui` | TUI 应用（纯 ACP client 前端，类型依赖 peri-agent/middlewares/widgets） |
| `langfuse-client` / `peri-lsp` / `peri-web-pty` | 独立基础库（遥测 / LSP / Web PTY） |
| `agm` | Agent Package Manager（agm.json 管理 Skills/Agents） |

依赖方向：`peri-tui` → `peri-acp`（运行时）→ `peri-agent`/`peri-middlewares`；`peri-middlewares` → `peri-agent`/`peri-lsp`。TUI 运行时仅通过 `MpscTransport` 与 ACP Server 通信。

## 开发命令

- `cargo run -p peri-tui -- -a`：HITL 审批模式
- `scripts/start-tui.sh`：启动 TUI（RELAY_PORT=3001）
- `lefthook run pre-commit`：pre-commit（fmt/check/clippy）
- `cargo test -p <crate> --lib -- <test_name>`：单个测试

## 核心文件树

```
peri-agent/src/
├── agent/
│   ├── stages/{mod,reason,act,compact,receive,end,tool_dispatch,middleware_runner}.rs  # v2 ReAct 循环（单路径）
│   ├── compact/{full,micro,re_inject,config,invariant}.rs                # 上下文压缩
│   ├── compact_v2.rs                                                    # v2 compact 入口
│   ├── react.rs    # ReactLLM trait + Reasoning / ToolCall / ToolResult
│   ├── state.rs    # AgentState（middleware_runner 桥接工作区）
│   ├── events.rs   # AgentEvent 枚举（v2 EventBus 转发）
│   └── events_v2.rs # v2 三层事件（Render / State / Observe）
├── llm/
│   ├── {openai,anthropic}/invoke.rs   # 请求构造 + Provider 特定处理 + System hoist
│   ├── react_adapter.rs               # BaseModel → ReactLLM
│   └── retry.rs                       # RetryableLLM
├── messages/{message,content}.rs      # BaseMessage / ContentBlock（含 Reasoning）
├── middleware/                        # Middleware trait + Chain
├── error_suggest/                     # 错误建议基础设施（trait/registry/context）
└── interaction/multiplex.rs           # MultiplexBroker

peri-middlewares/src/
├── tool_search/{core_tools,search_tool,execute_tool}.rs   # Core/Meta/Deferred 工具
├── error_suggest/suggesters/          # 具体建议器
├── subagent/{mod,tool/}               # SubAgent 中间件 + 构建器
├── skills/                            # Skills 加载（含 builtin/）
├── hooks/middleware.rs                # stop_hook_feedback
├── hitl/mod.rs                        # is_edit_tool + 审批列表
├── process/mod.rs                     # shell_command 跨平台 spawn
├── agents_md/                         # CLAUDE.md 加载（frozen 透传终点）
└── tools/filesystem/                  # Read/Write/Edit/Glob/Grep/folder

peri-acp/src/
├── session/
│   ├── executor.rs        # execute_prompt() 统一入口
│   ├── frozen.rs          # SessionState.frozen_* 会话内不可变数据
│   ├── state_builders.rs  # build_config_options()
│   ├── event_sink.rs      # TransportEventSink
│   ├── agent_pool.rs      # 大对象 session 级缓存
│   └── command/{compact,rewind,bg,clear}.rs   # Slash Commands
├── agent/builder.rs       # build_agent() 每轮重建
├── prompt/mod.rs          # build_system_prompt() + 边界标记
├── event/mapper.rs        # ExecutorEvent → AcpNotification（ToolKind 映射）
├── transport/{mpsc,stdio}.rs
├── dispatch/{init,...}.rs # JSON-RPC 方法分发
└── provider/              # Provider 配置 + 快照

peri-tui/src/
├── app/
│   ├── mod.rs             # App（ServiceRegistry + GlobalUiState）
│   ├── panel_component.rs # PanelComponent trait
│   ├── field_textarea.rs  # 主输入框（光标/buffer 后处理）
│   └── ...                # panel_*/agent_*/history_* 等 50+ 模块
├── event/{mod,mouse,macros}.rs + keyboard/   # 事件循环 + 按键
├── command/{core,session,panel}.rs           # Slash command 注册
├── ui/
│   ├── main_ui/mod.rs     # 主布局（光标 vs buffer 后处理）
│   └── message_view/      # 消息渲染
├── sync/{scanner,writer,receiver,sender,...}.rs
├── acp_client/client.rs   # AcpTuiClient（MpscTransport 前端）
└── tool_display.rs        # 工具简称映射

peri-tui/prompts/sections/   # 13 个系统提示词段落（01-07,10-15）
```

## 架构要点

**ReAct 循环**（`peri-agent`）：`before_agent → loop(500) { before_model → LLM → after_model → [before_tool → 并发执行 → after_tool → emit] | [回答 → emit TextChunk + StateSnapshot → after_agent] }`。TUI 覆盖 `max_iterations(500)`（核心默认 10）。

**消息类型**：`BaseMessage`（Human/Ai/System/Tool），`ContentBlock`（Text/Image/Document/ToolUse/ToolResult/Reasoning/Unknown）。`Reasoning` 携带 Anthropic thinking 签名。

**LLM 适配层**：`BaseModel` trait → `BaseModelReactLLM` → `ReactLLM`，外层 `RetryableLLM<L>` 指数退避。

**系统提示词**：`build_system_prompt()`（`peri-acp/src/prompt/mod.rs`）在 `session/new` 调用一次产出 `frozen_system_prompt`，后续复用。13 个段落文件在 `peri-tui/prompts/sections/`（01-07,10-15），`__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 分隔静态/动态区域。

**[TRAP]** 中间件 `before_tool`/`after_tool`/`on_error` 均不读 `state.messages()`——`tool_dispatch.rs` 延迟写入要求 collect/dispatch 两阶段间 state 不被读取。`AgentEvent::MessageAdded` 被 TUI 丢弃。

**[TRAP]** 新增/修改 `AgentEvent` 变体时必须同步更新 TUI 侧 `map_executor_event` 映射，事件丢弃导致下游状态不一致。

**[TRAP]** `Interrupted`/`Error` 与 `Done` 互斥：前者先 `request_rebuild()`+设 `reconcile_already_done=true`，后者跳过。Cancel 后 `result.ok==false` 时检查 `result.messages.len()` 判断有无进展，有则保留历史。

## 上下文缓存（第一优先）

会话开始后系统提示词不可变更——任何变化导致 Prompt Cache 失效 + 模型行为漂移。动态区域（边界标记之后）的占位符值可变，但结构/段落数量必须会话内不变。

**[TRAP]** 中途纠正消息（工具失败提示、`<stop_hook_feedback>`、goal steering、compact 续接）必须用 `BaseMessage::human(...)` 注入（`<system-reminder>` 或 `<goal-message>` 标签），**禁止** `BaseMessage::system(...)`——invoke.rs 会把所有 System 消息 hoist 到顶层，污染 frozen prompt。已采用：`goal_middleware.rs`、`compact_v2.rs::re_inject_v2`、`tool_dispatch.rs`、`hooks/middleware.rs`。

**[TRAP]** SubAgent 中间件链必须复用 main agent `session/new` 时 frozen 的 CLAUDE.md/Skills 数据，禁止重新读盘。透传链：`SubAgentMiddleware::with_frozen_data` → `SubAgentTool` → `build_subagent_middlewares` → `AgentsMdMiddleware::with_frozen_content` / `SkillsMiddleware::with_frozen_summary`。

**[TRAP]** Prompt Cache 前缀稳定性：非 System 消息必须用 `add_message`（尾部追加），禁止 `prepend_message`（改变 cache_control 标记位置）。动态占位符放边界标记之后。

## Tool Search 延迟加载

三层：**Core（12 个，始终可见）** Read/Write/Edit/Glob/Grep/folder_operations/Bash/WebFetch/WebSearch/Agent/AskUserQuestion/TodoWrite；**Meta（2 个）** `SearchExtraTools`/`ExecuteExtraTool`；**Deferred** Cron/MCP/LspTool 等（LLM 不可见，Meta 桥接）。定义在 `tool_search/core_tools.rs::CORE_TOOLS`。新增工具优先配为 deferred。

**Builtin Skills**：`SkillSource::Builtin` 第 5 种来源（最低优先级），`include_str!` 编译期嵌入，`disableBundledSkills: true` 全局禁用。

### 新增/删除 Core 工具检查清单

1. `tool_search/core_tools.rs` —— `CORE_TOOLS` + `TOOL_*` 常量
2. `05_using_tools.md` —— 用户可见工具指引
3. `hitl/mod.rs` —— `is_edit_tool()` + 审批列表
4. `event/mapper.rs` —— `ToolKind` 映射
5. `tool_display.rs` —— TUI 简称映射
6. `core_tools_test.rs` —— CSV 断言
7. Edit/Write 类额外检查 `GitAttributionMiddleware` 钩子

## 中间件链

详见 `peri-middlewares/CLAUDE.md`。19 个中间件固定顺序，末尾 `with_system_prompt()` prepend。

## 错误感知建议层

工具错误返回前通过 `ErrorSuggestRegistry` 注入建议文本（路径候选/参数修正）。集成在 `tool_dispatch.rs::collect_tool_results`（run_after_tool 之后、写 state 之前，非中间件）。基础设施在 `peri-agent/src/error_suggest/`，suggester 在 `peri-middlewares/src/error_suggest/suggesters/`。

新增建议器：`<name>_suggester.rs` → 实现 `ErrorSuggester` trait → `default_registry.rs::build_default_registry()` 注册。

## ACP/TUI 分层

`peri-tui` 是纯 ACP client，通过 `MpscTransport` 与 `peri-acp` 通信。TUI/Stdio 两条路径共享 `executor::execute_prompt()`。

**Frozen Data Flow**（`session/new` 一次性捕获，存 `SessionState.frozen_*`）：`frozen_date → frozen_claude_md → frozen_skill_summary → frozen_system_prompt`。每轮重新计算：`is_git_repo`、`YOLO_MODE`、compact env、`peri_config`/Provider snapshot、中间件链/AgentState/Cancel Token。

**[TRAP]** `PromptFeatures::detect()` 仍每轮读取 `YOLO_MODE`/`is_git_repo`，未 frozen——SubAgent 可能与 Main Agent 漂移。

**Slash Commands**：`/compact`（full compact）、`/rewind <id>`（回滚消息+文件变更）、`/bg <任务>`（后台 Fork Agent）。均为 `CommandKind::Immediate`。

**[TRAP]** Immediate 命令绕过 agent event pump，必须手动 `sink.push_done()`。

**[TRAP]** Agent 构建执行统一通过 `execute_prompt()`，禁止 TUI 层直接构建 Agent。TUI 数据必须走 ACP 协议，禁止直连。

## 上下文压缩

v2 `stages/compact.rs` 在 `run_react_loop` 每轮开头检查 `ContextBudget`：0.70 触发 micro-compact，0.85 触发 full compact。核心实现 `peri-agent/src/agent/compact_v2.rs`，配置 `peri-agent/src/agent/compact/config.rs::CompactConfig`。Full Compact 后 reset `token_tracker`，避免下轮 budget 计算错误。

`/compact` 命令路径复用 `compact_v2::run_compact(force=true)`，详见 `peri-acp/src/session/command/compact/pipeline.rs`。

## 环境变量

配置通过 `~/.peri/settings.json` 的 `env` 字段注入。分组：
- **Provider**：`ANTHROPIC_*`/`OPENAI_*`（API_KEY/BASE_URL/MODEL）、`MODEL_PROVIDER`
- **行为**：`YOLO_MODE`（HITL 开关）、`DISABLE_COMPACT`/`DISABLE_AUTO_COMPACT`/`COMPACT_THRESHOLD`
- **日志**：`RUST_LOG`/`RUST_LOG_FORMAT`（json）/`RUST_LOG_FILE`
- **遥测**：`LANGFUSE_PUBLIC_KEY`/`LANGFUSE_SECRET_KEY`/`LANGFUSE_BASE_URL`（缺一禁用）

## CLI

clap 4 derive，camelCase 参数同时支持 kebab-case。子命令：`plugin`/`acp`/`update`/`sync`。`-p/--print` 模式复用 ACP executor + `PrintEventSink`，不启动 TUI。运行时 `Shift+Tab` 切权限模式，`Ctrl+T` 切模型，`Ctrl+Shift+T` 切 Provider。

## 文档

`docs/blogs/`（40+ 技术博客）、`docs/superpowers/specs/`（设计规范）、`docs/acp/`（ACP 协议）、`.claude/skills/blog-writer/`（博客风格指南，写博客时触发）。

## 编码规范

- Rust 2021 + async-trait；库用 `thiserror`，应用层用 `anyhow`
- 库 crate 用 `tracing`（禁止 `println!`）；CLI 工具（agm）可用 `println!`
- 测试分离为同目录 `_test.rs`（≥30 行）；bin crate 测试在 `src/` 内
- 每模块一目录 `mod.rs` 入口；resolver = "2"
- 禁止 `ℹ`（U+2139）和 `[i]` 前缀
- **字符串截断用字符级**：`chars().take(N)`，`&s[..N]` 对 CJK panic；终端列宽用 `unicode-width`
- **快捷键**：禁止 `Shift+字母`；优先 `Ctrl+字母`（`Alt` 在 Windows 终端被截获）；不用 PageUp/Down（用 `Ctrl+U`/`Ctrl+D`）
- **面板系统**：`PanelManager` + `PanelComponent` trait，面板内禁止渲染提示行（用 `status_bar_hints()`）
- **`Event::Paste`** 独立于 key event 链，必须单独拦截

## 测试风格

- 命名 `test_<对象>_<场景>`；注释/断言用中文
- Arrange-Act-Assert 无空行；`unwrap()` 仅用于构造测试数据
- Mock 用 `make_` 前缀函数，不用 Mock 结构体；最小依赖（`tempfile` + `tokio-test`）

## 开发注意事项

- **测试隔离**：用 `App::save_config(cfg, self.config_path_override.as_deref())`，禁止写全局配置
- **`std::sync::RwLockReadGuard` 不是 `Send`**：async 中不能跨 `.await` 持有，用 `parking_lot::RwLock`
- **`App` 状态拆分**：`ServiceRegistry`（跨会话）+ `GlobalUiState`（UI 临时），dispatch 宏在 `event/macros.rs`
