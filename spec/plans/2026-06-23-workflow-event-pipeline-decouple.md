# 计划：解耦 workflow 事件管线，脱离 per-turn 生命周期

**创建日期**：2026-06-23
**关联 issue**：
- `spec/issues/2026-06-23-ultracode-skill-parallel-example-wrong-signature.md`（文档）
- `spec/issues/2026-06-23-workflow-panel-no-realtime-update.md`（Bug：面板空白）
- `spec/issues/2026-06-23-workflow-completion-notification-missing.md`（Bug：完成通知丢失）

## 目标

修复三个已确认根因的 workflow 问题。Issue 1 是独立文档修复；Issue 2 与 Issue 3 共享同一架构根因——**整个 workflow 事件管线（进度 + 完成）被绑死在 per-turn / per-execution 的 agent 生命周期上，但 workflow 是 fire-and-forget、必然跨轮存活**。

## 架构（核心设计决策）

**现状（根因）**：

| 事件 | 当前路径 | 生命周期 | 问题 |
|---|---|---|---|
| 进度 `WorkflowProgress` | `tool.rs:155` → `event_handler.on_event` → per-execution `event_tx`（executor.rs:934-938 包的 `FnEventHandler`） | 单次 `execute_prompt`，结束时 `close_channel`（executor.rs:1137） | workflow agent 在轮结束后才 emit → 发往已关闭通道 → `if let Some(tx)` 为假 → 静默丢弃 |
| 完成 `BackgroundTaskCompleted` | `registry.complete` → broadcast → per-turn forwarder（builder.rs:417-439）→ per-turn `bg_notification_tx` → `bg_notification_rx`（builder.rs:512 → `with_notification_rx` → executor `drain_notifications`） | 单轮，轮结束 receiver drop | 慢 workflow 跨轮完成 → consumer 已 drop → 丢失 |

**目标设计**：引入一个 **session 级常驻 `AgentEventHandler`**，在 `session/new` 创建**一次**，内部持有一个 session 级 mpsc channel；一个**永不取消的 forwarder 任务**消费该 channel，直接调 `EventSink::push_event`（`TransportEventSink`，session 级、底层 transport 跨轮存活）。因为 TUI 侧 `acp_client/client.rs:90 run_pump` 是常驻后台任务、`peri/agent_event` 通知**不依赖 executor 状态**恒定转发，所以 workflow 进度/完成无论哪一轮、executor 是否在跑，都能到达 TUI。

```
[WorkflowTool 进度]  ─┐
                      ├─► session 级 mpsc ─► 单 forwarder ─► EventSink::push_event ─► peri/agent_event ─► TUI run_pump（常驻）
[registry 完成]      ─┘   （session/new 建一次）                                              ├─► WorkflowProgress → 面板刷新
                                                                                                └─► BackgroundTaskCompleted → [后台任务] 通知
```

**分层合规**：`WorkflowMiddleware`（peri-middlewares）不能依赖 peri-acp 的 `EventSink`，但可用 peri-agent 的 `AgentEventHandler` trait。session 级 handler 在 peri-acp（有 `EventSink`）里构造，经 `WorkflowMiddlewareAdaptor` 注入 session 级 `WorkflowMiddleware`（**仅在 session/new 时注入一次**，而非每轮 `build_agent`）。

## 任务分解

> 原则：每个任务独立可验证、可单独 commit。Issue 1 先行（独立、零风险）。Issue 2/3 按设计顺序实现，每个任务带测试。

---

### 任务 1：修复 ultracode SKILL.md 的 parallel 示例签名（Issue 1）

**文件**：`peri-middlewares/src/skills/builtin/skills/ultracode/SKILL.md`

**改动**：
1. 第 53 行原语描述：`parallel([...promises])` → `parallel([...factories])`，补注「入参为返回 promise 的零参工厂函数（thunks），不是 promise」。
2. 第 67-71 行示例：每个 `agent(...)` 改为 `() => agent(...)`：
   ```javascript
   const [security, perf, bugs] = await parallel([
     () => agent('...', { label: 'security', ... }),
     () => agent('...', { label: 'performance', ... }),
     () => agent('...', { label: 'bugs', ... }),
   ])
   ```
3. 在示例下补一句警告框：
   > ⚠️ 直接传 `agent(...)` 调用（而非工厂函数）会被 runtime 静默吞掉——`parallel` 会把每个 Promise 当函数调用，抛 `TypeError` 后 catch 返回 `null`，workflow 以「假成功 completed + 全 null 返回值」结束，且无 `journal.jsonl`。必须用 `() => agent(...)`。

**验证**：
- `grep -n "parallel(\[" peri-middlewares/src/skills/builtin/skills/ultracode/SKILL.md` 确认无直接 `agent(...)` 调用形式。
- 重新加载 builtin skill，照新示例跑一个 3-agent workflow，确认 `return_value` 非空、有 `journal.jsonl`。

**commit**：`docs(workflow): fix parallel() example to use factory functions in ultracode skill`

---

### 任务 2：peri-acp 新增 session 级 EventSink 转发 handler（Issue 2/3 基础设施）

**文件**：`peri-acp/src/session/agent_pool.rs`（或 `event_sink.rs` 新增类型）

**新增类型** `WorkflowEventForwarder`：
- 持有 `Arc<dyn EventSink>`、`session_id: String`、`context_window: u32`、`Arc<UnboundedSender<ExecutorEvent>>`。
- 构造时 spawn 一个永不取消的 forwarder：消费 session 级 mpsc receiver，对每个 `ExecutorEvent` 调 `event_sink.push_event(&session_id, &event, context_window)`。
- 暴露 `handler(&self) -> Arc<dyn AgentEventHandler>`：返回一个 `FnEventHandler`，闭包把事件 `send` 到 session 级 sender（**不经过任何 per-turn 通道**）。

**关键代码骨架**：
```rust
pub struct WorkflowEventForwarder {
    tx: Arc<UnboundedSender<ExecutorEvent>>,
}
impl WorkflowEventForwarder {
    pub fn spawn(
        event_sink: Arc<dyn EventSink>,
        session_id: String,
        context_window: u32,
    ) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                event_sink.push_event(&session_id, &event, context_window).await;
            }
        });
        Self { tx: Arc::new(tx) }
    }
    pub fn handler(&self) -> Arc<dyn AgentEventHandler> {
        let tx = Arc::clone(&self.tx);
        Arc::new(peri_agent::agent::events::FnEventHandler::new(move |event| {
            let _ = tx.send(event); // session 级，永不关闭
        }))
    }
}
```

**验证（单元测试）**：
- 用 `concurrent_bg_agent_test.rs` 里的 fake EventSink（记录 push_event 调用）模式：spawn forwarder，发送 1 条 `WorkflowProgress` + 1 条 `BackgroundTaskCompleted`，断言 fake sink 收到 2 条、顺序正确。
- 断言 forwarder task 在「无活跃 executor」时仍把事件推给 sink（直接 await 一小段后检查）。

**commit**：`feat(acp): add session-level WorkflowEventForwarder backed by EventSink`

---

### 任务 3：session/new 注入 session 级 handler（Issue 2/3 接线）

**文件**：`peri-acp/src/session/executor.rs`（`execute_prompt` / session 建立处）、`peri-acp/src/agent/builder.rs`、`peri-middlewares/src/workflow/mod.rs`

**改动**：
1. session 建立时（session/new，**非 build_agent**）spawn 一次 `WorkflowEventForwarder`，存入 session 级 `SessionState`（与 session 级 `WorkflowMiddleware` 同生命周期，参考 CLAUDE.md「Frozen Data Flow / agent_pool session 级缓存」）。
2. `WorkflowMiddlewareAdaptor`（mod.rs:105）改为持有这个 **session 级** handler（而非 per-turn `event_handler`）；`collect_tools`（mod.rs:125）用它构造 `WorkflowTool`。
3. `builder.rs:414` 不再把 per-turn `event_handler` 传给 adaptor 用于进度（adaptor 改从 session 级 forwarder 取 handler）。

**验证（集成测试，`peri-acp/tests/`）**：
- 复刻 `concurrent_bg_agent_test.rs` 的 transport 捕获模式，构造一个会跨轮的 workflow（或 mock WorkflowTool 立即返回 run_id、随后异步 emit 事件）：在「execute_prompt 已 Done」之后 emit 一条 `WorkflowProgress`，断言 transport 收到对应 `peri/agent_event` 通知。

**commit**：`refactor(acp): inject session-level workflow event handler at session/new`

---

### 任务 4：进度事件改走 session 级 handler（Issue 2 收尾）

**文件**：`peri-workflow/src/tool.rs`

**改动**：
- `tool.rs:149-171` 进度转发器：`handler_for_progress` 已经是 `event_handler`（任务 3 后已替换为 session 级 handler）。本任务确认 `WorkflowTool` 持有的 `event_handler` 来源已是 session 级（来自任务 3 的 adaptor），无需再改 tool.rs 的转发逻辑——只需删掉对 per-turn 路径的依赖注释。
- 若 `WorkflowTool` 仍显式接收 per-turn handler，改为从 session 级 adaptor 获取。

**验证（端到端）**：
- 启动 TUI，跑一个真实派发 agent 的 workflow（工厂函数写法），执行期间 `/workflows` 打开面板，断言面板**实时刷新**（phase 推进 / agent / token 出现），且在启动轮结束后仍在更新。

**commit**：`fix(workflow): route progress events through session-level handler so panel updates live`

---

### 任务 5：完成通知改走 session 级 handler（Issue 3 收尾）

**文件**：`peri-acp/src/agent/builder.rs`、`peri-workflow/src/registry.rs`

**改动**：
1. `builder.rs:417-439` 的 per-turn forwarder（`subscribe_notifications` → `bg_notification_tx`）改为：把 `WorkflowTaskResult` 转成 `AgentEvent::BackgroundTaskCompleted(...)` 发到**任务 2 的 session 级 channel**（通过 forwarder.handler() 对应的 sender），不再用 per-turn `bg_notification_tx`。
   - 构造 `BackgroundTaskCompleted` 时复用现有 `to_notification()` 的摘要逻辑（builder.rs:428-432 的 `Workflow '{}' finished with status {:?} ({}ms, {} agents)`）。
2. 保留 `registry.complete()` 的 session 级 broadcast（给 to_notification 注入下一轮 ReAct 用，若需要 agent 感知完成），但 TUI 可见的 `[后台任务]` 通知改由 session 级 channel 投递。
3. **附带**：修正硬编码 `agent_count: 0`（`tool.rs:238`）——从 `WorkflowResult` 取真实 agent 计数（registry/runner 已有 `agent_count` 字段可用）。

**验证（端到端）**：
- 启动 TUI，跑一个**慢** workflow（真实 agent、耗时数秒、跨轮完成）。在启动轮结束后，断言仍收到「`[后台任务 … 已完成]`」通知，且 `工具调用/耗时/agents` 字段为真实值（非 0）。
- 对照：原先 run2 不通知，修复后应可靠通知。

**commit**：`fix(workflow): deliver completion notification via session-level channel across turns`

---

## 集成测试

新增 `peri-acp/tests/workflow_cross_turn_events_test.rs`：
- 构造 transport 捕获（fake `AcpTransport` 记录所有 `send_notification`）。
- 模拟 session/new → execute_prompt 启动一个会异步 emit 事件的 workflow → execute_prompt 返回（Done）→ 随后 emit `WorkflowProgress` + 完成事件。
- 断言：Done 之后发出的事件仍产生 `peri/agent_event` 通知（进度）与对应 `BackgroundTaskCompleted` 通知（完成）。

## 验证命令

```bash
# 单元 + 集成测试
cargo test -p peri-acp --test workflow_cross_turn_events_test
cargo test -p peri-acp workflow_event_forwarder
cargo test -p peri-workflow

# 整体
lefthook run pre-commit          # fmt / check / clippy
cargo run -p peri-tui -- -a      # 手动跑 workflow + 开 /workflows 面板验证实时刷新 + 完成通知
```

## 风险与回退

- **风险**：改动 `WorkflowMiddlewareAdaptor` 接线影响现有 per-turn 进度路径（若仍有调用方依赖）。缓解：任务 3 用集成测试守住「Done 后事件仍送达」。
- **风险**：session 级 forwarder task 泄漏（session 关闭时未清理）。缓解：session 销毁时 drop sender，forwarder `recv` 返回 None 自然退出（代码骨架已体现）。
- **回退**：每个任务独立 commit，可逐个 revert。任务 1（文档）与任务 2-5（代码）完全解耦，可分别合入。

## 执行顺序

任务 1（独立、立即可做）→ 任务 2（基础设施 + 单测）→ 任务 3（接线 + 集成测试）→ 任务 4（进度端到端）→ 任务 5（完成端到端）。
