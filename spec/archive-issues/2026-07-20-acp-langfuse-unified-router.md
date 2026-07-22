# 统一 Langfuse 事件路由：消除 v1/v2 双轨处理器

**状态**：Fixed
**优先级**：中
**类型**：架构改进
**创建日期**：2026-07-20
**来源**：`/tmp/architecture-review-peri-acp-20260720.html` 候选 #4（improve-codebase-architecture 审查 peri-acp）

## Problem Statement

`peri-acp` 中存在两套独立的 Langfuse 事件转发逻辑：

1. **v1 路径**：`session/executor_helpers.rs::forward_langfuse_event`——消费 `ExecutorEvent` 枚举
2. **v2 路径**：`event/forwarder.rs` 中的三个函数（`forward_langfuse_render_event`、`forward_langfuse_state_event`、`forward_langfuse_observe_event`）——消费 v2 的三类事件（Render/State/Observe）

两套转发逻辑处理相同类型的事件（LlmCallEnd、ToolStart/End、Compact、SubAgent 等），但使用不同的事件结构和路由入口。

**影响**：
- 新增事件类型（如新增 stage 事件）需要在两个 handler 中同步添加映射
- 两个 handler 的行为可能分化（如在 Compact span 的 parent_id 处理上已有细微差异）
- v1 ExecutorEvent 废弃后，v1 的 `forward_langfuse_event` 函数会成为死代码，但删除时需要理解两套逻辑的差异

## 建议方案

引入统一事件路由层：

```
ExecutorEvent (v1) ──┐
RenderEvent   (v2) ──┤
StateEvent    (v2) ──┼──→ UnifiedLangfuseEvent ──→ LangfuseTracer
ObserveEvent  (v2) ──┘
```

1. 定义 `UnifiedLangfuseEvent` 枚举（内部使用，不公开）
2. 分别为 v1 和 v2 事件实现 `Into<UnifiedLangfuseEvent>` 转换
3. 单一 `LangfuseBridge` 结构体消费 `UnifiedLangfuseEvent` 并调用 `LangfuseTracer` 方法

等 v1 ExecutorEvent 废弃后，只需删除 v1 的 `Into` 实现即可。

## 涉及文件

| 文件 | 改动 |
|------|------|
| `session/executor_helpers.rs` | 移除 `forward_langfuse_event`，改为发送 `UnifiedLangfuseEvent` |
| `event/forwarder.rs` | 移除三个 langfuse forward 函数，改为发送 `UnifiedLangfuseEvent` |
| `langfuse/tracer/mod.rs` | 可能需要调整 `on_*` 方法的参数以匹配统一事件 |
| 新增 `langfuse/bridge.rs` | 单一 `LangfuseBridge` + `UnifiedLangfuseEvent` 定义 |

## 收益

- **locality**：Langfuse 事件映射只在一处定义
- **leverage**：新增事件类型只需加一个 `UnifiedLangfuseEvent` 变体 + 在 v1/v2 的 Into 实现中映射
- v1 废弃后可干净删除，不留下理解负担

## 依赖关系

- 依赖 v1 ExecutorEvent 退休进度（`spec/issues/2026-07-18-v1-executor-event-retirement.md`）
- 建议在 v1 退休前完成——此时做统一路由最有价值（两套 handler 都还在活跃使用）

## 风险

- `UnifiedLangfuseEvent` 需要是 v1 和 v2 事件的超集，可能需要 `Option` 字段
- 如果 v1 和 v2 对同一语义事件携带不同粒度的数据（如 tool input/output 的截断），统一后需要选更完整的那套

## 修复记录

### 修复 #1（2026-07-20）
- **操作人**：agent
- **commit**：`48ebfed6 refactor(acp): 统一 Langfuse 事件路由，消除 v1/v2 双轨处理器`
- **修复内容**：引入统一事件路由层，合并 v1/v2 双轨 Langfuse 处理器
