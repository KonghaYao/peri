# Workflow 事件管线架构简化方案

## 问题本质

workflow 事件管线经过三轮补丁后形成**双 push 管线并存**的复杂结构——
一条 per-turn（轮结束断裂）、一条 session 级（补丁加上但重复）。
核心矛盾：**progress_store 是 session 级内存数据，但面板更新却通过 8 跳 push 事件来驱动；通知有两条平行 forwarder 做重复工作。**

```
现状 push 模型 (~300 行胶水代码):
runner.js → runner.rs → progress_tx(mpsc) → tool.rs spawn task
  → event_handler(per-turn or session?) → EventSink → mapper
  → peri/agent_event → MpscTransport → TUI pump → handle_agent_event
  → workflow_tracker.apply() → panel.update_runs()
                                                      ↓
registry.complete() → broadcast ┬─ builder.rs per-turn forwarder → bg_notification_tx → 轮结束 drop ❌
                                └─ executor.rs session forwarder → WorkflowEventForwarder → EventSink → TUI
```

---

## 简化架构

```
简化后 pull 模型 (~80 行):
                          ┌─ TUI 面板 1s 轮询 ─┐
runner.js → runner.rs ─→ progress_store ──────→ workflow/list_runs ACP
                          └─ notification task ─→ registry.complete() → broadcast
                                                       │
                                                       └─ 单 session 级 consumer
                                                          ├─ TUI (via bg_event_tx)
                                                          └─ agent 消息流 (via session buffer)
```

---

## 改动清单

### Phase 1: 进度 → pull 模型

#### 删除

| 文件 | 删除内容 | 行号 |
|------|----------|------|
| `tool.rs` | 进度转发 task（`tokio::spawn` + `handler_for_progress.on_event(...)`） | L149-171 |
| `tool.rs` | `progress_tx`/`progress_rx` channel 创建 | L145 |
| `tool.rs` | `event_handler` 字段 + `new()` 参数 | L29, L38, L45 |
| `runner.rs` | `progress_tx` 参数（`run()` 签名） | L101 |
| `runner.rs` | `progress_tx_clone.send(event)` 调用 | L257 |
| `event_sink.rs` | `WorkflowEventForwarder` 整个结构体 | L135-162 |
| `executor.rs` | `set_progress_handler` + `WorkflowEventForwarder::spawn` | L951-958 |
| `executor.rs` | session 级 notification forwarder（移入 Phase 2 重构） | L960-1002 |
| `mod.rs` (workflow) | `progress_handler` 字段 + setter/getter | L41, L109-120 |
| `mod.rs` (workflow) | `collect_tools()` handler 选择逻辑（简化为直传） | L152-155 |
| `builder.rs` | per-turn notification forwarder（`tokio::spawn` + `subscribe_notifications`） | L417-442 |
| `builder.rs` | `_bg_notification_tx_for_wf` clone | L385 |

#### 新增

| 文件 | 新增内容 |
|------|----------|
| `progress.rs` | `get_all_runs_snapshot()` → `Vec<RunSnapshot>`（序列化友好的快照） |
| `executor.rs` | ACP handler `workflow/list_runs` → 读 `progress_store`，返回 JSON |
| `workflow_panel.rs` | `on_open()` 启动 1s 轮询；`on_close()` 停止；定时调 ACP |

#### 修改

| 文件 | 修改内容 |
|------|----------|
| `tool.rs` | `event_handler` 去掉，`create_tool()` 不再需要 handler 参数 |
| `mod.rs` (workflow) | `create_tool()` 签名简化，`collect_tools()` 直接传 handler（无需选择逻辑） |
| `runner.rs` | `run()` 去掉 `progress_tx` 参数，内部只调 `apply_event` |

### Phase 2: 通知 → 单路径

#### 新增

| 文件 | 新增内容 |
|------|----------|
| `executor.rs` | **`WorkflowNotificationBuffer`**：session 级 `tokio::sync::mpsc::unbounded_channel<(BackgroundTaskResult)>` 对 |
| `executor.rs` | session 初始化时 spawn 单 notification consumer：subscribe broadcast → convert → `tx_workflow_notification.send(bg)` |
| `executor.rs` | `execute_prompt()` 开头 drain `rx_workflow_notification` → `state.add_message(BaseMessage::human(notification_text))` |
| `executor.rs` | 同时复用 `bg_event_tx`（已存在，session 级）发送 `BackgroundTaskCompleted` 到 TUI |

#### 删除

| 文件 | 删除内容 |
|------|----------|
| `builder.rs` | per-turn forwarder（Phase 1 已删，确认无残留） |
| `executor.rs` | `WorkflowEventForwarder` 依赖的 handler.on_event 通知路径（改用 bg_event_tx） |

---

## TUI 面板轮询细节

```
面板打开:
  open_workflows_panel()
  → 立即调用 workflow/list_runs 获取当前快照
  → 启动 tick timer（1s 间隔）
  → timer 触发时调用 workflow/list_runs，更新面板

面板关闭:
  → 停止 tick timer

ACP workflow/list_runs:
  request:  { "method": "workflow/list_runs" }
  response: { "runs": [{ run_id, workflow_name, status, phases: [...], agents: [...] }] }
  
  实现: executor 直接读 WorkflowProgressStore.runs，序列化 RunProgress
```

### 序列化类型（复用已有的 RunProgress）

`RunProgress`（`progress.rs:16-24`）已有 `#[derive(Serialize)]`，可直接 JSON 序列化。无需新增 snapshot 类型。

---

## 通知路径细节

```
WorkflowTaskRegistry.complete(result: WorkflowTaskResult)
  → notification_tx.send(result)  [broadcast, 已有，不改]

单 session 级 consumer（executor.rs, 首次 execute_prompt 时 spawn）:
  spawn async {
    let mut rx = wf_mw.subscribe_notifications();
    while let Ok(result) = rx.recv().await {
      let bg = BackgroundTaskResult {
        task_id: result.run_id.clone(),
        agent_name: format!("workflow:{}", result.workflow_name),
        prompt_summary: result.workflow_name.clone(),
        success: result.success,
        output: format!("... {}ms, {} agents, {} tool calls ...", ...),
        tool_calls_count: result.tool_calls_count,
        duration_ms: result.duration_ms,
        child_thread_id: None,
      };
      
      // 1. 发给 TUI（已有 bg_event_tx 是 session 级）
      let _ = workflow_bg_tx.send(ExecutorEvent::BackgroundTaskCompleted(bg.clone()));
      
      // 2. 缓冲给 agent 下一轮消息流
      let _ = workflow_notify_buffer_tx.send(bg.to_notification_text());
    }
  }

execute_prompt() 开头:
  while let Ok(notification) = workflow_notify_buffer_rx.try_recv() {
    agent_state.add_message(BaseMessage::human(format!(
      "<system-reminder>\n[后台任务已完成]\n{}\n</system-reminder>",
      notification
    )));
  }
```

---

## 影响的行数估计

| 操作 | 文件 | 行数 |
|------|------|------|
| 删除 | tool.rs（progress forwarder + event_handler） | ~40 |
| 删除 | runner.rs（progress_tx 参数 + send） | ~5 |
| 删除 | event_sink.rs（WorkflowEventForwarder） | ~30 |
| 删除 | executor.rs（set_progress_handler + old notification forwarder） | ~55 |
| 删除 | mod.rs（progress_handler + handler 选择） | ~30 |
| 删除 | builder.rs（per-turn forwarder） | ~30 |
| **删除合计** | | **~190** |
| 新增 | progress.rs（get_all_runs_snapshot） | ~10 |
| 新增 | executor.rs（workflow/list_runs handler） | ~20 |
| 新增 | executor.rs（单 notification consumer + buffer） | ~40 |
| 新增 | workflow_panel.rs（轮询逻辑） | ~30 |
| **新增合计** | | **~100** |
| **净减少** | | **~90 行** |

---

## 风险与假设

1. **假设**：ACP transport 支持 request/response（TUI → executor → TUI），用于 `workflow/list_runs`。需确认 MpscTransport 支持此模式。
2. **假设**：`bg_event_tx` 确实是 session 级。需确认——目前 builder.rs:391 创建，但 `return` 给 executor 侧，executor 侧持有并在各轮复用。如不是，需改为 session 初始化时创建。
3. **轮询开销**：1s polling 读 `RwLock<HashMap>` 几乎无开销。面板关闭停止轮询，不影响正常使用。
4. **时序**：面板打开时立即调用一次 `list_runs`，确保已有数据立刻显示，不依赖等 1s。

---

## 验证方法

1. 启动 workflow → 立即 `/workflows` 打开面板 → 面板显示 run 条目和已有 phase/agent
2. workflow 执行期间 → 面板 1s 内刷新，显示新 agent 和 phase 切换
3. workflow 完成 → TUI 弹出 `[后台任务 已完成]` 通知
4. 下一轮对话 → agent 收到 `<system-reminder>` 通知块
5. 所有 3 个相关 issue 状态更新为 Fixed
