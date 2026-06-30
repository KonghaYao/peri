# peri-tui

TUI 应用，纯 ACP client 前端。运行时仅通过 `peri-acp` 的 `MpscTransport`（in-memory channel pair）与 ACP Server 通信，不直接依赖 `peri-agent`/`peri-middlewares` 的运行时路径（仅作为类型依赖）。

## 当前架构（v2 + B3 Cutover）

**双路径事件分发**（详见根 CLAUDE.md 「TUI 双路径事件分发」）：
- **1a. 状态机路径**（`state_machine::handle`）：纯函数 `(State, Event) → (State, Vec<Effect>)`
- **1b. Legacy 兜底**：`keyboard::handle_key_event`（6 文件 ~1200 行）+ `handle_acp_event`（ACP notification 桥接）
- **1c. 合并去重**：Render 唯一化后执行

**v2 状态机骨架**（`state_machine/`）：
- `state.rs`：State 4 变体（Idle/Streaming/Modal/Switching）+ ModalState（struct，持 saved_view/saved_input/saved_*/kind）+ ModalKind（Panel|Interaction）
- `event.rs`：Event 9 变体 + AcpEventData decode（22 子变体 + Unknown 兜底）
- `view_store.rs`：替换语义 commit（非 extend）
- `current_turn.rs`：流式累积 text/reasoning/tool_cards/spinner
- `input/`：InputState（buffer+cursor+history+at_mention+slash+attachments）
- `transitions/`：idle/streaming/modal/switching 4 个转换函数 + enter_modal_from_idle/streaming helper
- `handlers/`：Handler trait + 4 个 Interaction Handlers（hitl/ask_user/rewind/oauth）+ NoopHandler

**Handler trait**（v2，Phase 1.4 扩展）：
```rust
pub trait Handler: Send + std::fmt::Debug {
    fn render(&self, frame: &mut ratatui::Frame, area: Rect);
    fn handle_key(&mut self, key: KeyEvent) -> HandlerOutput;
    fn desired_height(&self, _screen_height: u16, _screen_width: u16) -> u16 { 12 }
}
```

**[已知缺陷 #15]** v2 Interaction Handler 路径在生产仍是死代码（ACP 层无 AcpEvent::HitlPending 等变体，HITL 走 v1 RequestPermission JSON-RPC 路径）。Phase 1.3/1.4 代码作为前端预留接口，等 ACP 层扩展后启用。详见 `docs/refactor/tui-v3-plan.md` 2026-07-01 cron #7 调研结论。

## 核心文件

| 文件 | 职责 |
|------|------|
| `src/render/vm_convert.rs` | v1 MessageViewModel → v2 ViewModel 转换器（Phase 2.6 桥接） |
| `src/app/subagent_status.rs` | SubAgentStatusMap + SessionSubAgentProbe（运行时状态 + 子内容注入） |
| `src/runtime/main_loop.rs` | 主事件循环（双路径分发 + Effect 执行） |
| `src/runtime/event_channel.rs` | 统一 unbounded channel（5 类输入源合并） |
| `src/runtime/acp_notifier.rs` | ACP notification → TuiEvent 桥接（后台 task） |
| `src/runtime/keyboard_collector.rs` | crossterm 事件轮询（持久化 spawn_blocking → mpsc） |
| `src/runtime/apply_context.rs` | Effect 执行器（terminal + acp_client + clipboard） |
| `src/runtime/effect.rs` | Effect 枚举（26 变体） |
| `src/state_machine/` | v2 纯函数状态机 |
| `src/acp_client/client.rs` | ACP client 封装，`AcpNotification` 变体定义 |
| `src/app/agent.rs` | `ExecutorEvent → AgentEvent` 映射（`map_executor_event`） |
| `src/app/agent_ops/acp_bridge.rs` | `AcpNotification → AgentEvent` 桥接 |
| `src/app/agent_ops_interaction.rs` | HITL/AskUser v1 路径（`InteractionPrompt` 设置） |
| `src/app/agent_submit.rs` | 用户输入提交入口 |
| `src/app/agent_compact.rs` | Compact 事件处理：pipeline 清理 + UI 通知 |
| `src/app/message_state.rs` | MessageState（view_messages + round_start_vm_idx） |
| `src/app/panel_types.rs` | PanelKind（14 变体） + MutexGroup |
| `src/ui/main_ui/mod.rs` | 主布局（含 v1 popup 渲染 + v2 panel_area 预留） |
| `src/i18n/` | 国际化模块（`LcRegistry` + Fluent） |

## ACP 数据流

```
TUI 输入 → AcpTuiClient.new_session() / .prompt()
         → MpscClientTransport.send_request/notification()
         → MpscServerTransport.recv() (ACP Server, tokio::spawn)
         → ExecutorEvent → TransportEventSink.push_event()
         → AcpTuiClient.pump_notifications() → AcpNotification
         → acp_notifier 后台 task → TuiEvent::AcpEvent
         → main_loop 1a (state_machine) + 1b (handle_acp_event 桥接)
         → handle_acp_notification → map_executor_event → AgentEvent
         → UI 更新
```

**[TRAP]** TUI 层数据必须通过 ACP 协议到达 ACP 层，禁止直连。所有 TUI → ACP Server 的状态变更必须通过 `acp_client` 的协议方法。TUI 本地清空状态（如 `new_thread()`）不等于 ACP Server 端状态同步——必须同时通过 ACP 协议通知 Server 侧。

**[TRAP]** `keyboard_collector.rs` 禁止用 `tokio::select!` 同时竞态 `spawn_blocking`（crossterm poll）和 tick interval——会导致事件丢失。必须用持久化 spawn_blocking → mpsc channel → select 从 recv() 读取（见根 CLAUDE.md `keyboard_collector.rs` 重写）。

## 消息渲染（v2 已是生产路径 — Phase 2.6 桥接完成）

**生产渲染路径**：v2 单源
- `runtime::apply_context::draw_now` 从 `state.view_models()` + 流式 `current_turn` 派生 `v2_vms`
- `v2_vms` 通过 `ui::main_ui::render(f, app, panel_height, Some(&v2_vms))` 传入
- `message_area::render_messages` 走 v2 分支（`build_sync_render_cache_v2`）

**SubAgent 子内容桥接**（Phase 2.6 关键）：
- ACP 层 `view_mapper` 生成 `SubAgentGroupData` 时 `view_models` 永久为空 placeholder
- `app/subagent_status.rs::SessionSubAgentProbe` 复合 probe 在 `draw_now` 时构造：
  1. 包装 `SubAgentStatusMap`（运行时状态：is_running / total_steps / final_result / ...）
  2. 从 `view_messages` 通过 `render/vm_convert::message_view_models_to_v2` 解析所有 `SubAgentGroup.recent_messages`（按 agent_id 缓存）
- `render/view_render.rs::render_subagent_group` 子内容优先级：`DTO.view_models > probe.recent_messages > 空`

**v1 view_messages 的当前作用**：
- SessionSubAgentProbe 提取 SubAgentGroup 头部 + recent_messages（生产中 recent_messages 永远为空 Vec，因 v1 不累积子 Agent 内部消息 — 设计如此，"子 agent 内部消息不持久化"）
- 兼容性测试路径（`main_ui::render(f, app, None, None)`，~36 个测试，主要集中在 `headless_test.rs`）

**vm_convert 模块**（`render/vm_convert.rs`）：
- 纯函数 `message_view_model_to_v2(vm) -> Option<ViewModel>` + 批量 `message_view_models_to_v2(vms) -> Vec<ViewModel>`
- 6 变体映射：UserBubble / AssistantBubble（Text + Reasoning 提取）/ ToolBlock / SystemNote / CacheWarning / ToolCallGroup 扁平化 / SubAgentGroup 不嵌套（返回 None）
- 用于：① SessionSubAgentProbe 子内容注入 ② HeadlessHandle::render 测试路径统一走 v2

**[INFO]** `MessageState.view_messages` 仍由 `handle_agent_event` 维护（`apply_add_message` / `apply_rebuild_all`），但生产渲染不再读它。**Phase 2.5 已退役 `ephemeral_notes` 锚点管理**（SystemNote 通过 `pending_v2_notes → Event::PushSystemNote` 独立路由）。**Phase 2.6 完整退役 view_messages 剩余工作**：① 让 SessionSubAgentProbe 完全脱离 view_messages（删除 `legacy_children` 兼容源） ② 删除 `handle_subagent_start` 中 `apply_add_message(SubAgentGroup)` 推送 ③ 移除 `view_messages` 字段（涉及 88+ 测试更新）。

**[TRAP]** BaseMessage vs MessageViewModel 维度混淆：`completed_len_at_round_start` 是 BaseMessage 长度，`prefix_len` 是 VM 索引，两者非 1:1。`prefix_len` 必须用 `round_start_vm_idx`，`drain` 必须钳位。

**[INFO]** `MessageViewModel` 已不再包含 `message_id` 字段。SubAgentGroup 使用 `instance_id: Option<String>` 标识。

## 主布局

单 Session 垂直切分（Sticky Header → Messages → Attachment Bar → Panel Area → Input → Status Bar → BG Agent Bar）。高度优先级：Status Bar 固定 3 行 → Input 动态（3~40% 屏幕）→ 面板（60-75% 屏幕）→ 其余分配给消息区。

### 界面组件

| 组件 | 文件 | 说明 |
|------|------|------|
| Welcome Card | `ui/welcome.rs` | 空消息时替代显示，ASCII Art + 功能要点 + 命令提示 |
| Sticky Header | `ui/main_ui/sticky_header.rs` | 滚动时顶部固定显示最后 Human 消息摘要 |
| Attachment Bar | `ui/main_ui/attachment.rs` | 图片附件标签列表，Input 正上方 |
| Input Area | `edit_utils.rs` | `tui_textarea::TextArea` 封装，高度动态 |
| Hints 浮层 | `ui/main_ui/popups/hints.rs` | `/` 前缀命令匹配，输入框上方 |
| @提及弹窗 | `app/at_mention/mod.rs` | `@` 触发文件搜索，200ms 节流 |
| BG Agent Bar | `ui/main_ui/bg_agent_bar.rs` | 后台 Agent 列表，8 色循环 |

### 弹窗系统（双轨）

**v1 路径（生产活跃）**：统一通过 `InteractionPrompt` 枚举互斥管理（3 种：Approval/Questions/Rewind）。OAuth 和 Setup Wizard 通过 `GlobalUiState` 独立管理。
- **HITL 审批**（`popups/hitl.rs`）：`InteractionPrompt::Approval` + `app/agent_ops_interaction.rs::handle_acp_request_permission`（JSON-RPC RequestPermission）
- **AskUser 问答**（`popups/ask_user.rs`）：`InteractionPrompt::Questions` + `handle_acp_elicitation`（JSON-RPC Elicitation）
- **Rewind 确认**（`popups/rewind.rs`）：`InteractionPrompt::Rewind`（双击 Esc 触发）
- **OAuth 授权**（`popups/oauth.rs`）：`GlobalUiState.oauth_prompt`
- **Setup Wizard**（`popups/setup_wizard.rs`）：`GlobalUiState.setup_wizard`

**v2 路径（前端就位，后端待启用）**：`State::Modal(ModalKind::Interaction(Box<dyn Handler>))`。4 个 Handler（hitl/ask_user/rewind/oauth）的 render + desired_height 全部实现（Phase 1.4）。draw_now 在 `panel_area` 渲染。**当前 ACP 层不 emit 对应 AcpEvent，路径不会触发**（见缺陷 #15）。

### 面板系统（v2 PanelState trait）

14 种 `PanelKind`（`app/panel_types.rs`）：Model/Login/Agent/Hooks/Config/ThreadBrowser（Session 作用域）；Mcp/Plugin/Cron/Tasks/Status/Memory/Betas/Workflow（Global 作用域）。

互斥组（`MutexGroup`）：Settings（Model/Login/Config）、Agent（Agent/Hooks）、Tools（MCP/Plugin/Cron/Tasks/Workflow）、Info（Status/Memory/Betas）、Thread（ThreadBrowser 独占）。

**v2 PanelState trait**（`state_machine/state.rs`）：`render + desired_height + handle_key + from_app(&App)` 工厂。`registry::create_panel(kind, app)` 工厂 0 stub，全部 14 面板迁移完成。

Slash 命令 → v2 面板端到端通路：`Command::execute → Vec<Effect> → Effect::OpenPanel(PanelKind) → main_loop create_panel(kind, app) → State::Modal(Panel) → draw_now overlay 渲染`。

8 个面板有 `from_app(app)` 构造函数从 ServiceRegistry 提取真实数据（Model/Login/Config/Mcp/Plugin/Cron/Status/Workflow）。

### Status Bar

双行布局（`ui/main_ui/status_bar.rs`）：
- **第一行**：权限模式 → 工作目录 → 模型名 → CPU% → MEM → 上下文使用率
- **第二行**：左侧瞬时状态（复制提示/后台 agent/LLM 重试/MCP/LSP）→ 右侧快捷键 hints

瞬时提示用 `Instant` + Duration 控制消失；颜色分级用 `theme::ERROR`/`WARNING`/`SAGE`；面板 hints 通过 `PanelState::status_bar_hints()` trait 注入。

### 消息区

Welcome Card 或消息列表 + 滚动条 + spinner。视口裁剪渲染（`viewport_clip`）。`MessageViewModel` 7 种变体：`UserBubble` / `AssistantBubble`（含 Text/Reasoning/ToolUse） / `ToolBlock` / `SystemNote` / `CacheWarning` / `ToolCallGroup` / `SubAgentGroup`。

## i18n

`LcRegistry` 存储在 `ServiceRegistry.lc` 中，翻译资源通过 `include!` 编译时嵌入 `locales/{lang}/main.ftl`。

`Command trait` 的 `description()` 接收 `&LcRegistry` 参数并返回 `String`。`CommandRegistry::match_prefix()` 和 `list()` 均需 `&LcRegistry`。

## 状态管理

**`ServiceRegistry` 与 `GlobalUiState`**：`App` 状态拆分为 `ServiceRegistry`（跨会话共享：config/MCP/cron/provider）和 `GlobalUiState`（纯 UI 临时状态：高亮计时器/弹窗/鼠标检测）。

**`ServiceRegistrySnapshot`**（`panel/read_context.rs`）：从 `&App` 派生的快照，含 cwd/model_alias/provider_name/permission_mode。`build_v2_panel_read_context` 使用真实 snapshot + State.view_models() 数据。

**`CommandRegistry::dispatch` 借用限制 [TRAP]**：`&self` + `&mut App` 冲突，用 `std::mem::take` + put-back 解决。dispatch 期间不可改变 `app.session_mgr` 的 session 实例。

## Compact 事件处理

**[TRAP]** `handle_compact_completed` 必须三步清理：① `pipeline.clear()` ② `pipeline.restore_completed(messages)` ③ `RebuildAll { prefix_len: 0 }`。缺少任一步都会导致旧消息残留或 system 消息泄漏。禁止在 TUI 层触发 auto-compact——所有触发判断在 executor 内部。

**[TRAP]** `restore_completed(messages)` 会把 system 消息放入 completed 列表。re_inject 产生的 System 消息不应渲染。`round_start_vm_idx` 和 `completed_len_at_round_start` 必须正确设置。
