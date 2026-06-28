# v2 重构 — 文件级执行计划

> 日期：2026-06-28 | 上游：`01-gap-analysis.md`、`peri-tui-architecture.md`、`peri-acp-protocol.md`
> 原则：按设计文档规格直接重写，不并存。每阶段验收标准 = 全部已有测试通过。

本文档将 gap 分析的 Phase 调整顺序（P0 → P1 → P1.5 → P2 → P3 → P4a → P4b → P5）展开为可被工作流 agent 直接消费的文件级操作清单。

---

## Execution Workflow Plan（工作流编排）

本次重构切分为 **5 个执行工作流（Workflow A–E）**，按依赖图编排。每个工作流是一个独立可派发的 agent 任务，输入边界清晰、产物可单独验证。

| 工作流 | 覆盖阶段 | 范围 | 阻塞 |
|---|---|---|---|
| **Workflow A** | **P0 + P4a foundation** | 创建 `peri-acp-types` crate；在 ACP 层落地 `peri/unstable-event` 通道、事件路由器、视图映射器（迁自 TUI 的 `ui/message_view/build.rs`）；建立 `session/execute-command` dispatch 与 `session/prompt` 单入口。产出：TUI 与 ACP 之间的纯 DTO 契约就绪。 | 无前置；后续所有 workflow 都隐式依赖 P0 部分 |
| **Workflow B** | **P1 + P1.5 + P2** | TUI 事件循环骨架（双后台 task + 单通道）；异步回路迁移（Cron/Channel → Agent 层）；State/Effect 纯函数状态机重写；输入字段集中；交互弹窗 Handler trait。产出：TUI 内核是纯函数状态机。 | 依赖 A 的 P0（ViewModel 类型）；B 内部 P1→P1.5→P2 串行 |
| **Workflow C** | **P3** | 14 面板按 `PanelState + PanelReadContext + PanelEffect(6 变体)` 重写；4 交互弹窗迁到 Handler trait；删除 PanelManager / PanelContext / `panel_dispatch!` / `with_*_panels!` 宏。 | 依赖 B 完成（PanelReadContext 由状态机注入） |
| **Workflow D** | **P4b 类型隔离** | 删除 TUI 所有 `peri_agent` / `peri_middlewares` import；ServiceRegistry 后端字段改 ACP 查询；启用 pre-commit 钩子；删 AgentEvent 枚举。 | 依赖 A（事件路由器）+ B（状态机）+ C（面板重写） |
| **Workflow E** | **P5 渲染重写** | 删除双线程渲染 / RenderCache / RenderEvent / 渲染通知通道；主线程同步渲染 + 16ms 帧率节流；删除 MessagePipeline 替换为 ViewStore；Block 模式保留为内部细节。 | 依赖 D（ViewModel 已是唯一数据源） |

**关键路径**：A(P0) → B(P1) → B(P1.5) → B(P2) → D(P4b) → E(P5)，约 13–19 周。C(P3) 可在 B 完成后与 D 并行。

---

## Phase 0: peri-acp-types crate + DTO 契约

**Goal**: 建立仅依赖 serde 的纯 DTO crate，作为 TUI 与 ACP 的类型契约基础，后续所有阶段都基于此写代码。
**Blocks**: P1（主循环 Effect::Render 携带 ViewModel）、P4a（事件 data 结构）。
**Blocked by**: 无。

### Files to create
- `peri-acp-types/Cargo.toml` — workspace 成员；依赖仅 `serde`（+ `serde_json` 用于 `Value` 别名）。
- `peri-acp-types/src/lib.rs` — `pub mod view_model; pub mod event_data; pub mod summary;`
- `peri-acp-types/src/view_model.rs` — ViewModel 枚举（7 变体：UserBubble / AssistantBubble / ToolCard / SystemNote / SubAgentGroup / CollapsedGroup / Divider）+ 配套子结构（ToolCardField / DiffBlock / ReasoningBlock 等）。设计文档 §4.1。
- `peri-acp-types/src/event_data.rs` — 协议文档 §4 全部 data 结构：流式（TextChunk / ToolStarted / ToolEnded / ReasoningChunk）、边界（ViewCommit / TurnDone / TurnInterrupted）、状态（TokenUsage / ToolCount / Progress / BudgetWarning / SystemNotification）、输入辅助（Prediction / FileSuggestions）、交互请求（HitlPending + ToolApproval、AskUser + Question、RewindPreview + FileChange + RewindMessage、OauthNeeded）、结构（SubagentStarted / SubagentStopped）。
- `peri-acp-types/src/summary.rs` — 跨会话查询返回的摘要结构：SkillSummary / CronSummary / CronTaskDto / McpServerDto / McpToolDto / PluginDto / HookDto / ModelAliasDto / ProviderSnapshotDto / WorkflowProgressDto（迁自 `peri-acp/src/event/dto.rs::WorkflowProgressDto`）/ CompactFileInfoDto（同上迁入）/ TodoItemDto / TodoStatusDto / TokenUsageDto / StopReasonDto。

### Files to modify
- `Cargo.toml`（workspace 根）— `members` 数组新增 `"peri-acp-types"`。
- `peri-acp/Cargo.toml` — `dependencies` 新增 `peri-acp-types = { path = "../peri-acp-types" }`。
- `peri-tui/Cargo.toml` — `dependencies` 新增 `peri-acp-types = { path = "../peri-acp-types" }`（此阶段不强制切换 import，仅引入路径）。
- `peri-acp/src/event/dto.rs` — 将 6 个 DTO `pub use` 重导出到 `peri-acp-types::summary`，本文件改为 `pub use peri_acp_types::summary::*;`（保持向后兼容直到 P4b 删除）。

### Validation
- `cargo build -p peri-acp-types` 绿。
- `cargo test -p peri-acp-types`（新增 smoke 测试：每个 data 结构可 serde round-trip）。
- `cargo build --workspace` 绿（DTO 重导出未破坏既有引用）。

### Subtask breakdown（可并行）
1. crate 脚手架（Cargo.toml + lib.rs + workspace 注册）。
2. ViewModel 枚举 + 子结构定义。
3. event_data.rs 全部 data 结构（按协议 §4 五类）。
4. summary.rs 摘要结构 + 从 `peri-acp/src/event/dto.rs` 迁移 6 个 DTO。
5. DTO 重导出兼容层 + workspace build 验证。

---

## Phase 1: 事件循环骨架（主循环 + 双后台 task）

**Goal**: 删除 `poll_agent` 9 队列架构的大部分，建立 `recv → handle → apply effects → loop` 主循环。状态机先用瘦壳（包装旧 App 方法），不强行纯函数化。
**Blocks**: P1.5（删 polling 最后三条 drain 前需先有 ACP 通知 task）、P2（状态机纯函数化的中间态）。
**Blocked by**: P0。

### Files to create
- `peri-tui/src/runtime/mod.rs` — 运行时模块入口（与 `app/` 状态层解耦）。
- `peri-tui/src/runtime/event_channel.rs` — 单一 `tokio::mpsc::UnboundedReceiver<TuiEvent>`；TuiEvent 枚举（Key / Mouse / Paste / Resize / Tick / AcpEvent / System）。
- `peri-tui/src/runtime/keyboard_collector.rs` — 后台 task：内部轮询 crossterm + 50ms 定时器推 Tick；按键/鼠标/粘贴/Resize 立即转 TuiEvent 推入通道。
- `peri-tui/src/runtime/acp_notifier.rs` — 后台 task：监听 ACP transport 通知通道，每个 `{event, data}` 包成 `TuiEvent::AcpEvent` 推入同一通道。
- `peri-tui/src/runtime/main_loop.rs` — 主循环：`while let Some(ev) = rx.recv() { let (s, effects) = state_machine::handle(state, ev); state = s; for e in effects { apply_effect(e, &mut ctx).await } }`。
- `peri-tui/src/runtime/effect.rs` — Effect 枚举四变体（Render(snapshot) / SendToAcp(event, data) / CopyToClipboard(text) / Quit）。
- `peri-tui/src/runtime/apply_context.rs` — ApplyContext：持有 `terminal` + `acp_client` + clipboard handle，无状态。

### Files to modify
- `peri-tui/src/main.rs` — 启动序列改为：构造 channel → spawn keyboard_collector + acp_notifier → 进入 main_loop（替代当前 App::run）。
- `peri-tui/src/app/agent_ops/polling.rs` — 删除 9 队列中的 5 条：cancel timeout / throttle / continuation / pending_messages / background events；**保留** v2 queue drain / cron / channel 三条（P1.5 删）。函数顶部加 `// TODO(P1.5): delete remaining 3 drains`。
- `peri-tui/src/app/mod.rs` — App::run 退化为 thin wrapper 调 main_loop；run_inner 内部 I/O 调用暂保留（瘦壳），由 state_machine::handle 内部直接调旧 App 方法。
- `peri-tui/src/app/agent_ops/mod.rs` — poll_agent 改为仅负责剩余 3 条 drain，仍由瘦壳状态机周期性调用。
- `peri-tui/src/event/mod.rs` — 现有事件循环改为只向新 channel 推事件的兼容层（P2 完整替换前并存）。

### Files to delete
- 无（P1.5 才删 polling.rs；P2 才删 App::run 内部逻辑）。

### Validation
- `cargo run -p peri-tui -- -a` 能启动；键盘输入有响应；Agent 仍能完成一次完整 ReAct 循环。
- 手工验证：cron 触发 / channel 消息 / bg_results 三条异步路径仍工作（polling.rs 保留 3 条 drain）。
- `cargo test --workspace` 全过。

### Subtask breakdown
1. event_channel + TuiEvent 枚举 + Effect 枚举骨架。
2. keyboard_collector 后台 task（crossterm poll → TuiEvent）。
3. acp_notifier 后台 task（transport → TuiEvent::AcpEvent）。
4. main_loop + ApplyContext + main.rs 启动序列切换。
5. polling.rs 5 条 drain 删除 + 保留 3 条 drain 标注 TODO。
6. 集成验证（手工跑 TUI + 全测试）。

---

## Phase 1.5: 异步回路迁移（Cron/Channel/bg → Agent 层）

**Goal**: 把 CronScheduler / channel_notification_rx / bg_results 三个触发源从 TUI 侧迁到 Agent/ACP 层；Agent Session 实现空闲 await-wake；删除 polling.rs 最后三条 drain。
**Blocks**: P2（状态机不感知触发源，必须先迁完）。
**Blocked by**: P1。

**[TRAP]** S6 回归：ACP executor 末尾禁止加 `drain_for_end` 循环——destructive，取出后不续跑消息物理丢失。迁移过程必须先建 Agent Session await-wake，再删 TUI drain。

### Files to create
- `peri-agent/src/agent/session/inbox.rs` — Session 收件箱（基于 v2 MessageQueue 扩展）：`async fn await_wake(&self)`，空闲时阻塞直到新 Prompt 入队；新 Prompt 到达自动恢复 run_session_loop。
- `peri-agent/src/agent/session/cron_owner.rs` — CronScheduler 迁入 Agent 层；trigger_tx 直接推 Session 收件箱（`Kind::Prompt` 或 `Defer`）。
- `peri-agent/src/agent/session/channel_owner.rs` — channel_notification_rx 迁入 Agent 层；ChannelState 推 Session 收件箱。
- `peri-acp/src/session/async_router.rs` — bg_results / workflow 推送统一从 ACP 层入 Session 收件箱（替代当前 executor.rs 直接 push v2 queue）。

### Files to modify
- `peri-agent/src/agent/session/mod.rs` — Session 持有 inbox + cron_owner + channel_owner；run_session_loop 在每轮 End 阶段后调 `inbox.await_wake()` 决定是否续跑。
- `peri-agent/src/agent/stages/end.rs` — `should_continue` 改为 `inbox.has_pending()` 检查（替代当前 v2 queue drain）。
- `peri-acp/src/session/executor.rs` — `run_session_loop` 末尾的 ACP 事件循环改为 `inbox.await_wake()`；删除当前对 v2 queue 的直接 push（改走 async_router）。
- `peri-acp/src/agent/builder.rs` — build_agent 时构造 cron_owner / channel_owner 并注入 Session。
- `peri-tui/src/app/cron_state.rs` — 删除 CronScheduler / trigger_rx 字段；CronState 仅保留 UI 展示用的缓存（数据从 ACP `"system-notification"` 或 query 拉取）。
- `peri-tui/src/app/message_state.rs` — 删除 channel_notification_rx 字段。
- `peri-tui/src/app/service_registry.rs`（或对应模块） — CronState / ChannelState 字段降级为只读缓存。
- `peri-tui/src/app/agent_ops/polling.rs` — 删除最后 3 条 drain（v2 queue / cron / channel）。**整个 polling.rs 此时变空**。
- `peri-tui/src/app/agent_ops_bg.rs` — `pending_continuation` 路径删除（Agent 层自动续跑）。
- `peri-middlewares/src/cron/mod.rs`（或 Scheduler 所在文件） — Scheduler 构造改为接受 `SessionInbox` handle 而非返回 trigger_rx。

### Files to delete
- `peri-tui/src/app/agent_ops/polling.rs` — 所有 drain 已迁移，整个文件死代码。
- `peri-tui/src/app/agent_events_bg.rs` 中 `pending_continuation` 相关函数（若无其他用途则整文件删除）。

### Validation
- 手工测试三条异步路径：cron 触发后 Agent 自动开始输出；channel 消息送达后 Agent 续跑；bg SubAgent 完成后主 Agent 自动续跑。
- `grep -r 'drain_for_end' peri-tui/` 零结果。
- `cargo test --workspace` 全过（重点：peri-acp 的 async/event 相关测试）。
- 长时运行测试：TUI idle 60s 后 cron 触发能正常续跑（验证 await-wake）。

### Subtask breakdown
1. Session inbox + await_wake 实现（peri-agent）。
2. CronScheduler 迁移到 Agent 层（trigger_tx → inbox）。
3. channel_notification_rx 迁移到 ACP 层。
4. bg_results / workflow 推送走 async_router。
5. end.rs `should_continue` 切换到 inbox API。
6. TUI 侧删 polling.rs 最后 3 条 drain + CronState/MessageState 字段降级。
7. 集成测试（三条路径手工验证 + 全测试通过）。

---

## Phase 2: 状态机纯函数化（State/Effect + 输入集中 + Handler trait）

**Goal**: 引入 State / Effect 枚举；handle 函数化为 `(State, Event) → (State, Vec<Effect>)`；输入字段集中到 State::Idle/Streaming；4 交互弹窗迁到 Handler trait；删除 App 50+ 方法中的 I/O。
**Blocks**: P3（PanelReadContext 由状态机注入）、P4a（view-commit 全量替换语义在状态机内实现）、P5（ViewStore 安放状态机内）。
**Blocked by**: P1、P1.5。

### Files to create
- `peri-tui/src/state_machine/mod.rs` — `pub fn handle(state: State, event: Event) -> (State, Vec<Effect>)` 纯函数入口；零 I/O。
- `peri-tui/src/state_machine/state.rs` — State 枚举四变体：Idle / Streaming / Modal / Switching。Idle 持有输入框 + @ 补全 + / 补全 + 附件 + 滚动 + 双击 Esc 计时器；Streaming 持有 CurrentTurn（文本片段 + 工具卡片列表）+ 输入框（用户可继续输入）；Modal 持有 Box<dyn PanelState> 或 Box<dyn Handler> + 保存的 Streaming 增量；Switching 持有加载指示状态。
- `peri-tui/src/state_machine/event.rs` — Event 枚举：Key / Mouse / Paste / Resize / Tick / AcpEvent(AcpEventData) / AcpDisconnected / SessionLoaded / Shutdown。AcpEventData 是 `{event: String, data: Value}` 解析后的枚举（text-chunk / view-commit / turn-done / hitl-pending / ask-user / 等，按协议 §4 全量）。
- `peri-tui/src/state_machine/current_turn.rs` — Streaming 状态的 CurrentTurn 结构（累积文本、工具卡片列表、spinner 帧）。
- `peri-tui/src/state_machine/transitions/idle.rs` — Idle 状态的 handle 实现（输入框编辑、@ 补全触发/导航、/ 补全、Enter 提交产出 SendToAcp、双击 Esc 产出 Quit）。
- `peri-tui/src/state_machine/transitions/streaming.rs` — Streaming 状态的 handle（text-chunk 追加 CurrentTurn；view-commit 替换 ViewModel 列表 + 清空 CurrentTurn；turn-done 切 Idle；用户输入仍可累积）。
- `peri-tui/src/state_machine/transitions/modal.rs` — Modal 状态的 handle（按键委托 PanelState 或 Handler；Esc 产出 Close；Streaming→Modal 保存增量）。
- `peri-tui/src/state_machine/transitions/switching.rs` — Switching 状态的 handle（首批 ViewModel 到达切 Idle）。
- `peri-tui/src/state_machine/handler.rs` — Handler trait（交互弹窗）：`render(...) -> ...` / `handle_key(...) -> HandlerOutput` / `produce() -> Vec<Effect>`。
- `peri-tui/src/state_machine/handlers/hitl.rs` — HITL 审批 Handler（替代 `hitl_prompt.rs`）。
- `peri-tui/src/state_machine/handlers/ask_user.rs` — AskUser Handler（替代 `ask_user_prompt.rs`）。
- `peri-tui/src/state_machine/handlers/rewind.rs` — Rewind 预览 Handler（替代 `rewind_prompt.rs`）。
- `peri-tui/src/state_machine/handlers/oauth.rs` — OAuth 授权 Handler（替代 `oauth_prompt.rs`）。
- `peri-tui/src/state_machine/input/textarea_state.rs` — 输入框状态（buffer + cursor + history index + prediction），从 `field_textarea.rs` 迁移状态所有权（FieldTextarea 保留为实现细节）。
- `peri-tui/src/state_machine/input/at_mention.rs` — @ 补全子状态（active / candidates / selected_index / cache），从 `at_mention/` 迁移所有权。
- `peri-tui/src/state_machine/input/slash_completion.rs` — / 补全子状态。
- `peri-tui/src/state_machine/input/attachments.rs` — 附件列表状态。
- `peri-tui/src/state_machine/view_store.rs` — ViewModel 列表缓存（最后一次 view-commit 吸收的全量列表）+ 派生函数 `view_models_for_render(&self, current_turn: Option<&CurrentTurn>) -> &[ViewModel]`。

### Files to modify
- `peri-tui/src/app/mod.rs` — App struct 大幅瘦身：仅保留 ApplyContext 需要的句柄（terminal / acp_client / clipboard）；删除 50+ 业务方法；状态字段全部迁移到 State 枚举。
- `peri-tui/src/app/chat_session.rs` — ChatSession 六组件（UiState / MessageState / AgentComm / CommandSystem / SessionMetadata / PanelManager）拆解；可变状态进 State 枚举；不可变配置进 frozen。
- `peri-tui/src/app/global_ui_state.rs` — quit_pending / rewind_pending 计时器迁入 State::Idle 的双击 Esc 逻辑；oauth_prompt / setup_wizard 迁入 Modal。
- `peri-tui/src/app/message_state.rs` — 消息状态字段迁入 ViewStore；本文件可能整体删除（视剩余内容）。
- `peri-tui/src/app/message_pipeline/mod.rs` — **暂时保留**（P5 删除），但内部 commit_iteration / restore_completed 调用改为通过 State::Streaming::on_view_commit 转发；build_tail_vms 纯函数逻辑迁入 ViewStore 作为参考实现。
- `peri-tui/src/app/agent_render.rs::apply_pipeline_action` — ephemeral_notes anchor 计算保留（P5 删）。
- `peri-tui/src/event/keyboard/normal_keys.rs` + `shortcuts.rs` — 12 级优先级管线降级为单一委托：所有按键包成 `Event::Key` 推入 channel，由 state_machine::handle 决定路由（panel > popup > input）；硬编码 OAuth>AskUser>HITL 优先级删除。
- `peri-tui/src/app/events.rs` — AgentEvent 枚举暂保留（P4a 删除），但 `handle_agent_event` 改为把每个 AgentEvent 翻译成 `Event::AcpEvent(...)` 推入新 channel（兼容层）。
- `peri-tui/src/app/agent_ops_interaction.rs` — 改为状态机内 Modal 进入逻辑的兼容层。
- `peri-tui/src/app/hitl_prompt.rs` / `ask_user_prompt.rs` / `rewind_prompt.rs` / `oauth_prompt.rs` — 内部逻辑迁入对应 Handler 后**整体删除**。

### Files to delete
- `peri-tui/src/app/hitl_prompt.rs`
- `peri-tui/src/app/ask_user_prompt.rs`
- `peri-tui/src/app/rewind_prompt.rs`
- `peri-tui/src/app/oauth_prompt.rs`（含 `oauth_prompt_test.rs`）
- `peri-tui/src/app/hitl_prompt.rs` 对应测试文件（若有）
- `peri-tui/src/event/macros.rs` 中 `with_global_panels!` / `with_session_panels!`（P3 才删 PanelContext，但宏若已无引用可提前删）

### Validation
- 状态机纯函数测试：`#[test] fn test_idle_typing_appends_to_buffer()` 等数十个不依赖终端的测试。
- `cargo test -p peri-tui` 全过（含改写后的 message_pipeline_test.rs 81 测试——验证 view-commit 替换语义）。
- 手工验证：4 个交互弹窗（HITL / AskUser / Rewind / OAuth）各自独立触发，不再硬编码优先级。
- 手工验证：Streaming 期间可输入；Streaming→Modal 保存增量，关闭后恢复。

### Subtask breakdown（可部分并行）
1. State / Event / Effect 枚举骨架 + state_machine::handle 主入口。
2. ViewStore + view_models_for_render 派生函数（迁 build_tail_vms 逻辑）。
3. Idle transition（输入框 + @ 补全 + / 补全 + Enter 提交 + 双击 Esc）。
4. Streaming transition（text-chunk / view-commit / turn-done）。
5. Modal + Switching transition。
6. Handler trait + 4 个具体 Handler（hitl / ask_user / rewind / oauth）。
7. App struct 瘦身 + ChatSession 六组件拆解 + 业务方法删除。
8. keyboard 管线降级为单一委托 + handle_agent_event 改兼容层。
9. 测试改写（message_pipeline_test 81 个 + headless 相关）。

---

## Phase 3: 面板重写（PanelState + 双向通道）

**Goal**: 14 面板按 `PanelState + PanelReadContext + PanelEffect(6 变体)` 重写；删除 PanelManager / PanelContext / panel_dispatch! 宏。
**Blocks**: P4b（pre-commit 必须面板重写完才能启用）。
**Blocked by**: P2。

### Files to create
- `peri-tui/src/panel/mod.rs` — PanelState trait：`fn render(&self, ctx: &PanelReadContext, area: Rect, frame: &mut Frame)` / `fn handle_key(&mut self, key: KeyEvent, ctx: &PanelReadContext) -> Vec<PanelEffect>`。
- `peri-tui/src/panel/read_context.rs` — PanelReadContext（只读快照）：&[ViewModel] / scroll_offset / panel_area / selected_index / config_snapshot / acp_query_cache。
- `peri-tui/src/panel/effect.rs` — PanelEffect 枚举六变体：ShowNotification(text) / SendToAcp(event, data) / Close / SwitchSession(session_id) / Copy(text) / UpdateConfig(key, value)。
- `peri-tui/src/panel/registry.rs` — PanelKind 枚举（14 + 2 会话级）+ `fn open(kind) -> Box<dyn PanelState>` 工厂。
- `peri-tui/src/panel/panels/config.rs` / `model.rs` / `login.rs` / `mcp.rs` / `plugin.rs` / `hooks.rs` / `cron.rs` / `status.rs` / `memory.rs` / `betas.rs` / `workflow.rs` / `thread_browser.rs` / `setup_wizard.rs` / `tasks.rs` / `agent.rs` — 16 个面板各自实现 PanelState。每个面板：
  - 持有 UI 局部状态（选中索引、滚动、表单输入）。
  - 通过 PanelReadContext 读取数据（而非 `&mut ServiceRegistry`）。
  - 通过 PanelEffect 产出指令（而非直接 `ctx.session_mut.xxx()` 或 spawn tokio）。
  - 数据获取流程：打开时产 `SendToAcp("query", {resource, params})` → ACP 返回 `{event, data}` → 状态机更新 acp_query_cache → 下次 handle_key 时 PanelReadContext 可见。

### Files to modify
- `peri-tui/src/state_machine/transitions/modal.rs` — Modal::Panel 持有 `Box<dyn PanelState>`；handle_key 委托 PanelState::handle_key；将产出的 `Vec<PanelEffect>` 映射为 `Vec<Effect>`（ShowNotification → Render + 注入 SystemNote / SendToAcp → Effect::SendToAcp / Close → 切回 Idle / SwitchSession → Effect + 切 Switching / Copy → Effect::CopyToClipboard / UpdateConfig → Effect::SendToAcp("config/update")）。
- `peri-tui/src/state_machine/view_store.rs` — 新增 `acp_query_cache: HashMap<String, Value>`（面板查询结果缓存）。
- `peri-tui/src/app/mcp_panel/{component,ops}.rs` + `ui/main_ui/panels/mcp.rs` — 删除 `peri_middlewares::mcp::*` import，改读 PanelReadContext 中的 McpServerDto / McpToolDto。
- `peri-tui/src/app/hooks_panel.rs` — 删除 hooks types import，改读 HookDto。
- `peri-tui/src/app/cron_state.rs` + `tasks_panel.rs` — 删除 `peri_middlewares::cron::*` import，改读 CronTaskDto。
- `peri-tui/src/app/memory_panel.rs` — 外部编辑器 spawn 改为 PanelEffect（需新增效果——suspend TUI / exec $EDITOR / restore——可作为 ShowNotification 的特殊变体或新增 PanelEffect 变体；建议用 SendToAcp 委托给 ACP 执行）。
- `peri-tui/src/app/thread_browser.rs`（或对应文件） — 会话切换改产 `PanelEffect::SwitchSession`，终端 raw mode 切换由 main_loop 在 apply Effect 时处理。

### Files to delete
- `peri-tui/src/app/panel_component.rs` — PanelComponent trait 死。
- `peri-tui/src/app/panel_manager.rs` — PanelManager + PanelState enum + panel_dispatch! 死。
- `peri-tui/src/app/panel_context.rs`（若存在；否则在 panel_manager.rs 内） — PanelContext 死。
- `peri-tui/src/app/panel_list.rs` + `panel_list_test.rs` — 改为新 registry 后旧 list 死。
- `peri-tui/src/app/panel_manager_test.rs` — 测试迁到新 registry。
- `peri-tui/src/app/panel_config.rs` / `panel_hooks.rs` / `panel_login.rs` / `panel_betas.rs` / `panel_agent.rs` — 旧面板实现死（逻辑迁入 `panel/panels/*.rs`）。
- `peri-tui/src/event/macros.rs` — `with_global_panels!` / `with_session_panels!` 宏死。
- `peri-tui/src/app/agent_events_oauth.rs` / `agent_events_plugin.rs` — Handler trait 上线后失去存在理由。

### Validation
- 16 面板逐一手工验证：打开 → 数据加载（query）→ 按键交互 → 产出 Effect → Esc 关闭。
- 重点验证 MemoryPanel（外部编辑器）+ ThreadBrowser（会话切换 + raw mode）。
- `cargo test -p peri-tui` 全过；panel 相关测试改写后通过。
- `grep -r 'PanelContext' peri-tui/` 零结果。
- `grep -r 'panel_dispatch' peri-tui/` 零结果。

### Subtask breakdown（高并行）
1. PanelState trait + PanelReadContext + PanelEffect + registry 基础设施。
2. modal.rs 接入 Box<dyn PanelState> + PanelEffect → Effect 映射。
3. 16 面板逐一重写（每个独立，可分 16 个并行子任务）：
   - 简单面板（StatusPanel / BetasPanel / AgentPanel / TasksPanel）。
   - 中等面板（ConfigPanel / ModelPanel / LoginPanel / HooksPanel / CronPanel / WorkflowPanel）。
   - 复杂面板（McpPanel / PluginPanel / MemoryPanel / ThreadBrowser / SetupWizard）。
4. 删除 PanelManager / PanelContext / panel_dispatch! / 宏 / 旧 panel_*.rs。
5. 测试改写。

---

## Phase 4a: ACP 事件路由器 + 视图映射器 + slash 命令 dispatch

**Goal**: ACP 层落地事件路由器（AgentEvent → `{event, data}`）、视图映射器（BaseMessage → ViewModel 增量缓存）、`peri/unstable-event` 通道；4 slash 命令路由通过 `session/execute-command` dispatch；`execute_prompt` 统一为 `session/prompt` 入口。
**Blocks**: P4b（删 TUI AgentEvent 枚举与 BaseMessage import 前需先有新通道）。
**Blocked by**: P0、P2。

### Files to create
- `peri-acp/src/event/router.rs` — 事件路由器：`fn route(agent_event: AgentEvent, view_mapper: &mut ViewMapper) -> Option<(String, Value)>`；按协议 §5 映射 16 种 AgentEvent 到自定义事件名；3 种丢弃（LlmRetrying / LspDiagnostics / CompactStarted/Completed）。
- `peri-acp/src/event/view_mapper.rs` — 视图映射器：增量缓存已转换 ViewModel 数量；TurnCompleted 时调 `convert_range(messages, start_idx)` 转换新增 BaseMessage；产出完整 `Vec<ViewModel>` 作为 `"view-commit"` data。逻辑迁自 `peri-tui/src/ui/message_view/build.rs`。
- `peri-acp/src/event/view_mapper_test.rs` — 测试迁自 `peri-tui/src/ui/message_view/message_view_test.rs`（27 测试）。
- `peri-acp/src/dispatch/execute_command.rs` — `session/execute-command` 方法处理器：`{session_id, command, args}` → 路由到 bg / compact / clear / rewind 四个现有 command 模块。
- `peri-acp/src/dispatch/prompt.rs` — `session/prompt` 方法处理器：统一入口调 `execute_prompt()`。

### Files to modify
- `peri-acp/src/event/mapper.rs` — 现有 ExecutorEvent → AcpNotification 映射保留兼容（P4b 删）；新增 `forward_to_unstable_event` 函数将路由器产出的 `{event, data}` 推入 `peri/unstable-event`。
- `peri-acp/src/event/mapper_v2.rs` — 整合到 router.rs，或保留为 router 的内部实现。
- `peri-acp/src/session/event_sink.rs` — TransportEventSink：双通知（`session/update` + `peri/agent_event`）改为只发 `peri/unstable-event`（自定义事件格式）；标准方法仍走 JSON-RPC。**双通知兼容层保留到 P4b 切换验证完毕**。
- `peri-acp/src/session/executor.rs` — 删除 `intercept_immediate_command`（slash 命令改走 dispatch 层）；`run_session_loop` 末尾改为通过 event_router 推送事件而非直接调 EventSink 双通知。
- `peri-acp/src/dispatch/mod.rs` — 注册 `session/execute-command` 与 `session/prompt` 方法路由。
- `peri-acp/src/dispatch/commands.rs` — 现有命令分发逻辑迁入 execute_command.rs。
- `peri-tui/src/acp_client/client.rs` — AcpTuiClient 新增 `send_event_handler`：监听 `peri/unstable-event` 推送，转 `TuiEvent::AcpEvent` 推入 channel（替代当前从 rx 手动 poll）。
- `peri-tui/src/state_machine/event.rs` — AcpEventData 枚举对齐协议 §4 全部事件名。
- `peri-tui/src/state_machine/transitions/streaming.rs` — `"view-commit"` 处理：`view_store.commit(data.view_models)` 全量替换；`"text-chunk"` 处理：`current_turn.append_text(data.text, data.agent_id)`。

### Files to delete
- `peri-acp/src/session/executor.rs::intercept_immediate_command`（函数级删除）。
- `peri-tui/src/ui/message_view/build.rs` — 转换层整体迁到 ACP 后死。
- `peri-tui/src/ui/message_view/message_view_test.rs` — 测试已迁到 peri-acp。

### Validation
- `cargo test -p peri-acp` 全过；view_mapper_test 27 测试通过；mapper_test 27 测试改写后通过。
- 手工验证：4 slash 命令（/bg /compact /clear /rewind）通过 `session/execute-command` dispatch 正常执行。
- 手工验证：流式输出、view-commit、turn-done、交互请求全部经 `peri/unstable-event` 到达 TUI。
- 抓包验证：TUI 收到的所有 Agent→TUI 消息都是 `{event, data}` 格式，无 `session/update` 残留。

### Subtask breakdown
1. event/router.rs + AgentEvent → `{event, data}` 映射（16 种 + 3 丢弃）。
2. view_mapper.rs（迁 build.rs 324 LOC）+ 增量缓存。
3. dispatch/execute_command.rs + 4 命令路由。
4. dispatch/prompt.rs + execute_prompt 单入口对齐。
5. event_sink.rs 双通知改单通道（保留兼容层）。
6. AcpTuiClient 监听 peri/unstable-event 推 channel。
7. 测试迁移（view_mapper_test 27 + mapper_test 27 改写）。

---

## Phase 4b: TUI 类型隔离 + pre-commit 钩子

**Goal**: 删除 TUI 所有 `peri_agent` / `peri_middlewares` import；ServiceRegistry 后端字段改 ACP 查询；启用 pre-commit 钩子；删除 AgentEvent 枚举（被事件名 + data 结构替代）。
**Blocks**: P5（删 MessagePipeline 前必须切到 ViewModel）。
**Blocked by**: P3、P4a。

### Files to create
- `scripts/check-tui-imports.sh` — pre-commit 钩子脚本：grep `use peri_agent::` / `use peri_middlewares::` in `peri-tui/src/`（排除合法桥接：`acp_server/` / `acp_stdio/context.rs` / `main.rs` / `cli_print.rs`），命中则 exit 1。
- `.lefthook.yml` 或对应 pre-commit 配置 — 注册 check-tui-imports.sh。

### Files to modify
- `peri-tui/src/app/events.rs` — **删除 AgentEvent 枚举**（25 变体）；所有事件流改用 `Event::AcpEvent(AcpEventData)`；`handle_agent_event` 200+ 行 match 删除。
- `peri-tui/src/app/agent_ops/*.rs`（除 polling 已在 P1.5 删） — 删除对 AgentEvent 变体的 match；改为对 `Event::AcpEvent` 的处理。
- `peri-tui/src/app/message_pipeline/{mod,lifecycle,transform,state,tools}.rs` — 删除 BaseMessage 持有；改用 `Vec<ViewModel>`（从 ViewStore 派生）。**P5 整体删除前的中间状态**。
- `peri-tui/src/app/service_registry.rs` — MCP 连接池 / Cron 字段 / 插件数据 / 资源监控字段全部删除；改为通过 `Effect::SendToAcp("query", ...)` 异步获取。
- `peri-tui/src/app/command/core/gc.rs` 及其他直接 import BaseMessage 的命令文件 — 改用 ViewModel 或 DTO。
- `peri-tui/src/app/mcp_panel/*` / `hooks_panel.rs` / `cron_state.rs` / `tasks_panel.rs` — P3 已迁大部分，本阶段清剩余 import。
- 14 测试文件中 import BaseMessage / ContentBlock / SkillMetadata 等 — 改为构造 ViewModel 或 DTO（headless_test.rs 72 测试重点改写）。
- `peri-tui/Cargo.toml` — 删除 `peri-agent` / `peri-middlewares` 依赖（保留 `peri-acp-types` + `peri-widgets`）。

### Files to delete
- `peri-tui/src/app/events.rs` 中 AgentEvent 枚举（保留 Event::AcpEvent 路径）。
- `peri-tui/src/app/agent_ops/lifecycle.rs` / `acp_bridge.rs` / `subagent.rs` 中针对 AgentEvent 的处理函数（若无新用途）。
- `peri-acp/src/event/dto.rs` 中 6 个 DTO 的旧定义（已在 P0 迁到 peri-acp-types，重导出兼容层此时删除）。

### Validation
- `scripts/check-tui-imports.sh` 主动运行：零违规。
- `lefthook run pre-commit` 通过（含 check-tui-imports）。
- `cargo build -p peri-tui` 不依赖 `peri-agent` / `peri-middlewares`（检查 Cargo.lock）。
- `grep -r 'use peri_agent::' peri-tui/src/` 仅剩合法桥接文件。
- `grep -r 'use peri_middlewares::' peri-tui/src/` 仅剩合法桥接文件。
- `grep -r 'AgentEvent' peri-tui/src/` 零结果。
- `cargo test --workspace` 全过。

### Subtask breakdown
1. pre-commit 脚本 + 注册（先 dry-run，列出违规清单）。
2. ServiceRegistry 后端字段删除 + query 替换。
3. message_pipeline 5 文件切到 Vec<ViewModel>。
4. AgentEvent 枚举删除 + agent_ops 事件流切换。
5. 14 测试文件改写（headless_test 72 + 其他）。
6. Cargo.toml 依赖裁剪 + 合法桥接白名单验证。
7. pre-commit 启用 + 全测试。

---

## Phase 5: 渲染重写（删双线程 + ViewStore 替换 Pipeline）

**Goal**: 删除双线程渲染 / RenderCache / RenderEvent / 渲染通知通道；主线程同步渲染 + 16ms 帧率节流；删除 MessagePipeline 替换为 ViewStore；Block 模式保留为渲染层内部细节。
**Blocks**: 无（最终阶段）。
**Blocked by**: P2、P4b。

### Files to create
- `peri-tui/src/render/mod.rs` — 渲染入口：`fn render(state: &State, terminal: &mut Terminal) -> Result<()>`；从 state 读 ViewStore + CurrentTurn 派生最终视图；同步调 `terminal.draw()`。
- `peri-tui/src/render/throttle.rs` — 16ms 帧率节流：main_loop 在 `Effect::Render` 执行时检查距上次渲染时间；view-commit / turn-done / turn-interrupted 跳过节流立即渲染；Tick 在 Idle 下不渲染、Streaming 下推进 spinner 帧并渲染。
- `peri-tui/src/render/block_mode.rs` — 代码围栏块模式：流式输出进入 Markdown 围栏时缓冲，闭合标记到达后一次性渲染（迁自现有实现，作为渲染层内部细节）。

### Files to modify
- `peri-tui/src/runtime/main_loop.rs` — Effect::Render 执行改为调 `render::render(&state, terminal)` + throttle 检查；删除当前的双线程渲染触发。
- `peri-tui/src/runtime/effect.rs` — Effect::Render 不再携带完整 snapshot（改为引用 state）；或保留 snapshot 但由 ViewStore 派生。
- `peri-tui/src/state_machine/view_store.rs` — 正式作为状态机内部唯一视图数据源（P2 已建，本阶段强化）。
- `peri-tui/src/state_machine/transitions/streaming.rs` — text-chunk 不再触发独立 Render（合并到下一帧）；view-commit / turn-done 立即 Render。
- `peri-tui/src/state_machine/transitions/idle.rs` — Tick 在 Idle 下不产出 Render（省电）。

### Files to delete
- `peri-tui/src/app/message_pipeline/{mod,lifecycle,transform,state,tools}.rs` — 整个 message_pipeline 目录死（ViewStore 已替代）。
- `peri-tui/src/app/message_pipeline/message_pipeline_test.rs` — 81 测试中大部分迁移到 view_store 测试（验证 commit/restore 替换语义）；与渲染线程相关的测试删除。
- `peri-tui/src/app/agent_render.rs` — 渲染逻辑迁入 render/ 模块；apply_pipeline_action 中 ephemeral_notes anchor 计算删除（全量替换语义消除）。
- 双线程渲染相关文件（grep `RenderCache` / `RenderEvent` / 渲染通知通道定位后删除）。
- `AdaptiveChunkingPolicy` 三种流式模式切换逻辑（主循环帧率节流替代）。

### Validation
- `cargo run -p peri-tui -- -a` 启动；流式输出无闪烁；代码块完整渲染。
- CPU 占用验证：Idle 状态下 Tick 不触发渲染，CPU 接近 0。
- 60fps 验证：高频流式输出（每秒数十次 text-chunk）下渲染稳定在 ~60fps。
- `grep -r 'RenderCache\|RenderEvent\|AdaptiveChunkingPolicy' peri-tui/src/` 零结果。
- `grep -r 'message_pipeline' peri-tui/src/` 零结果。
- `cargo test --workspace` 全过。

### Subtask breakdown
1. render/ 模块骨架 + render::render 同步绘制。
2. throttle 16ms 节流 + view-commit/turn-done 跳过节流。
3. block_mode 代码围栏缓冲逻辑迁移。
4. main_loop 切换到新 render 调用。
5. message_pipeline 目录 + agent_render 删除。
6. 双线程渲染基础设施删除（RenderCache / RenderEvent / 通知通道 / AdaptiveChunkingPolicy）。
7. 测试迁移与改写。

---

## 附录 A：测试改写风险矩阵

| 测试文件 | 测试数 | 阶段 | 改写策略 |
|---|---|---|---|
| `peri-tui/src/app/message_pipeline/message_pipeline_test.rs` | 81 | P2（部分）+ P5（整体删） | P2：验证 view-commit 替换语义；P5：迁移到 view_store 测试 |
| `peri-acp/src/event/mapper_test.rs` | 27 | P4a | 改为测 router.rs 的 AgentEvent → `{event, data}` 映射 |
| `peri-tui/src/ui/headless_test.rs` | 72 | P4b | 改为构造 ViewModel / DTO 而非 BaseMessage |
| `peri-tui/src/ui/message_view/message_view_test.rs` | 27 | P4a | 整体迁到 `peri-acp/src/event/view_mapper_test.rs` |
| `peri-acp/src/session/command/{rewind,compact}_test.rs` | — | P4a | 入口改为 session/execute-command dispatch；核心契约断言不变 |
| `peri-tui/src/app/panel_manager_test.rs` 等 | — | P3 | 迁到新 registry 测试 |

## 附录 B：关键 [TRAP] 检查清单

每个阶段提交前对照检查：

- [ ] **P1.5**：ACP executor 末尾无 `drain_for_end` 循环（S6 回归）。
- [ ] **P2**：`commit_iteration` / `restore_completed` 用替换语义（非 extend）。
- [ ] **P2**：`TurnCommitted` / `StateSnapshot` 在 `in_subagent() == true` 时返回 None。
- [ ] **P2**：Pipeline `handle_event` 永不返回 `RebuildAll`。
- [ ] **P3**：PanelEffect 仅 6 变体，面板无 `&mut ChatSession` 路径。
- [ ] **P4b**：`scripts/check-tui-imports.sh` 零违规。
- [ ] **P5**：主循环无决策逻辑（只 recv → handle → apply）。
- [ ] **全阶段**：状态机零 I/O（可脱离终端做纯函数测试）。
