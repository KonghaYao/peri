# Phase 2：消息源单一化迁移计划

**目标**：删除 `MessageState.view_messages` + `MessageState.message_cache` + `app.global_ui.v2_view_models` 桥接，所有渲染从 `State.view + current_turn` 取。

**前置结论**（2026-07-01 cron #8 调研）：v2 流式路径在生产上实际工作（`peri/unstable-event` JSON-RPC notification 正常更新 state.view），v1 `view_messages` 仅由 v1 path 的 TurnCommitted/StateSnapshot/Compact 事件维护，**渲染时不读**（render_messages V2 path 不读 view_messages）。Phase 2 可行。

## 子阶段拆分（每个子阶段一个 cron 窗口）

### Phase 2.1：迁移测试到 V2 路径 ⏳

**问题**：`message_area.rs:189-206` 的 legacy fallback 路径在生产环境**死代码**（draw_now 总是设置 `v2_view_models = Some(...)`），但测试代码（`headless_test.rs` / `popups/*_test.rs` 等 ~30+ 个测试）通过 `main_ui::render(f, &mut app, None)` 直接调用，**不走 draw_now**，因此 legacy fallback 是测试专用路径。

**方案**：A/B/C 选择：
- **A. 测试 helper 包装**：新建 `test_render(f, app, state)` helper 自动构造 v2_view_models；批量替换测试调用点
- **B. 保留 legacy + 标记 deprecated**：保留 `render_messages` 的 legacy 分支，加 `#[deprecated]` 注释；未来 Phase 2.x 再删
- **C. 推迟到 Phase 2.x 后**：先迁移其他读取点，最后再处理测试

→ **倾向 B**（最低风险）。本窗口只加注释，不动逻辑。

### Phase 2.2：删除 `v2_view_models` 桥接字段 ⏳

**问题**：`app.global_ui.v2_view_models: Option<Vec<ViewModel>>` 是 per-frame 临时字段（draw_now 设置 → render 消费 → draw_now 清除），存在的唯一原因是 `render` 函数签名只有 `&mut App`，没有 `&State`。

**方案**：扩展 `render` 签名为 `render(f: &mut Frame, app: &mut App, state: &State, v2_panel_height: Option<u16>)`。所有调用点（`main_loop::draw_now` + 测试 helper）同步更新。删除 `global_ui_state.rs:35` 的 `v2_view_models` 字段 + `apply_context.rs:233/271` 的设置/清除。

**影响文件**：
- `peri-tui/src/ui/main_ui/mod.rs`（render 签名）
- `peri-tui/src/ui/main_ui/message_area.rs`（render_messages 签名 + 删除 `take()` 改为参数）
- `peri-tui/src/runtime/apply_context.rs`（draw_now 传参）
- `peri-tui/src/app/global_ui_state.rs`（删除字段）
- 测试 helper（`headless.rs::wait_for_render` 等）

**风险**：`render` 签名变更影响面广，但机械改动。**可独立提交**。

### Phase 2.3：SubAgent 流式状态扩展 ⏳（最大阻塞）

**问题**：`agent_events_bg.rs:175-196` 通过索引就地修改 `view_messages[idx]` 的 `SubAgentGroup.is_running / final_result / is_error / total_steps` 字段。v2 `ViewModel::SubAgentGroup(SubAgentGroupData)` 是不可变 DTO，无这些运行时字段。

**方案**：A/B/C 选择：
- **A. 扩展 DTO**：`peri-acp-types::SubAgentGroupData` 添加 `is_running: bool` / `final_result: Option<String>` / `is_error: bool` / `total_steps: usize` 字段（需 ACP 层 emit；**超出 TUI 重构范围**）
- **B. TUI 层状态 map**：`App` 维护 `HashMap<agent_id, SubAgentStatus>`，渲染时合并 v2 ViewModel 的静态部分 + App 的动态状态
- **C. 重构为 rebuild**：每次 SubAgent 事件触发整个 view 重建（性能差）

→ **倾向 B**（TUI 自包含，不依赖 ACP 层）。设计草案：

```rust
// 新增 app/subagent_status.rs
pub struct SubAgentStatus {
    pub is_running: bool,
    pub final_result: Option<String>,
    pub is_error: bool,
    pub total_steps: usize,
}
pub struct SubAgentStatusMap(pub HashMap<String, SubAgentStatus>);

// render_v2_vm 时合并：
// SubAgentGroup(agent_id) -> 查 status_map -> 渲染时附加 is_running/final_result 指示
```

**风险**：需要修改 `render_v2_vm` 函数签名传入 status map。中等复杂度。

### Phase 2.4：命令通知迁移到 v2 事件流 ⏳

**问题**：v1 命令路径直接 push view_messages：
- `command/agent.rs:22,28` — `/agent` 切换通知
- `command/panel/model.rs:27` — `/model` 切换失败通知
- `app/agent_ops/mod.rs:273` — 某分支 push
- `thread_ops.rs:108` — 历史线程恢复

v2 状态机是纯函数 `(State, Event) -> (State, Vec<Effect>)`，没有"push system note"路径。

**方案**：新增 `Event::PushSystemNote(String)` 变体 + state_machine 在 Idle/Streaming 状态下 append 到 `view`（ViewModel::SystemNote）。

**实现步骤**：
1. `state_machine/event.rs` 添加 `Event::PushSystemNote(String)` 变体
2. `state_machine/transitions/idle.rs` + `streaming.rs` 处理：`state.view.push(SystemNote)`
3. main_loop 在执行命令后 emit 该事件
4. 删除 v1 命令路径中的 `apply_add_message(SystemNote::...)` 调用

**风险**：事件流改造，但每个命令点机械迁移。低风险。

### Phase 2.5：apply_rebuild_all / ephemeral_notes 退役 ⏳

**问题**：v1 的 `apply_rebuild_all(prefix_len, tail_vms)` 包含 ephemeral_notes 锚点保存/恢复逻辑。v2 view-store 的 replace 语义（commit）应该已经覆盖这个场景，但需要确认。

**验证步骤**：
1. 手动测试 Rewind（`/rewind`）+ Compact（`/compact`）+ interrupted（Ctrl+C）三个场景
2. 确认 v2 state.view 在这些事件后正确重建
3. 如果 v2 view-store 语义完整，直接删除 `apply_rebuild_all` + `ephemeral_notes` 字段

**风险**：手动测试耗时，但逻辑改动机械。中风险（依赖前述 Phase 2.4 完成）。

### Phase 2.6：删除 view_messages 字段 ⏳（最终）

**前置**：Phase 2.1-2.5 全部完成。

**步骤**：
1. `grep -rn 'view_messages' peri-tui/src/` 应该零结果
2. 删除 `MessageState.view_messages` 字段
3. 删除 `MessageState.round_start_vm_idx` 字段（不再有 VM 索引维度）
4. 删除 `MessageState.message_cache` 字段（v2 路径每帧重建）
5. 删除 `MessageState.ephemeral_notes` 字段
6. 删除 `agent_render.rs::apply_add_message` / `apply_rebuild_all`
7. 删除 `message_state.rs::push_system_note`

**风险**：删除最后残留，应该机械。低风险。

## 子阶段依赖关系

```
Phase 2.1 (测试 helper) ─┐
                         ├─→ Phase 2.2 (删除桥接)
                         │
Phase 2.3 (SubAgent)    ─┼─→ Phase 2.4 (命令通知)
                         │
                         ├─→ Phase 2.5 (apply_rebuild_all 退役)
                         │
                         └─→ Phase 2.6 (最终删除 view_messages)
```

Phase 2.1 和 2.3 可并行；2.2、2.4 依赖 2.1；2.5 依赖 2.4；2.6 是终点。

## 风险控制

1. **每个子阶段一个 commit**：失败可单独 revert
2. **每子阶段后跑 `cargo test -p peri-tui --lib`**：必须全过
3. **手动验证关键场景**：流式输出、SubAgent 后台、HITL、Compact、Rewind
4. **遇到设计岔路**：暂停本窗口，记录到本文档，等待下个 cron 窗口

## 进度日志

### 2026-07-01 cron #9 — Phase 2 计划起草

**完成**：
- 深度调研 Phase 2 范围（Explore agent 报告）
- 发现 legacy fallback 在测试中活跃（不能直接删）
- 发现 SubAgent 流式状态是最大阻塞（DTO 扩展 vs TUI 层 status map）
- 拆分 6 个子阶段，每个独立可提交
- 起草本文档

**未完成**：
- 子阶段 2.1 未启动（仅调研结论）
- 测试 helper 设计未细化

**下一步**：cron #10 启动 Phase 2.1（B 方案：保留 legacy + 加 deprecated 注释）
