# ExecutorEvent 枚举最终退役——彻底删除 v1 事件体系

**状态**：Partial
**优先级**：中
**创建日期**：2026-07-18
**前置依赖**：
- `spec/issues/2026-07-18-langfuse-v2-migration.md`
- `spec/issues/2026-07-18-events-v2-mapper-removal.md`

## 背景

v1 ExecutorEvent 枚举经历了三阶段逐步削减：

| 阶段 | 操作 | 剩余变体 |
|------|------|:--:|
| Phase C | 删除 22 个 deprecated 变体 | ~18 |
| Langfuse 迁移后 | 移除 Langfuse 消费路径 | ~10 |
| events_v2_mapper 删除后 | 桥接不再需要 | 可能全部 |

当前剩余的非 deprecated 变体大致分为三组：

| 分组 | 变体 | 消费方 |
|------|------|--------|
| Cat ① SessionUpdate | TextChunk, AiReasoning, ToolStart, ToolEnd, TodoUpdate, LlmCallEnd, MessageAdded | ACP StdioEventSink (IDE) |
| Langfuse | TurnStarted, TurnEnded, Stage*, StateSnapshotMeta 等 | forward_langfuse_event() |
| event_tx 底层 | TurnCommitted, TurnSuspended, AgentExecutionFailed 等 | spawn_event_pump() |

## 删除前置条件

- [ ] **Langfuse 迁移到 v2 事件** → `forward_langfuse_event()` 下线
- [ ] **events_v2_mapper 删除** → v2→v1 桥接退役
- [ ] **StdioEventSink 迁移到 v2** → IDE 客户端不再依赖 Category ① SessionUpdate
- [ ] **spawn_event_pump() 下线** → event_tx 通道不再需要

## 删除步骤

### 1. 确认零消费者
```bash
# 全仓库搜索 ExecutorEvent 引用
grep -r 'ExecutorEvent' peri-*/src/ --include='*.rs' | grep -v 'test' | grep -v '#\[deprecated'
```

### 2. 删除枚举定义
- `peri-agent/src/agent/events.rs` — 删除整个 `ExecutorEvent` enum
- 同步删除 `AgentEvent` type alias（若存在）

### 3. 清理 event_tx 通道
- `executor_helpers.rs` — 删除 `event_tx: UnboundedSender<ExecutorEvent>` 以及整个 `spawn_event_pump()`
- `agent_context.rs` / `build_and_execute_agent_v2()` — 不再构造 event_tx

### 4. 清理 StdioEventSink
- `event_sink.rs` — `StdioEventSink::push_event()` 改为接收 v2 事件或直接删除
- `event/mapper.rs` — 删除 `map_event()`（最后一个调用方）

### 5. 删除 events.rs 残留
- 删除 `StopReason` 枚举（若仅 ExecutorEvent 使用）
- 删除 `inject_source_agent_id` 引用清理
- `events_test.rs` 同步删除

### 6. 更新 mod.rs + lib.rs + prelude
- 移除 `pub mod events;`
- 移除所有 `pub use` 相关 re-export

## 影响文件（估）

| 文件 | 操作 |
|------|------|
| `peri-agent/src/agent/events.rs` | **删除** |
| `peri-agent/src/agent/events_test.rs` | **删除** |
| `peri-agent/src/agent/mod.rs` | 移除 pub mod |
| `peri-agent/src/lib.rs` | prelude 清理 |
| `peri-acp/src/event/mapper.rs` | **删除**（最后调用方） |
| `peri-acp/src/event/mapper_test.rs` | **删除** |
| `peri-acp/src/event/mod.rs` | 清理 |
| `peri-acp/src/session/executor_helpers.rs` | 删除 event_tx + event_pump |
| `peri-acp/src/session/event_sink.rs` | StdioEventSink 重构 |
| `peri-acp/src/agent/workflow_agent.rs` | 删除 FnEventHandler |

## 验证标准

- [ ] `cargo build --workspace` 编译通过
- [ ] `cargo test --workspace` 全过
- [ ] `grep -r 'ExecutorEvent' peri-*/src/` 返回零匹配
- [ ] `events.rs` 文件不再存在
