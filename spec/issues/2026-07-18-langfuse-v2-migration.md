# Langfuse 追踪迁移到 v2 事件体系

**状态**：Done
**优先级**：中
**创建日期**：2026-07-18
**父 issue**：`spec/issues/2026-07-18-v1-executor-event-retirement.md` (后续)

## 背景

v1 ExecutorEvent 全量下线完成后，TUI 和 ACP 层已完全切换到 v2 事件（RenderEvent/StateEvent/ObserveEvent）。但 **Langfuse 追踪仍依赖 v1 ExecutorEvent**：

- `peri-acp/src/session/executor_helpers.rs::forward_langfuse_event()` — 主追踪函数，匹配 ExecutorEvent 变体
- `peri-acp/src/agent/workflow_agent.rs::FnEventHandler` — 同一路径

这是 v1 ExecutorEvent 枚举**最后的消费方**。此迁移完成后，ExecutorEvent 才能被完全删除。

## 当前 Langfuse 事件映射（不完全列表）

| ExecutorEvent 变体 | Langfuse 追踪行为 |
|-------------------|------------------|
| `TextChunk` | （不追踪） |
| `AiReasoning` | （不追踪） |
| `ToolStart` / `ToolEnd` | ToolBatch span / Observation |
| `TurnStarted` / `TurnEnded` | Turn span lifecycle |
| `LlmCallStart` / `LlmCallEnd` | Generation span |
| `CompactStarted` / `CompactCompleted` | Compact span |
| `TurnCommitted` | flush current span |
| `TodoUpdate` | （不追踪） |
| `StateSnapshotMeta` | context usage snapshot |

## 对应 v2 事件

| v2 Event | 已有？ | Langfuse 对应 |
|----------|:-----:|--------------|
| `ObserveEvent::LlmCallStart` / `LlmCallEnd` | ✅ | ✅ 直接映射 |
| `ObserveEvent::CompactStarted` / `MessagesCompacted` | ✅ | ✅ 直接映射 |
| `ObserveEvent::StageStarted` / `StageEnded` | ✅ | ✅ 直接映射 |
| `RenderEvent::TurnCompleted` | ✅ | ✅ flush span |
| `StateEvent::StateSnapshot` | ✅ | ✅ context snapshot |

## 迁移步骤

1. 在 `executor_helpers.rs` 中新增 `forward_langfuse_event_v2(event: V2Event)` 函数
2. 从 v2 事件提取 Langfuse span/observation/usage 数据
3. 在 `forwarder.rs` 的 `spawn_eventbus_forwarder` 中注册回调
4. 验证 Langfuse 仪表盘中 span 层级、统计量与迁移前一致
5. 验证通过后移除 `forward_langfuse_event()`（v1 版本）
6. 移除 `spawn_event_pump` 中 legacy event_tx 消费

## 影响范围

| 文件 | 变更 |
|------|------|
| `peri-acp/src/session/executor_helpers.rs` | 新增 v2 forward 函数 |
| `peri-acp/src/event/forwarder.rs` | 注册 Langfuse 回调 |
| `peri-agent/src/agent/events_v2.rs` | 无需改（事件已够） |

## 验证标准

- [ ] `cargo test -p peri-acp --lib` 全过
- [ ] Langfuse 仪表盘 Turn/Generation/Tool span 层级正确
- [ ] usage 上报数据量不降
- [ ] 迁移后 `forward_langfuse_event()` 可安全删除
