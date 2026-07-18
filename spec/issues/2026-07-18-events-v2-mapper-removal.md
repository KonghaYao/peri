# events_v2_mapper.rs 最终删除——v2→v1 桥接退役

**状态**：Partial
**优先级**：中
**创建日期**：2026-07-18
**前置依赖**：`spec/issues/2026-07-18-langfuse-v2-migration.md`

## 背景

`peri-agent/src/agent/events_v2_mapper.rs` 的 `V2Event` 枚举和三个映射函数（`render_event_to_executor`、`state_event_to_executor`、`observe_event_to_executor`）是整个 v2→v1 桥接体系的最后连接件。

v1 ExecutorEvent 全量下线后，此文件仅被以下消费方使用：

| 消费方 | 用途 | Langfuse 迁移后？ |
|--------|------|:--:|
| `forwarder.rs` → event_tx → spawn_event_pump → Langfuse | v2→v1 转换后追踪 | 可删 |
| `subagent_event_forwarder.rs` | SubAgent v2→v1 转发 | 需重构 |
| `v2_channel.rs` | V2Event 扇出 | 可移入 events_v2.rs |
| `peri-acp/src/event/mod.rs` | V2Event re-export | 可改路径 |

## 删除前置条件

- [x] TUI 切换到 v2 直连通道（Phase A ✅）
- [x] ACP 层移除 ExecutorEvent 依赖（Phase B ✅）
- [x] ExecutorEvent deprecated 变体删除（Phase C ✅）
- [ ] **Langfuse 迁移到 v2 事件** ← 前置依赖

## 删除步骤（Langfuse 迁移完成后）

### 1. 移动 `V2Event` 枚举到 `events_v2.rs`

```rust
// peri-agent/src/agent/events_v2.rs 末尾新增：
pub enum V2Event {
    Render(RenderEvent),
    State(StateEvent),
    Observe(ObserveEvent),
}

impl V2Event {
    pub fn from_render(ev: RenderEvent) -> Self { Self::Render(ev) }
    pub fn from_state(ev: StateEvent) -> Self { Self::State(ev) }
    pub fn from_observe(ev: ObserveEvent) -> Self { Self::Observe(ev) }
}
```

### 2. 更新所有 import 路径

| 旧 import | 新 import |
|----------|----------|
| `peri_agent::agent::events_v2_mapper::V2Event` | `peri_agent::agent::events_v2::V2Event` |
| `peri_acp::event::V2Event`（re-export） | 更新 mod.rs re-export 路径 |

影响文件：
- `peri-acp/src/event/forwarder.rs`
- `peri-acp/src/event/mod.rs`
- `peri-tui/src/kit/v2_bridge.rs`
- `peri-agent/src/agent/subagent_event_forwarder.rs`

### 3. 删除 `events_v2_mapper.rs` + `events_v2_mapper_test.rs`

- 删除源文件
- 从 `peri-agent/src/agent/mod.rs` 移除 `pub mod events_v2_mapper;`
- 从 `peri-agent/src/lib.rs` prelude 移除相关 re-export

### 4. 重构 SubAgent forwarder

`subagent_event_forwarder.rs` 当前使用 `*_event_to_executor()` 做 v2→v1 转换后发送到父 Agent 的 `event_tx`。删除 mapper 后需要：
- 直接消费 v2 事件（RenderEvent/StateEvent/ObserveEvent）
- 或创建新的 v2→子 Agent 事件转发机制

### 5. 删除 `subagent_event_forwarder_test.rs`

对应测试与 `events_v2_mapper_test.rs` 同步删除。

## 影响文件

| 文件 | 操作 |
|------|------|
| `peri-agent/src/agent/events_v2.rs` | V2Event 枚举移入 |
| `peri-agent/src/agent/events_v2_mapper.rs` | **删除** |
| `peri-agent/src/agent/events_v2_mapper_test.rs` | **删除** |
| `peri-agent/src/agent/mod.rs` | 移除 pub mod |
| `peri-agent/src/agent/subagent_event_forwarder.rs` | 重构转发逻辑 |
| `peri-agent/src/agent/subagent_event_forwarder_test.rs` | 删除 |
| `peri-agent/src/lib.rs` | prelude 清理 |
| `peri-acp/src/event/mod.rs` | re-export 路径 |
| `peri-acp/src/event/forwarder.rs` | import 路径 |
| `peri-tui/src/kit/v2_bridge.rs` | import 路径 |

## 验证标准

- [ ] `cargo build --workspace` 编译通过
- [ ] `cargo test --workspace` 全过
- [ ] SubAgent 流式事件在 TUI 正常显示
- [ ] `events_v2_mapper.rs` 文件不再存在
