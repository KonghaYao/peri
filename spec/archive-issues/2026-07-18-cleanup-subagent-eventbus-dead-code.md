# 清理 SubagentStarted 的 EventBus 残留路径（死代码 + 双重发送陷阱）


> 归档于 2026-07-20，原路径 spec/issues/2026-07-18-cleanup-subagent-eventbus-dead-code.md
**状态**：Fixed
**优先级**：低
**类型**：技术债
**创建日期**：2026-07-18

## 问题描述

`2026-07-16-eventbus-unified-emission` 架构迁改中，`SubagentStarted`/`SubagentStopped` 因时序死结未能搬入 EventBus v2 路径，被保留在旧的 `handler.on_event()` 路径上。但迁改时预先铺设的 `ObserveEvent → v2_channel → v2_bridge → bridge_tx` 通道并未彻底清除，形成了「有收有发但无双发」的残留代码——在生产中永不触发，但未来若有人补齐发射端，会形成双重发送冲突。

## 症状详情

发现于 `2026-07-18-subagent-tool-cards-regression-empty` 修复过程中。

### 死代码：`v2_bridge.rs` 的 `ObserveEvent::SubagentStart` 消费分支

**位置**：`peri-tui/src/kit/v2_bridge.rs:95-104`

```rust
ObserveEvent::SubagentStart {
    agent_name,
    child_agent_id,
    is_background,
    ..
} => Some(AcpEventData::SubagentStarted {
    agent_id: child_agent_id.to_string(),
    agent_name,
    is_background,
}),
```

`ObserveEvent::SubagentStart` 在生产代码中**从未被 emit**——所有 emit 调用仅存在于测试文件：
- `subagent_event_forwarder_test.rs`
- `events_v2_test.rs`
- `events_v2_mapper_test.rs`

因此这支分支**永远不会执行**，是死代码。

### 双重发送陷阱：`forwarder.rs` 的观察事件转发

**位置**：`peri-acp/src/event/forwarder.rs:241-249`

```rust
biased; observe => {
    let _ = v2_channel.try_send_v2_event(ev);   // → v2_bridge → bridge_tx
    on_event(exec_ev);                           // → event_sink → peri/agent_event → bridge_tx
}
```

当前 `SubagentStarted` 的**唯一活跃路径**是 `event_sink → peri/agent_event`（即 2026-07-18 修复的路径）。但如果未来有人在 SubAgent 创建时 `emit_observe(ObserveEvent::SubagentStart{...})`，`forwarder.rs` 会**同时通过两条路径**发送该事件：

| 路径 | 机制 | 问题 |
|------|------|------|
| A: `try_send_v2_event` | v2_channel → v2_bridge → bridge_tx | 生成 `SubagentStarted` |
| B: `on_event(exec_ev)` | event_tx → event_sink → peri/agent_event → bridge_tx | 再生成一个 `SubagentStarted` |

TUI 收到两次 `SubagentStarted` → 创建两个 SubAgentAccumulator → 内部工具卡片重复 / 状态错乱。

### 架构背景

`2026-07-16-eventbus-unified-emission` 为 SubAgent 铺设了完整的 `ObserveEvent` 通道，但评估发现时序死结——SubAgent 的 EventBus 在 `build_v2_subagent_context` 创建，而 `SubagentStarted` 需要在此之前 emit。因此该事件被**永久保留**在旧路径，EventBus 通道未能完工即废弃。残留的收/发代码段就是这次要清理的对象。

## 涉及文件

| 文件 | 代码段 | 状态 |
|------|--------|------|
| `peri-tui/src/kit/v2_bridge.rs:95-104` | `ObserveEvent::SubagentStart` → `SubagentStarted` | 死代码（消费端） |
| `peri-tui/src/kit/v2_bridge.rs:105-109` | `ObserveEvent::SubagentStop` → `SubagentStopped` | **同样死代码**——`ObserveEvent::SubagentStop` 也从未被 emit |
| `peri-acp/src/event/forwarder.rs:244` | `try_send_v2_event(observe_ev)` | 双重发送陷阱（发射端） |

## 期望改进方向

1. 删除 `v2_bridge.rs` 中 `ObserveEvent::SubagentStart` / `SubagentStop` 的死分支
2. 在 `forwarder.rs` 的 `try_send_v2_event` 位置添加注释，说明 SubAgent 事件不经过此通道（如未来启用需先删除 v2_bridge 分支）
3. 建议在 `v2_bridge.rs` 文件头部添加注释说明哪些事件被有意排除

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-18 | — | Open | agent | 创建。发现于 `2026-07-18-subagent-tool-cards-regression-empty` 分析过程中 |

## 修复记录

（留空）
