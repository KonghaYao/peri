# v1 ExecutorEvent 全量下线——TUI 直连 v2 事件通道后物理删除

**状态**：已归档（被当前 v2 事件链取代）
**优先级**：高
**创建日期**：2026-07-18
**父 issue**：`spec/issues/residual-code-scan-20260718.md` (P0-1, P0-5, P2-3)

## 背景

`peri-agent/src/agent/events.rs` 中 v1 `ExecutorEvent` 枚举约 50 个变体。2026-07-18 残码扫描报告后已完成：

- ✅ 22 个变体标记 `#[deprecated(since = "0.2.0")]`（在 v2 events_v2 已有等价物）
- ✅ 下游 4 处添加 `#[allow(deprecated)]` 抑制构建警告

剩余 18 个未标记变体（v2 无明确等价物）：`StateSnapshotMeta`, `LlmRetrying`, `BackgroundTaskCompleted`, `RewindCompleted`, `CompactError`, `TodoUpdate`, `LspDiagnostics`, `BgToolStep`, `WorkflowProgress`, `SessionStarted`, `TurnStarted`, `TurnEnded`, `MiddlewareStarted`, `MiddlewareEnded`, `BudgetThresholdHit`, `WorkflowStarted`, `WorkflowEnded`

## 目标

TUI 切换到 v2 事件通道后，物理删除以下内容：

1. `events_v2_mapper.rs`（v2→v1 桥接，含 `message_id: Default::default()` 语义丢失）
2. `events.rs:477-503` 的 `inject_source_agent_id` 事后补丁函数
3. `ExecutorEvent` 枚举中已 deprecated 的 22 个变体
4. `group/mod.rs` 中 `event_tx: UnboundedSender<ExecutorEvent>`（若 group 仍无消费者）

## 当前阻断

- **TUI 仍消费 v1 ExecutorEvent**（via `acp_events.rs` + bridge）
- `events_v2_mapper.rs` 持续将 v2 事件桥接为 v1
- 下游三个 crate（peri-acp, peri-tui, peri-middlewares）依赖 v1 类型

## 迁移步骤

### 阶段 A：TUI 侧 v2 通道（peri-tui）

1. `acp_events.rs` 改从 v2 `ObserveEvent` / `RenderEvent` / `StateEvent` 直接消费
2. `acp_bridge.rs` 的 `BridgeState` 不再依赖 `ExecutorEvent`
3. `message_area/` 渲染管线改用 v2 事件类型

### 阶段 B：ACP 侧下线（peri-acp）

4. `event/mapper.rs` 移除 `ExecutorEvent` 映射分支
5. `agent/workflow_agent.rs` 移除 `#[allow(deprecated)]`
6. `session/executor_helpers.rs` 移除 v1 事件转发

### 阶段 C：物理删除（peri-agent）

7. 删除 `events_v2_mapper.rs` + 对应 `_test.rs`
8. 删除 `inject_source_agent_id` 函数
9. 从 `ExecutorEvent` 枚举物理删除 22 个 deprecated 变体
10. `events_test.rs` 移除 `#[allow(deprecated)]`

## 影响范围

| Crate | 涉及文件（估） | 风险 |
|-------|:-----------:|------|
| peri-tui | `acp_events.rs`, `acp_bridge.rs`, `acp_notifier.rs`, `message_area/` | 高——渲染主路径 |
| peri-acp | `event/mapper.rs`, `session/executor_helpers.rs`, `agent/workflow_agent.rs` | 中——事件路由 |
| peri-agent | `events.rs`, `events_v2_mapper.rs`, `group/mod.rs` | 低——仅删除 |

## 验证标准

- [ ] TUI 消息区渲染与改造前一致
- [ ] SubAgent 事件正常显示
- [ ] HITL / AskUser 弹窗功能不变
- [ ] cargo test --workspace 全过
- [ ] `peri_agent::agent::events::ExecutorEvent` 减少 22+ 变体
