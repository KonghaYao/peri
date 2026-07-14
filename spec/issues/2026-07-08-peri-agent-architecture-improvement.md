# peri-agent 架构设计改进

**状态**：Partial
**优先级**：高
**创建日期**：2026-07-08
**类型**：重构

## 问题描述

per-agent 在 2026-07-08 的全面架构审查中得分 78/100。五阶段 pipeline + 三层 EventBus + Queue 类型驱动消费构成了高质量的架构骨架，但 v1→v2 演进留下了几项核心架构债务，需在后续版本中清理。

## 症状详情

### 子项 1：AgentState ⇄ MessageTranscript 双轨存储（P0，2-3 周）

v1 middleware 通过 `AgentState.messages: Vec<BaseMessage>` 操作消息，v2 stages 通过 `MessageTranscript.entries` 管理消息。`middleware_runner.rs:33-63` 的 `snapshot_to_agent_state` / `restore_from_agent_state` 在**每次 middleware hook 调用时**都需 clone 整份 `visible_messages` 在两个系统间转换。在长对话（500+ 条消息）场景，19 个 middleware × 每轮 3-5 次 hook × O(n) clone = 显著性能开销。

期望：将 `MessageTranscript` 作为唯一真相源，middleware 通过扩展 trait 直接操作它，消除 snapshot/restore clone 开销。

### 子项 2：中间件执行顺序缺乏声明性约束（P1，3-5 天）

19 个 middleware 的固定顺序仅记录在 CLAUDE.md 文档中（`chain.rs` 按 `add()` 顺序执行），代码层面无任何机制表达"CompactMiddleware 必须在 GoalSteering 之前"这类约束。第三方 middleware 如果在错误位置 add，prompt 注入可能在 compact 之前执行，破坏 prompt cache。

期望：引入 `fn priority() -> i32` → `MiddlewareChain::add()` 自动按优先级排序。

### 子项 3：StageContext 字段过度膨胀（P2，2-3 天）

`stages/mod.rs:70-135` 的 `StageContext` 包含 21 个字段（10 个 `Option`），其中 `compact_pre_hook`/`compact_post_hook`（ACP 层注入回调）、`idle_inbox`/`idle_should_wait`（transport-aware 唤醒策略）、`recall_buffer`（跨 hook 累加器）都是横切关注点，散落在结构体中而非聚合为独立子结构或 trait。

期望：将相关关注点聚合为子结构（如 `IdlePolicy`、`CompactHooks`、`RecallBuffer`），降低 `StageContext` 的认知负荷。

### 子项 4：AgentGroup 仍使用 v1 ExecutorEvent（P2，1-2 周）

`group/mod.rs:68` 的 `AgentGroup::event_tx` 使用 `UnboundedSender<ExecutorEvent>`，而 v2 核心使用 `EventBus` 三层系统。SubAgent 的 `ExecutorEvent` 需额外映射才能在 v2 的 `RenderEvent`/`ObserveEvent` 中消费。

期望：将 AgentGroup 迁移到 v2 EventBus。

### 子项 5：compact 模块目录残留（P3，1 天）

`agent/compact/mod.rs` 仅导出 `CompactConfig` 和一个常量 `CONTINUATION_HINT`，实际实现全部在 `agent/compact_v2.rs`。v1 compact 目录物理删除后外壳还在。

期望：删除 `compact/mod.rs` 空壳，把 `CompactConfig` 和 `CONTINUATION_HINT` 直接暴露在 `compact_v2.rs`。

### 子项 6：`Reasoning` 结构体职责过载（P3，1 天）

`react.rs:131-146` 同时包含 LLM 输出 (`thought`/`tool_calls`/`final_answer`)、追踪元数据 (`model`/`usage`/`streamed`/`stop_reason`) 和桥接数据 (`source_message: Option<BaseMessage>`)。

期望：拆分为 `ReasoningOutput`（业务数据）和 `ReasoningMetadata`（追踪信息）。

## 涉及文件

- `peri-agent/src/agent/stages/mod.rs:70-135` —— StageContext 定义（21 个字段）
- `peri-agent/src/agent/stages/middleware_runner.rs:33-63` —— snapshot/restore 桥接
- `peri-agent/src/agent/state.rs` —— AgentState 定义
- `peri-agent/src/session/transcript.rs` —— MessageTranscript 定义
- `peri-agent/src/middleware/chain.rs` —— MiddlewareChain 无排序约束
- `peri-agent/src/group/mod.rs:68` —— AgentGroup v1 ExecutorEvent 残留
- `peri-agent/src/agent/compact/mod.rs` —— 空壳目录
- `peri-agent/src/agent/react.rs:131-146` —— Reasoning 结构体

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-08 | Open | Partial | agent | P0 #1（双轨统一）已完成；P1/P2 子项待处理 |

## 修复记录

### 修复 #1（2026-07-08）

- **操作人**：agent
- **用户原意**：消除 AgentState/MessageTranscript 双轨存储的 per-hook clone 开销
- **修复内容**：新建 `AgentContext`（薄封装 StageContext），重写 `middleware_runner` 消除 snapshot/restore 桥接。`add_message()` 直接双写 transcript + cache。MiddlewareState trait 零变更，AgentState 保留为测试兼容层。
- **涉及文件**：`agent_context.rs`（+391 行）、`middleware_runner.rs`（-152 行）、`compact.rs`/`act.rs`（轻量调整）
- **涉及 commit**：待提交
- **验证状态**：已验证（611/611 测试通过）
