# Workflow 完成后 Agent 自动响应 — 实现计划

**关联 issue**：`spec/issues/2026-06-23-workflow-completion-system-reminder-no-agent-reaction.md`
**方案**：A（TUI 侧检测 + pending_messages 驱动）
**创建日期**：2026-06-23
**状态**：计划中

---

## 0. 背景总结

workflow 完成 → 通知双路径到达：

```
session 级 forwarder（executor.rs:971-1021）
├─ Path B: agent_notify_tx → WorkflowMiddleware.notification_buffer → 等 execute_prompt() drain
└─ Path A: EventSink → TUI → handle_background_task_completed()
    └─ 但 workflow 的 child_thread_id=None，不匹配 background_agents
       → 当前无自动 continuation 逻辑
       → agent 静默，需用户手动发消息触发 drain
```

**改动思路**：在 TUI 侧检测 `agent_name.starts_with("workflow:")`，推入 `pending_messages`，通过已有的 `flush_pending_messages` → `submit_message` 自动触发新一轮 `execute_prompt()`（后者开头 drain Path B 缓冲 = agent 看到通知）。

---

## 1. 改动清单

| 文件 | 改动类型 | 行数估计 |
|------|----------|----------|
| `peri-tui/src/app/agent_events_bg.rs` | 新增 workflow 分支 | +20 行 |
| `peri-tui/src/app/agent_ops/polling.rs` | 新增 pending_messages 空闲 flush | +6 行 |

### 1.1 `peri-tui/src/app/agent_events_bg.rs` — workflow 自动 continuation 分支

**插入位置**：`handle_background_task_completed()` 第 400 行（`(true, false, false)` return 之前）

**改动内容**：

```rust
// Workflow 完成 → agent 自动 continuation。
// workflow 的 child_thread_id=None，不匹配 background_agents（非 SubAgent），
// 需独立检测并触发自动响应：推入 pending_messages → poll_agent 下一帧 flush → submit_message
// → execute_prompt() 开头 drain 通知缓冲 → agent 处理结果。
if agent_name.starts_with("workflow:") {
    let workflow_name = agent_name.strip_prefix("workflow:").unwrap_or(&agent_name);
    let continuation_text = format!(
        "<system-reminder>\nWorkflow '{}' has completed. Please review the results from \
         .claude/workflow-runs/{}/state.json.\n</system-reminder>",
        workflow_name, task_id,
    );

    let loading = self.session_mgr.current_mut().ui.loading;
    self.session_mgr
        .current_mut()
        .messages
        .pending_messages
        .push(continuation_text);

    if !loading {
        // Agent 空闲：返回 should_return=true 退出本轮事件处理，
        // poll_agent 下一帧检查 pending_messages 并触发 flush → submit_message。
        return (true, false, true);
    }
    // Agent 运行中：pending_messages 由 handle_done → flush_pending_messages 消费。
}
```

**边界处理**：

| 场景 | 行为 |
|------|------|
| agent 空闲时 workflow 完成 | `should_return=true` → 下一帧 `poll_agent` 检测 `pending_messages` → `flush_pending_messages` → `submit_message` |
| agent 运行中 workflow 完成 | 推入 `pending_messages` → agent Done → `handle_done` → `flush_pending_messages` → `submit_message` |
| 多个 workflow 并发完成 | 每个完成事件推一条 continuation。仅第一条触发 submit_message（flush 一条/帧），后续在 Done 后逐条消费。Path B 缓冲在第一次 execute_prompt drain 时全部清空 |
| workflow 完成 + 用户同时输入 | 用户的 `submit_message` 先触发 execute_prompt → drain 所有通知 → agent 看到通知+用户消息。完成后 pending_messages 中的 continuation 再次触发 new round → 但通知缓冲已空 → continuation 变为无上下文的提示。**缓解**：`submit_message` 内部 `reset_for_new_round()` 重置 bg_task_state，不影响 workflow |

### 1.2 `peri-tui/src/app/agent_ops/polling.rs` — pending_messages 空闲 flush

**插入位置**：`poll_agent()` 第 37 行之后（`pending_continuation` 检查之后，ACP 通知 drain 之前）

**改动内容**：

```rust
// 消费缓冲的自动消息（workflow completion、cron、channel 等），
// agent 空闲时立即提交第一条，其余留待后续 Done 周期逐条消费。
// 注意：pending_continuation 检查已排在前面，bg continuation 优先于 pending_messages。
if !self.session_mgr.current_mut().ui.loading {
    if !self.session_mgr.current_mut().messages.pending_messages.is_empty() {
        self.flush_pending_messages();
        return true;
    }
}
```

**为什么放在 `pending_continuation` 之后、ACP drain 之前**：
- 优先级：bg continuation（SubAgent 结果注入）> pending_messages（workflow/cron/channel 自动触发）
- ACP drain 之前是因为 workflow 完成事件来自 ACP 通道，上一帧的 `should_return=true` 中断了 drain 循环——如果 pending_messages flush 也放后面，会被 drain 循环再次阻塞

---

## 2. 数据流验证

```
workflow 完成（run 019ef3ca）
    ↓
session 级 forwarder（executor.rs:971-1021）
    ├─ Path B: agent_notify_tx.send(notif_text)
    │    → WorkflowMiddleware buffer → 等待 drain
    │
    └─ Path A: EventSink.push_event(BackgroundTaskCompleted { agent_name: "workflow:smoke-test", task_id: "019ef3ca...", ... })
         → MpscTransport → TUI AcpTuiClient
         → AcpNotification::AgentEvent { event: BackgroundTaskCompleted }
         → handle_agent_event → handle_background_task_completed()
         → [NEW] agent_name.starts_with("workflow:") ✓
         → continuation_text = "<system-reminder>..." 推入 pending_messages
         → loading=false → return (true, false, true)

poll_agent 下一帧（~16ms 后）
    → pending_continuation.take() = None（跳过）
    → [NEW] !loading && !pending_messages.is_empty() ✓
    → flush_pending_messages()
    → submit_message(continuation_text)
    → ACP session/prompt RPC

execute_prompt() 开头（executor.rs:350-356）
    → drain notification buffer → history.push(BaseMessage::human(
        "[后台任务 019ef3ca 已完成] workflow-smoke-test (21379ms, 5 agents, N tool calls)"
      ))
    → history.push(BaseMessage::human(
        "<system-reminder> Workflow 'smoke-test' has completed. Please review..."
      ))
    → build_and_execute_agent → LLM 看到通知 → 读取 state.json → 产出摘要 ✓
```

---

## 3. 不改变的部分

- **ACP executor.rs**：零改动。Path B 通知缓冲 + drain 逻辑不变
- **Existing PULL 架构**：通知仍通过 Path B buffer → drain 注入，Path A 仅作为**触发器**（无需更改变 PULL 模型）
- **pending_continuation 机制**：bg SubAgent continuation 不受影响（优先级在前）
- **cron/channel pending_messages 路径**：与 workflow 共享同一队列，互不冲突
- **handle_done / handle_interrupted**：现有 `flush_pending_messages()` 调用不变，workflow continuation 自然融入

---

## 4. 验证步骤

1. 启动 TUI → 发 `/ultracode 让我们简单测试一下 workflow 的能力, 看看会出什么问题`
2. Agent 调用 Workflow 工具 → workflow 在后台运行
3. Agent 完成当前轮次（Done）→ 返回 prompt 交互
4. **Workflow 完成** → TUI 收到通知 → **< 1 帧内自动提交 continuation prompt**
5. Agent 读取 `.claude/workflow-runs/{run_id}/state.json` → 产出摘要
6. 验证：agent 应能在 workflow 完成后**无需用户手动输入**即自动响应

---

## 5. 风险与缓解

| 风险 | 缓解 |
|------|------|
| workflow 完成瞬间用户也提交了输入 | `prompt_lock` 串行化两次 session/prompt。第一次 drain 清空通知缓冲，第二次 continuation 变为普通消息（无通知上下文）——可接受 |
| 多个 workflow 并发 → pending_messages 堆积 | `flush_pending_messages` 每次只 pop 一条，agent Done 后逐条消费。Path B 缓冲在第一次 drain 时全部清空，后续 continuation 无重复通知 |
| `submit_message` 在 handle_background_task_completed 内部的借用冲突 | 不直接调用 submit_message，改用 return should_return=true + poll_agent 下一帧 flush |
| workflow agent_name 前缀 `"workflow:"` 的稳定性 | `executor.rs:990` 硬编码此格式，与 SubAgent 的命名空间隔离，不会冲突 |
