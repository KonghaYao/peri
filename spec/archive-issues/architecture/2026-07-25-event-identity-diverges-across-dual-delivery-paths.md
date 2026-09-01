> 归档于 2026-08-11，原路径 spec/issues/2026-07-25-event-identity-diverges-across-dual-delivery-paths.md

# 同一 Agent 事件经双轨投递后身份与语义可能不一致

**状态**：Fixed
**优先级**：高
**类型**：重构
**创建日期**：2026-07-25
**来源**：2026-07-24 架构审查 A2；过程文档已删除，本归档 issue 保留结论

## 问题描述

同一 v2 Agent 事件目前会跨越 `RenderEvent / StateEvent / ObserveEvent`、`ExecutorEvent`、ACP 事件和 TUI 本地 DTO，并可同时走 ACP transport 与 TUI v2 直连路径。转换过程中部分 `message_id`、`source_agent_id` 等身份字段被替换为默认值或 `None`，两条路径还各自维护 mapper 与 suppress 规则。期望项目只有一个 canonical 事件语义来源，不同 transport 对同一事件保留相同身份并产生等价的下游输入。

## 现状

当前主要转换链包括：

1. v2 事件 → `ExecutorEvent`；
2. `ExecutorEvent` → ACP `SessionUpdate` 或 `peri/agent_event`；
3. ACP 事件 → TUI `AcpEventData`；
4. v2 事件 → TUI `AcpEventData` 的独立直连 mapper。

架构审查观察到：

- `TextChunk`、`ToolStarted`、`ToolEnded` 的兼容映射会使用默认 `message_id`，并把 `source_agent_id` 置为 `None`；
- v2 原始事件已携带 `turn_id` 和 `agent_id`，但这些身份没有通过统一 envelope 跨 transport 保留；
- TUI 启动时启用双轨运行，同一事件是否转发或抑制依赖分散的 match 分支；
- variant coverage 存在 wildcard，新事件可能静默落入“不生成下游更新”的分支。

这会增加以下风险：

- TUI 与 IDE 对新增事件表现不一致；
- 同一事件被重复投递，产生重复工具卡或 SystemNote；
- 并发消息无法按稳定 `message_id` 归组；
- SubAgent 路由丢失 `source_agent_id` 后依赖旁路 metadata；
- 删除兼容层时缺少可证明的 transport 等价基线。

## 期望改进方向

建立一个 canonical event envelope 或等价的 typed contract，统一承载：

- `session_id` / session epoch；
- `turn_id`；
- `agent_id` / `source_agent_id`；
- 可选但语义明确的 `message_id`；
- 单调 `sequence`；
- delivery class；
- typed payload。

ACP stdio 与 TUI in-process transport 应消费同一个 canonical 语义模型。是否最终保留两种 transport 可以另行决定，但不应继续维护两套独立事件定义、身份补丁和 suppression 知识。

## 与现有 Issue 的边界

- `spec/issues/2026-07-18-events-v2-mapper-removal.md` 关注删除 `events_v2_mapper.rs` 及其桥接调用方。
- `spec/issues/2026-07-18-executor-event-retirement.md` 关注彻底退役 v1 `ExecutorEvent` 与 `event_tx`。
- langfuse v2 迁移计划（2026-08-11 已随归档清理删除）是上述退役工作的既有前置依赖。
- **本 issue 不重复规定删除步骤**，而是先定义完整身份、事件目录和 transport 等价契约，为这些退役 issue 提供可验证的迁移边界。

## 验收标准

- [ ] 所有跨 transport 事件都通过一个 canonical typed contract，身份字段不再由各 mapper 临时补齐。
- [ ] 必需身份不存在时返回结构化错误或拒绝构造，不使用 `Default::default()`、空字符串或 `None` 伪装缺失值。
- [ ] 每个事件变体显式声明 destination、capability gating、identity requirement 与 delivery class。
- [ ] mapper 对 canonical 事件使用穷尽匹配；新增变体无法静默落入 wildcard 丢弃分支。
- [ ] 同一组 fixture 分别经过 stdio ACP 与 in-process transport 后，得到语义等价的 TUI reducer 输入。
- [ ] 合同测试覆盖 Text、Thinking、Tool、Turn、Compact、SubAgent、Error 和状态快照事件。
- [ ] 双轨同时启用期间，同一个 `(session, turn, sequence)` 不会在 TUI 产生重复业务效果。
- [ ] 更新三个既有退役 issue 的依赖或完成条件，使其引用本 issue 建立的 contract。

## 非目标

- 不要求在本 issue 中直接删除 ACP 协议支持。
- 不预先锁定 TUI 最终只走 stdio ACP，或必须保留 in-process transport。
- 不承担 EventBus 队列饱和时的可靠性交付改造。

## 关联 Issue

- `spec/issues/2026-07-25-control-boundary-events-can-be-dropped.md` —— EventBus 交付可靠性。
- `spec/issues/2026-07-25-stale-v2-events-bypass-session-filter.md` —— canonical identity 中 session epoch 的直接使用场景。

## 涉及文件

- `peri-agent/src/agent/events_v2.rs` —— v2 事件及未来 canonical contract 的候选归属。
- `peri-agent/src/agent/events_v2_mapper.rs` —— 当前 v2 → v1 身份衰减路径。
- `peri-acp/src/event/mapper.rs` —— `ExecutorEvent` → ACP 映射。
- `peri-acp/src/session/event_sink.rs` —— ACP custom event、caps 与 metadata 路由。
- `peri-tui/src/kit/acp_notifier.rs` —— ACP → TUI DTO 映射。
- `peri-tui/src/kit/v2_bridge.rs` —— v2 → TUI DTO 的第二套 mapper。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-25 | — | Open | agent | 根据架构审查 A2 创建，并关联既有事件退役 issue |
| 2026-08-11 | Open | Fixed | agent | 归档：3.0-M 事件三通道收敛单链路（UnstampedEvent/EventEnvelope/身份契约），v2 mapper 退役，修复记录见正文 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
