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

**完整迁移计划**：详见 `docs/refactor/phase2-migration-plan.md`（2026-07-01 cron #9 起草）。

拆分 6 个子阶段（每子阶段独立 cron 窗口）：
- **Phase 2.1**（task #15）：测试 helper 包装 / 标记 legacy fallback deprecated。`message_area.rs:189-206` legacy 路径在测试活跃（`main_ui::render` 被 headless_test/popups_test 直接调用），生产环境死代码。
- **Phase 2.2**（task #16）：扩展 `render` 签名为 `render(f, app, state, panel_height)`，删除 `app.global_ui.v2_view_models` 桥接字段。
- **Phase 2.3**（task #17）：SubAgent 流式状态扩展。`agent_events_bg.rs:175-196` 就地 mutation 模式无法用不可变 ViewModel 替代，方案 B：App 层 `HashMap<agent_id, SubAgentStatus>` + render_v2_vm 合并。
- **Phase 2.4**（task #18）：新增 `Event::PushSystemNote(String)` + state_machine append 到 `state.view`。迁移 4 个 v1 命令通知点。
- **Phase 2.5**（task #19）：手动验证 Rewind/Compact/Interrupted 后 v2 state.view 重建，删除 `apply_rebuild_all` + `ephemeral_notes`。
- **Phase 2.6**（task #20）：删除 `MessageState.view_messages / round_start_vm_idx / message_cache / ephemeral_notes` 字段。

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

**关键调研结论（缺陷 #15，原 P0 → 修正为 P2）**：

**最初误判**：以为「v2 状态机路径生产死代码」（基于 acp_notifier 发送 "agent-event" 包装的判断）。

**修正后的真相**：v2 路径**部分**死代码。具体区分：

1. ✅ **v2 流式事件路径（生产活跃）**：ACP server 通过 `peri/unstable-event` JSON-RPC notification 发送 `{ event: "view-commit" / "text-chunk" / ... }`（见 `peri-acp/src/session/event_sink.rs:167-186`）。acp_notifier 对 `UnstableEvent` 直接转发 method 名（不包装为 "agent-event"，见 `acp_notifier.rs:130-143`）。v2 状态机 decode 成功，正确更新 state.view / state.current_turn。**v1 路径 no-op**（`acp_bridge.rs:67` 注释「Handled by the v2 state machine (path 1a). Legacy path is no-op.」）。

2. ❌ **v2 Interaction Handler 路径（死代码）**：4 个 Handler（HitlPending/AskUser/RewindPreview/OauthNeeded）期望 ACP 层 emit 对应的 `peri/unstable-event`，但 `peri-acp/src/event/router.rs:149-152` 明确注释「ExecutorEvent has no dedicated HitlPending variant. HITL approval is handled via a separate channel (UserInteractionBroker). Skipped here.」。

3. **生产交互路径**：
   - HITL：JSON-RPC `session/requestPermission` 请求 → `app.handle_acp_request_permission` → 设置 `InteractionPrompt::Approval` → v1 popup
   - AskUser：JSON-RPC `session/elicitation`（CreateElicitation）请求 → `handle_acp_elicitation` → 设置 `InteractionPrompt::Questions`
   - Rewind：双击 Esc 触发，v1 路径（不通过 AcpEvent）
   - OAuth：v1 `GlobalUiState.oauth_prompt`

**真正的启用条件**（不在 TUI 重构范围内）：
- ACP 层扩展 `ExecutorEvent` 添加 HitlPending/AskUser/RewindPreview/OauthNeeded 变体
- `event/router.rs` 添加路由（已预留位置，见 router.rs:149-152）
- 决定是否废弃 JSON-RPC `RequestPermission`/`Elicitation` 路径（双轨切换）

**对 Phase 2 的影响**：可以推进！v2 状态机的 `state.view` + `current_turn` 在生产上实际工作（通过 unstable-event），是渲染源。v1 `view_messages` 由 TurnCommitted/StateSnapshot/Compact 等 AcpEvent 维护（通过 v1 路径的 handle_agent_event），但 render_messages 走 V2 path 不读它。所以 Phase 2 删除 view_messages 是可行的，但需谨慎处理 v1 依赖。

### 2026-07-01 cron #8 — 缺陷 #15 严重性修正 + Phase 2 调研

**修正**：缺陷 #15 从 P0 降为 P2。原判断「v2 死代码」过度悲观，实际只有 4 个 Interaction Handler 死代码（不影响生产），流式路径完全工作。progress.html 同步修正。

**Phase 2 调研结论**：v2 状态机的 `state.view` 在生产上通过 `peri/unstable-event` 的 `view-commit` 事件正常更新（acp_notifier.rs:130 直接转发 method 名，acp_bridge.rs:67 v1 no-op 让 v2 独占）。生产渲染走 V2 path（draw_now 总是设置 `v2_view_models = Some(...)`）。v1 `view_messages` 仅被 v1 path 的 TurnCommitted/StateSnapshot/Compact 事件维护，**渲染时不读**（render_messages V2 path 不读 view_messages）。

### 2026-07-01 cron #9 — Phase 2 计划起草 + 子阶段拆分

**完成**：
- 深度调研 Phase 2 范围（Explore agent 报告 + 关键文件核实）
- 发现 legacy fallback 在测试活跃（`message_area.rs:189-206` 被 headless_test/popups_test 直接调用的 ~30+ 测试覆盖）→ 不能直接删
- 发现 SubAgent 流式状态（`agent_events_bg.rs:175-196`）是最大阻塞 → 方案 B：App 层 status map
- 拆分 6 个子阶段，每个独立可提交、可回滚
- 起草 `docs/refactor/phase2-migration-plan.md`
- TaskCreate #15-#20 子任务 + blocks 依赖关系
- 看板 + 主计划文档同步更新

**未完成**：
- 子阶段未启动（本窗口仅规划）
- 测试 helper 设计未细化

**下一步**：cron #10 启动 Phase 2.1（task #15）

### 2026-07-01 cron #18 — Phase 2.6 step 6 完成（SubAgentGroup view_messages 全退役）

**完成**：commit `b5db7dbe` + dashboard 同步 `aed80e81`。净减 832 行。

**变更范围**：
- `agent_ops/subagent.rs`：删除 `handle_subagent_start` 中 `apply_add_message(SubAgentGroup)` 推送
- `agent_ops/mod.rs`：删除 `SubAgentEnd` 的 `iter_mut` 突变 + ToolBlock 回退（~60 行）
- `agent_events_bg.rs`：删除 `BackgroundTaskCompleted` 的 `iter_mut` 突变 + ToolBlock 回退（~100 行）
- `headless.rs`：`HeadlessHandle::render` 从 `subagent_status` 合成 v2 `SubAgentGroupData` 占位符（headless 路径无 ACP ViewCommit，需手动注入让 probe 流经）
- `headless_test.rs`：退役 7 个 v1 测试（`test_subagent_group_basic/sliding_window/assistant_chunk`、`test_background_task_notification`、`test_subagent_group_preserved_after_done_reconcile`、`test_diagnostic_bg_subagent_group_disappears`、`test_diagnostic_fork_plus_background_subagent_group`）+ 2 个诊断辅助函数（`bg_diag_count_subagent_groups`、`bg_diag_print_vms`）

**关键突破**：SubAgentGroup 渲染路径完全脱离 view_messages 写入侧。`SubAgentStatusMap` 是唯一权威源（`start` / `complete_foreground` / `complete_background` / `incr_tool_step` / `append_child_text`）。生产渲染完全通过 `SessionSubAgentProbe` 读取。

**测试覆盖**：1041 passed, 0 failed。覆盖路径：
- `subagent_status.rs` 单元测试（start / complete_foreground / complete_background / TTL / 容量）
- `test_subagent_group_renders_child_content_via_probe`（e2e v2）
- `test_subagent_child_tool_renders_on_screen`（e2e v2）
- `test_bg_completed_before_done_triggers_continuation`（pre_done_completions 路径）

### Phase 2.6 step 7 规划（下一个 cron 窗口）

**目标**：退役 `apply_add_message` 的 UserBubble 路径（`agent_submit.rs:47`）。

**复杂度**：高。UserBubble 写入 view_messages 后被以下读者依赖：
- `handle_done`（`lifecycle.rs:50-61`）：查找最后一个 VM 设置 `AssistantBubble.is_streaming = false`
- `handle_interrupted`（`lifecycle.rs:128-154`）：`rposition UserBubble` + 检查其后是否有 ToolCallGroup/ToolBlock 决定中断后行为
- `apply_rebuild_all`（`agent_render.rs:34`）：UserBubble 去重逻辑
- `thread_ops.rs`：线程切换时复制 view_messages
- `ask_user_ops.rs`：AskUser 提示查找
- `command/core/gc.rs`：统计 VM 数

**前置依赖**：v2 ViewStore 需要提供：
1. `last_user_bubble_index()` → `Option<usize>`
2. `last_assistant_bubble_mut()` → 可变引用（设置 `is_streaming = false`）
3. `has_tool_calls_after(idx)` → `bool`

或更激进：彻底放弃「在 view_messages 上标记 is_streaming」+「rposition UserBubble」这类索引型 API，改为：
1. `handle_done` 通过 Effect::MarkStreamingDone 通知状态机更新 `current_turn.is_streaming`
2. `handle_interrupted` 通过 Effect::CheckInterruptHasProgress 查询 v2 ViewStore

**风险**：触及核心控制流，可能引入回归。**建议**：先用 Explore agent 全面调研所有 view_messages 读者，再做架构决策（workflow：survey → design → execute）。

**估算**：1 个完整 cron 窗口（15min）调研 + 设计；1 个窗口执行 + 测试。
