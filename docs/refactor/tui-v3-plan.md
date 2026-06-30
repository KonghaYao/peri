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

### 2026-07-01 cron #19 — Phase 2.6 step 7a + 7b 完成

**workflow 调研**：启动 `phase-2-6-step-7-survey` workflow（4 个并行 Explore agent + 1 个 Plan agent 综合设计）。结论：step 7 不能在单一窗口完成（agent_submit.rs:47 的 UserBubble 写入与 handle_interrupted rposition / app/mod.rs 中断 truncate / round_start_vm_idx 不变量紧耦合），建议拆为 7a/7b/7c/7d 四个窗口。

**完成**（2 个本地 commit，均不 push）：
- `6959003a` refactor：**Phase 2.6 step 7a** — 删除 `apply_rebuild_all` 中 UserBubble 去重分支（死代码）。所有 4 个调用点（agent_compact ×2 / agent_ops::mod ×1 / agent_ops::lifecycle ×1）要么传 `prefix_len=0` 要么传空 tail，dedup 条件 `prefix_len > 0 && !tail.is_empty()` 永远为 false。净减 17 行，零行为变更。1041 测试全过。
- `8c931318` feat：**Phase 2.6 step 7b** — v2 ViewStore 添加 query helpers。两个 free function（`last_user_bubble_index(view)` / `has_tool_cards_after(view, idx)`）+ 两个 ViewStore method（包装 free function）。8 个新单元测试覆盖 empty / no UserBubble / 多 UserBubble / idx 越界 / 无 ToolCard / 有 ToolCard / 嵌套组忽略 / method 入口。leaf addition 无行为变更。1049 测试全过（+8）。

**设计要点**：
- free function 形式让调用方可传 `ViewStore.view_models` 或 `state.view_models()`（含 current_turn）任意切片
- 顶层扫描（与 v1 `view_messages.iter().skip()` 语义一致），不递归到 `SubAgentGroup` / `CollapsedGroup` 内嵌 ToolCard（嵌套组由 `SubAgentStatusMap` 独立管理）
- 暴露 v2 ViewStore API 的存在：未来 step 7c 可直接 `state.view.last_user_bubble_index()` 替代 v1 `view_messages.iter().rposition()`

**关键发现**：v2 SM Enter transition（`idle.rs:252-266`）**不**推送 UserBubble 到 `state.view`，意味着生产渲染当前在 ACP view-commit 回声到达前不显示用户消息（latent 行为，非 step 7 引入）。step 7d 必须先修复这个 SM 缺口再退役 `apply_add_message(user_vm)`。

**下一步**：
- step 7c（下窗口）：迁移 `handle_interrupted`（lifecycle.rs:128-179）+ `app/mod.rs` 中断路径（443-455）到 v2 ViewStore helpers。需将 `&State` 或 `&[ViewModel]` 传入处理器（当前只有 `&mut App`）。
- step 7d（未来）：SM Enter 推送 UserBubble 到 `state.view` + 退役 `agent_submit.rs:47` apply_add_message(user_vm)
- step 7e（未来）：退役 ~20 个 headless_test 中 `apply_add_message(MessageViewModel::user(...))` 直接 push 模式 → 删除 `view_messages` 字段

---

### cron #20 — 2026-07-01 — step 7d 调研 + 保守实施

**调研工作流 `wjdz1xyqm`**（4 agent：3 trace + 1 synthesize）：
- trace:handle_interrupted — 调用链 `main_loop → handle_acp_event → handle_acp_notification → handle_agent_event → handle_interrupted`。state 是 main_loop 局部变量（line 50），不传入 dispatch。
- trace:interrupt_fallback — `interrupt` fallback 路径（app/mod.rs:424-486）**在生产是死代码**（acp_client 始终 Some，line 419 提前返回）。仅测试触发。
- trace:state_flow — 3 个方案：A 传参 / B App 快照（architecturally regressive）/ C Effect::InterruptRecovery（~50 行，太大）。推荐 A。
- synthesize:plan — **颠倒依赖顺序确认**：7d 必须先于 7c。理由：若 7c 先做，submit 后第一次 ViewCommit 之前 state.view 无 UserBubble，rposition 返回 None → unwrap_or(0) → 整个视图被清空（regression）。

**step 7d 实施 `745a7020`**（保守版本）：
- idle.rs Enter 转换（line 252-266）：在构造 StreamingState 前 push `ViewModel::UserBubble(UserBubbleData { text: text.clone() })` 到 `state.view`
- **保留** `agent_submit.rs:47` 的 `apply_add_message(user_vm)`（不退役）—— 避免中断路径在 7c 完成前回归（view_messages 仍需 UserBubble）
- 4 个新测试：`test_enter_pushes_userbubble_to_state_view` / `test_enter_does_not_push_userbubble_for_slash_commands` / `test_empty_enter_does_not_push_userbubble` / `test_enter_preserves_prior_view_when_pushing_userbubble`
- 1053 测试全过（1049 + 4 新）

**ACP view_mapper 验证**：`view_mapper.rs:186` 确认 ACP 层在 ViewCommit 中**会**生成 UserBubble，因此 SM 推送的 raw-text placeholder 会在第一次 ViewCommit 时被 wholesale 替换为 canonical UserBubble（含附件格式化）。短暂闪烁与 v1 view_messages 现有行为一致。

**step 7c 详细执行计划**（cron #21+）：
- 方案 A（传 `&[ViewModel]` 参数）~30-40 行，触及 7 文件
- 关键风险 1：ToolCard (v2) vs ToolCallGroup|ToolBlock (v1) 语义匹配验证（view_mapper 已扁平化 ToolCallGroup 为多个 ToolCard，应该匹配）
- 关键风险 2：current_turn 数据丢失 —— 早期中断期间 current_turn 中的 ToolCard 未包含在 state.view_models()（仅 committed base），需用 for_render() 复合切片（committed + current_turn）
- 关键风险 3：interrupt fallback 是死代码 —— 工作流建议直接删除（acp_client 始终 Some）

---

### cron #21 — 2026-07-01 — step 7c 完成（中断路径全量迁移到 v2 ViewStore）

**完成**（commit `cd883e0a`，1056 测试全过 +3）：

**方案 A 实施**（workflow `wjdz1xyqm` 设计）—— 通过调用链传 `&[ViewModel]` 切片：
- `Effect::PollAgent` 处理器在 main_loop 捕获 `state.view_models()` 快照一次
- 参数透传链：`main_loop::handle_acp_event` → `handle_acp_notification` → `handle_agent_event` → `handle_interrupted`
- `handle_interrupted` 全量切换到 v2 ViewStore helpers：
  - `last_user_bubble_index(view_slice)` 替换 v1 `view_messages.iter().rposition(UserBubble)`
  - `has_tool_cards_after(view_slice, idx)` 替换 v1 `view_messages.iter().skip().any(ToolCallGroup|ToolBlock)`
  - 指标字段重命名 `messages_in_state` → `view_vm_count`（用 `view_slice.len()`）

**关键风险 #2 解决（current_turn 数据丢失）**：
- 问题：`StreamingState::into_idle()` 丢弃 `current_turn`，中断时已流式产出的 text/reasoning/tool cards 会消失
- 修复：streaming.rs `TurnInterrupted` 处理器在 `into_idle()` 前**先**将 `current_turn.view_models()` 持久化到 `state.view`，确保 `handle_interrupted` 的 `has_tool_cards_after` 正确检测工具进展（匹配 v1 语义）
- 3 个新测试覆盖：persist tool cards / persist streaming text / empty current_turn extends nothing

**死代码 interrupt fallback 退役**（关键发现 #3）：
- `app/mod.rs` 中生产 interrupt fallback（原 421-487 行，~65 行）确认为死代码
- 生产环境 `acp_client` 始终为 `Some`（`main.rs:816` 启动时设置），该分支永不触发
- 安全删除：view_messages.rposition / truncate / textarea-restore 逻辑全清
- 替换为最小兜底（仅 cancel_token / 清 loading 标志）

**关键不变量验证**：
- v1 `ToolCallGroup|ToolBlock` 在 v2 已扁平化为多个 `ToolCard`（view_mapper.rs 处理），`has_tool_cards_after` 顶层扫描语义匹配
- 中断后清理路径仍能识别工具进展（已 commit 的 tool cards 在 state.view 中，未 commit 的在 current_turn 中已被持久化）

**剩余 Phase 2.6 工作**（step 7e）：
- 退役 ~20 个 headless_test 中 `apply_add_message(MessageViewModel::user(...))` 直接 push 模式
- 删除 `MessageState.view_messages` / `round_start_vm_idx` 字段
- 估算影响 88+ 测试，需独立窗口

### cron #22 prep — step 7e 执行计划（细化拆分）

**完整读者清单**（来自 cron #21 末尾 Explore agent 调研）：

**生产 WRITES**（8 处）：
- `app/agent_render.rs:25` — `apply_add_message` push（核心 helper）
- `app/agent_render.rs:55-56` — `apply_rebuild_all` truncate+extend（已退役 dedup，但 truncate 仍被 handle_interrupted 调用）
- `app/agent_ops/mod.rs:240` — ToolEnd iter-mut patch ToolBlock
- `app/agent_ops/mod.rs:283` — ToolEnd orphan push
- `app/thread_ops.rs:96,103,189-195` — thread switch clear/assign
- `app/ask_user_ops.rs:146` — user answer push
- `app/agent_submit.rs:47` — UserBubble push（step 7c-after 目标）

**生产 READS**（10 处）：
- `app/agent_ops/lifecycle.rs:54` — `handle_done` last_mut() 找 AssistantBubble 设 is_streaming=false（**已确认 dead**：vm_convert.rs:293 总设 false，v2 渲染不读此字段）
- `app/agent_ops/lifecycle.rs:64` — `handle_done` 读 round_start_vm_idx 做 prefix_len
- `app/agent_render.rs:41,46` — `apply_rebuild_all` clamp
- `app/agent_ops/mod.rs:240` — `handle_tool_end` iter-mut 找匹配 ToolBlock
- `app/agent_submit.rs:49` — 读 len() 设 round_start_vm_idx
- `command/core/gc.rs:31` — gc_status 读 len() 展示
- `ui/main_ui/message_area.rs:155` — v1 fallback render 转换到 v2（仅测试触发，生产 draw_now 总传 v2_view_models=Some）
- `ui/main_ui/message_area.rs:246` — width-mismatch rebuild clone+convert
- `ui/headless.rs:105` — headless render（编译进 non-test binary，实际测试调用）

**step 7e 子拆分**（每个独立可提交）：

1. **step 7e.1** — 退役 `handle_done` 的 is_streaming 突变（dead code，最安全）
   - lifecycle.rs:50-61 整块删除
   - `recompute_hash()` 调用一并删除
   - 理由：vm_convert 总设 is_streaming=false，v2 渲染不读 v1 字段
   - 测试影响：可能影响 headless_test 中验证 is_streaming 状态的测试

2. **step 7e.2** — 退役 `handle_tool_end` 直接 mutation（mod.rs:240, 283）
   - v2 路径：ACP view-commit 已扁平化 ToolCallGroup 为多个 ToolCard，tool end 状态由 view_mapper 处理
   - 需验证：生产中是否还有 ToolEnd 走 v1 path（即 ACP 不 emit 对应 unstable-event）
   - 风险：高（涉及工具状态一致性）

3. **step 7e.3** — 退役 `ask_user_ops.rs:146` 推送
   - 用户答案通过 v2 AcpEvent 流入 state.view（需确认 mapper 是否处理）
   - 风险：中

4. **step 7e.4** — 退役 `thread_ops.rs` 的 clear/assign（3 处）
   - thread switch 时 v2 state.view 也需重置（需 State::clear() 或类似 API）
   - 可能需新增 ViewStore method
   - 风险：中

5. **step 7e.5** — 退役 `headless.rs:105` v1 路径
   - 改为从 SubAgentStatusMap + 空白 view 合成（已有部分合成在 headless 中）
   - 风险：低（仅影响测试 binary）

6. **step 7e.6** — 退役 `message_area.rs:155, 246` v1 fallback render
   - 全部 caller 已传 v2_view_models=Some，fallback 路径死代码
   - 同步删除 vm_convert 调用
   - 风险：低（删除死代码）

7. **step 7e.7** — 退役 `agent_submit.rs:47` UserBubble push + 重算 round_start_vm_idx
   - 依赖：上述所有 reader 已迁移
   - round_start_vm_idx 可改为基于 state.view 计算（或彻底废弃，因为 handle_done 不再用它）
   - 风险：中

8. **step 7e.8** — 迁移 ~20 个 headless_test 中 `apply_add_message` 直接 push 模式
   - 改为通过 v2 ViewCommit 注入（或新增测试 helper）
   - 风险：低（仅测试）

9. **step 7e.9** — 删除 `MessageState.view_messages` + `round_start_vm_idx` 字段
   - 删除 `apply_add_message` / `apply_rebuild_all` / `push_system_note` v1 分支
   - 风险：低（清理）

**关键不变量**：
- vm_convert.rs 是 v1→v2 单向桥，v1 字段变化不影响 v2 渲染（除非 vm_convert 读取该字段）
- v2 ViewStore 的 commit/clear 是 wholesale 替换语义，不依赖 v1 增量 push
- ACP view_mapper 是 state.view 的唯一 canonical source（除 SM Enter 的 UserBubble placeholder）

**估算**：9 个子步骤 × 1-2 窗口/步 = 9-18 个 cron 窗口。建议优先做 .1 / .6 / .8 / .9（低风险），中风险 (.3/.4/.5/.7) 视调研深度决定，高风险 (.2) 单独窗口 + workflow 调研。

---

### cron #22 — 2026-07-01 — 功能性审计 + Modal 数据丢失修复 + message_area rebuild 修复

**工作流 `tui-functional-audit-c22`**（run_id `wn3r7nk8l`，6 agent：5 并行 Explore 审计 + 1 综合）：

5 个子系统功能性审计（message-flow / popup-interaction / state-machine-edge / keyboard-input / status-bar-misc），区分功能性 bug（用户可见）vs 架构债务（清理）。

**审计发现的 P0/P1 bug 清单**（用于后续窗口规划）：

| # | 严重性 | 问题 | 状态 |
|---|--------|------|------|
| 1 | P1 | Modal 期间 TurnDone/TurnInterrupted 丢失 saved_current_turn 数据 + 不 emit Render | ✅ 本窗口修复 |
| 2 | P1 | handle_interrupted rollback 只 truncate v1 view_messages，不 truncate v2 state.view（cancel-and-rollback 后 stale messages 残留） | 待修复（MED 风险，2 文件） |
| 3 | P1 | AskUser 答案推到 view_messages，v2 渲染路径不读 → 用户答案从聊天中消失 | 待修复（UX 决策：push_system_note vs 新 Effect） |
| 4 | P1 | SM drops TokenUsage/BudgetWarning/Progress/SystemNotification with no Render（脆弱依赖 v1 path 总 co-fire） | 待修复（依赖 Phase 2.6 view_messages 退役） |
| 5 | P1 | 双通知处理导致 streaming events 双写（current_turn + view_messages 都累积） | 架构债务（Phase 2.6 处理） |
| 6 | P2 | message_area width-mismatch rebuild 读 stale view_messages 而非 effective_v2 | ✅ 本窗口修复 |
| 7 | P2 | AskUser popup 在 Elicitation Form 0 properties 时 panic | 待修复（LOW，edge case） |

**完成**（2 个本地 commit，均不 push）：

- `a89780d0` **fix：Modal TurnDone/TurnInterrupted 数据丢失修复**。
  - 问题：modal.rs:542-549 在 Modal 期间收到 TurnDone/TurnInterrupted 时清空 `saved_current_turn` 但**不**把累积的 view_models（text/reasoning/tool cards）flush 到 `saved_view`。用户关闭 popup 时，popup 期间发生的流式输出全部丢失。同时返回 `None`（无 Effect）→ popup 背景不重绘。
  - 修复：镜像 streaming.rs:98-99 的非 Modal 路径处理 —— `turn.view_models().to_vec()` 后 extend 到 `saved_view`，再 deactivate。返回 `Some(vec![Effect::Render])`。
  - 3 个回归测试：TurnDone flushes AssistantBubble text / TurnInterrupted flushes ToolCard / TurnDone without turn is safe no-op。
  - 1059 测试全过（1056 + 3 新）。
  - 风险：LOW（guarded by `if let Some(turn)`，Modal 从 Idle 打开时 no-op）。

- `b29273d2` **fix：message_area width-mismatch rebuild 用 effective_v2 替代 view_messages**。
  - 问题：message_area.rs:244-259 在终端 resize 时 clone `view_messages` + vm_convert 重建缓存。但这重复劳动（effective_v2 在 line 150 已计算），且 view_messages (v1) 与 state.view (v2) 在 ViewCommit 前后可能短暂不一致 → resize 时渲染闪烁。
  - 修复：直接用 `effective_v2` 重建。Phase 2.6 view_messages 删除后此路径不再依赖 v1。
  - 1059 测试全过。风险：LOW（pure refactor，相同渲染输出）。

**runner-up（未修复，待未来窗口）**：

- **P1 handle_interrupted rollback v2 state.view**：MED 风险，触及 interrupt-rollback 脆弱代码（has_tool_cards_after detection / origin_messages truncation / textarea restore）。streaming.rs TurnInterrupted 处理器在 Phase 2.6 step 7c 已小心修复过 —— 改动有回归风险。建议用 dedicated test fixture for cancel-during-streaming 场景。
- **P1 AskUser answers 不可见**：UX 决策（push_system_note 改变样式 vs 新 Effect variant 跨 3+ 文件）。asleep-user 自主执行不宜做 UX 判断，留给 user-in-the-loop session。
- **P2 AskUser popup 0-properties panic**：edge case，可与 AskUser-v2 P1 一起修。

**关键教训**：workflow 的 synthesis agent 在生成 recommendation 时**直接应用了修复**（本意是 audit-only）。这是 Workflow 的 agency 边界问题 —— 如果未来希望 workflow 只 audit 不 execute，需要在 prompt 中显式禁止 Edit/Write 工具。本次 case 中结果正确（修复匹配 streaming.rs 模式），但应视为偶然幸运。

---

