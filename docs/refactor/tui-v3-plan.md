# TUI v3 重构计划

**起点**：2026-06-30（cron `*/15 * * * *` 每 15 分钟自动推进）
**触发**：用户反馈"消息流渲染、弹窗、状态机均无法正常工作"
**约束**：ACP 层完好，仅重构 TUI；用户睡眠期间自主推进

## 现状（scout 调研结论）

通过 4 个并行 Explore agent + 关键源码核实，确认以下设计缺陷：

### 🔴 P0 致命缺陷

| # | 问题 | 根因位置 |
|---|---|---|
| 1 | 消息流双源冲突 | `state.current_turn` (v2) 与 `view_messages` (legacy via `handle_agent_event`) 同时累积 |
| 2 | 键盘双重执行 + 同步覆盖 | `is_sm_handled_shortcut` 未过滤 Ctrl+A/U/W + Backspace/Delete/Home/End/Left/Right；2b 同步 TextArea→InputState 无条件覆盖 SM 修改 → Backspace 失效 |
| 3 | v2 Interaction Handlers 全死代码 | HitlHandler/AskUserHandler/RewindHandler/OauthHandler 完整但**从未被实例化**；生产代码无 `ModalState::Interaction(Box::new(...))` |
| 4 | Handler trait 签名根本错误 | `render(&self, area: (u16, u16))` 无 Frame；`handle_key(&mut self, key: char)` 丢修饰键。即使启用 v2 Handler 也不能工作 |
| 5 | HandlerOutput::Submit → 杜撰 ACP 方法 | `Effect::SendToAcp { method: "interaction/submit" }` —— ACP 协议里不存在 |
| 6 | Modal 期间所有 AcpEvent 丢弃 | `modal.rs:109/417` + `state.rs:156` view_models() 在 Modal 返回 &[] |

### 🟠 P1 严重缺陷

| # | 问题 | 根因 |
|---|---|---|
| 7 | Streaming → Modal 不可达 | OpenPanel 只在 Idle 触发（main_loop.rs:340） |
| 8 | Status 事件全 drop | streaming.rs:79 + idle.rs:141 静默丢弃 TokenUsage/WorkflowProgress/CostUpdate |
| 9 | State::Switching 简陋 | 丢 Key/Paste/AcpEvent，AcpDisconnected 不恢复 Idle |

### 🟡 P2 死代码

| # | 问题 |
|---|---|
| 10 | 11 个 Effect 无人 emit：CopyToClipboard/InterruptAgent/ShowNotification/UpdateConfig/SwitchSession/ClearTextSelection/OpenRewindPrompt/OpenThreadWithFeedback/MemoryPanelOpenEditor/AskUserScroll/ClearPendingMessages |
| 11 | modal.rs TLS stubs（dispatch_key/dispatch_mouse_tls/dispatch_paste_tls）非测试代码引用 |
| 12 | `transition_to_idle_with_effects` 标 dead_code |

### 🟢 P3 文档过时

| # | 问题 |
|---|---|
| 13 | `peri-tui/CLAUDE.md` 大量引用已删除的 message_pipeline/MessagePipeline/RenderThread/RenderCache/AdaptiveChunkingPolicy/ephemeral_notes |
| 14 | 根 CLAUDE.md Effect 数量说 32 实际 27；Alt+M/Alt+Shift+M 不存在 |

## 重构 Phases

### Phase 0：进度跟踪基础设施 ✅
- [x] 创建 `docs/refactor/tui-v3-plan.md`（本文件）
- [x] 创建 artifact HTML 看板
- [x] TaskCreate 9 个 Phase 任务
- [ ] 启动 Design Review workflow 评估关键决策（v2 Handler trait 重构方向）

### Phase 1.1：修复键盘双重执行 ⏳
**核心决策**：双向同步冲突（SM 修改 InputState 被 2b 同步覆盖）。候选方案：
- **A. 删除 SM 输入编辑 arm**：Backspace/Delete/Left/Right/Home/End/Ctrl+A/U/W 改 no-op，keyboard fallback 独占
- **B. 修复同步顺序**：SM 处理后立即 to_textarea，让 textarea 反映 SM 修改后再 2b 同步
- **C. 让 is_sm_handled_shortcut 拦截 + 改 2b 条件同步**：仅在 keyboard fallback 未执行时跳过 2b

→ 倾向 A（最低风险、最快），但需评估测试影响。

### Phase 1.2：重构 Handler trait 签名 ⏳
当前：`render(&self, area: (u16, u16))` + `handle_key(&mut self, key: char)`
目标：`render(&self, f: &mut Frame, area: Rect, ctx: &HandlerReadContext)` + `handle_key(&mut self, key: KeyEvent) -> HandlerOutput`
影响：state.rs Handler trait + 4 个 handlers + modal.rs dispatch_key_with_ctx + 测试

### Phase 1.3：启用 v2 Interaction Handlers ⏳
- main_loop 接收 `HitlPending/AskUser/Rewind/OAuth` ACP 事件时构造对应 Handler → `State::Modal(Interaction)`
- 修正 `HandlerOutput::Submit` → 正确的 ACP 方法名（`session/approveTool` / `session/elicitationResponse` / 等）

### Phase 1.4：实现 Handler.render() + 删除 v1 popup ⏳
- 迁移 popups/{hitl,ask_user,rewind,oauth}.rs 渲染逻辑到 Handler.render()
- 删除 keyboard/popups.rs 对应分支
- 删除 App.interaction_prompt / global_ui.oauth_prompt 字段

### Phase 1.5：修复 Modal 期间 AcpEvent 丢弃 ⏳
- modal.rs 不再 drop AcpEvent；State::Modal 持有 saved_view/saved_current_turn
- state.rs `view_models()` 在 Modal 返回 saved 而非 &[]

### Phase 1.6：Streaming → Modal 可达 + Status 事件处理 ⏳
- OpenPanel 在 Streaming 也触发（保存 current_turn 到 Modal，关闭时恢复）
- streaming.rs §4.3 实现 status 事件（TokenUsage → status bar，WorkflowProgress → tracker）

### Phase 2：消息源单一化 ⏳
- 删除 `agent_ops/mod.rs` handle_agent_event 中的 origin_messages/view_messages 维护
- 所有渲染从 `state.view + current_turn` 取
- 删除 `app.global_ui.v2_view_models` 桥接（直接用 state）

### Phase 3：死 Effect 清理 + Switching 完善 ⏳
- 11 个无人 emit 的 Effect：接通或在枚举中删除
- modal.rs TLS stubs 删除（仅保留 #[cfg(test)] 路径）
- State::Switching 完善（AcpDisconnected 恢复 Idle，Key/Paste 转发到目标 session）

### Phase 4：CLAUDE.md 同步 ⏳
- 重写 peri-tui/CLAUDE.md（删除 message_pipeline/RenderThread/ephemeral_notes 引用）
- 根 CLAUDE.md 同步 v3 进展（Effect 数量、v3 Phase 完成、删除 Alt+M 引用）

## 执行原则

1. **每个 cron 窗口（15 分钟）一个 Phase**——避免半成品
2. **每完成一个 Phase 立即 git commit + push**——保证可恢复
3. **测试驱动**——每 Phase 完成后跑 `cargo test -p peri-tui --lib`
4. **artifact 持续更新**——可视化进度
5. **遇到设计岔路启动 Design Review workflow**——多视角 PK

## 进度日志

### 2026-06-30 cron #1
- ✅ 创建 4 个并行 Explore agent 调研（消息流/弹窗/状态机/main_loop）
- ✅ 整合 P0-P3 缺陷清单
- ✅ TaskCreate 9 个 Phase
- ⏳ Phase 0 进行中：plan 文档（本文件）+ artifact + PROGRESS
- ⏭ 下一步：启动 Design Review workflow 评估 Phase 1.1 方向

### 2026-07-01 cron #7 — Phase 1.4 全部完成 + 死代码路径确认

**完成内容**：Phase 1.4-ask_user（commit `89738a7e`）+ Phase 1.4-oauth（commit `ee9fe41c`）+ 清理（commit `17932367`）。4 个 v2 Interaction Handlers 的 render + desired_height 全部就位，draw_now 渲染入口打通。

**关键调研结论（新发现的 P0 缺陷 #15）**：v2 Interaction Handler 路径在生产环境**仍是死代码**。根因：
1. **ACP 层不 emit HitlPending/AskUser/RewindPreview/OauthNeeded AcpEvent**：`peri-acp/src/event/mapper.rs:265-268` 明确注释「当前 ExecutorEvent 中无专门的 HitlPending 变体，HITL 审批通过 UserInteractionBroker 的 ask/confirm 直接交互，不经过事件管道」。
2. **AcpEvent 枚举本身没有这些变体**：`peri-acp/src/event/mod.rs` 的 `pub enum AcpEvent` 仅含 StateSnapshot/TextChunk/ReasoningChunk 等流式事件，无 HitlPending/AskUser/Rewind/OAuth 变体。
3. **生产路径走 v1 JSON-RPC**：HITL 经 `RequestPermission` 请求 → `app.handle_acp_request_permission` → 设置 `InteractionPrompt::Approval` → v1 popup 渲染。AskUser 经 `Elicitation` 请求 → `handle_acp_elicitation`。
4. **acp_notifier 包装层**：发送 `TuiEvent::AcpEvent { event: "agent-event", data: { ..., event: <AcpEvent DTO> } }`。v2 状态机的 `AcpEventData::decode("agent-event", ...)` 落入 `_ => Unknown` 分支，HitlPending 路径完全不会触发。
5. **`grep -r 'AcpEvent::HitlPending'` 零结果**：全代码库无任何位置构造这 4 个 AcpEvent 变体（仅 `peri-acp-types/src/event_data.rs` 中定义了 DTO struct）。

**影响评估**：
- ✅ **不影响生产**：v1 路径完整运行，HITL/AskUser/Rewind/OAuth 通过 v1 popup 正常工作
- ✅ **Phase 1.3/1.4 代码有价值**：v2 路径作为前端预留接口，等 ACP 层未来扩展 ExecutorEvent::HitlPending 等变体并 emit 后立即可用
- ❌ **Phase 1.3 描述误导**：progress.html 之前说「启用 v2 Interaction Handlers」实际只是「代码路径就位」，真实启用需 ACP 层配合

**真正的启用条件**（不在 TUI 重构范围内）：
- ACP 层扩展 `ExecutorEvent` 添加 HitlPending/AskUser/RewindPreview/OauthNeeded 变体
- `event/router.rs` 把这些变体映射到对应的 AcpEvent DTO
- `event/mapper.rs` 添加 Category ② 路由位（已预留，见 mapper.rs:264-268）
- 决定是否废弃 JSON-RPC `RequestPermission`/`Elicitation` 路径（双轨切换）

**当前窗口决策**：不做架构改动（风险高 + 用户睡眠），仅记录此发现。继续推进安全的 Phase 4 CLAUDE.md 同步。Phase 2 消息源单一化也受此影响 — 在 ACP 层切换前，v2 view_models 实际上是空的，UI 仍依赖 v1 view_messages。
