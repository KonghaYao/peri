# 控制边界事件在通道饱和时可能被丢弃

**状态**：Open
**优先级**：高
**类型**：Bug
**创建日期**：2026-07-25
**来源**：`docs/architecture-review-2026-07-24.md` A1

## 问题描述

`EventBus` 将 `RenderEvent` 和 `StateEvent` 描述为 critical 事件，但当前发送接口在有界通道已满时使用 `try_send` 并直接放弃事件。流式文本与 `ToolStarted`、`ToolEnded`、`TurnCompleted`、`TurnSuspended` 等控制边界共享该交付策略，因此高负载下不仅可能少显示 token，也可能失去工具配对和 turn 状态边界。期望不同事件类别具有明确、可验证的交付语义，控制边界在队列饱和时仍能按序到达。

> 架构审查基于静态代码，尚未运行构建、测试或 E2E；运行时影响需由本 issue 的压力回归测试确认。

## 症状详情

| 事件类别 | 当前可观察行为 | 潜在影响 |
|----------|----------------|----------|
| `TextChunk` / `ThinkingChunk` | 通道满时可被丢弃 | 流式内容不完整 |
| `ToolStarted` / `ToolEnded` | 与流式事件采用相同 `try_send` 语义 | 工具卡无法配对或结束 |
| `TurnCompleted` | render queue 满时可被丢弃 | 缺失迭代提交边界，partial 内容可能跨迭代漂移 |
| `TurnSuspended` | state queue 满时可被丢弃 | 下游仍认为 turn 正在运行 |
| Observe 事件 | broadcast consumer lag 时可跳过事件 | 遥测不完整；若被状态消费者使用会造成语义错误 |

现有测试明确覆盖并接受“通道满后丢弃事件”，但尚未区分可丢弃的流数据与不可丢的控制事件。

## 复现条件

- **复现频率**：队列达到容量后，`try_send` 失败是确定行为；用户侧异常出现频率取决于事件生产速度和消费者延迟。
- **触发步骤**：
  1. 用容量为 1 的 render/state 通道构造 `EventBus`。
  2. 在不消费或慢消费的情况下填满通道。
  3. 依次发送流式 chunk、工具开始/结束、turn 完成或挂起事件。
  4. 恢复消费并检查控制事件是否到达、是否保持顺序与配对。
- **环境**：审查基线 `ce682d53`（`main` / `agent-v2.8.6`）；适用于 TUI、stdio、SubAgent 等消费 EventBus 的路径。

## 目标状态

- 控制边界、可合并状态、可损流数据和纯观测事件的交付契约在类型或接口层明确区分。
- `TurnCompleted`、`TurnSuspended`、Tool 开始/结束等状态机边界不可因流式队列饱和而静默消失。
- 流式 chunk 可以批量、合并或降采样，但必须在后续控制边界之前完成必要的 flush。
- 通道关闭、饱和、consumer lag 等结果可观测，不再只返回 `()` 并记录 warning。

## 验收标准

- [ ] 为所有 `RenderEvent`、`StateEvent`、`ObserveEvent` 变体声明交付类别，并有穷尽性测试防止新增变体漏分类。
- [ ] 容量为 1 的饱和测试证明 `ToolStarted → ToolEnded` 与 turn 控制边界均能到达且顺序稳定。
- [ ] 流式数据被合并或丢弃时，下一控制边界前不会遗留未提交的 chunk。
- [ ] 可合并状态在压力下至少保留当前 turn 的最新有效值。
- [ ] Observe consumer lag 不会影响 render/state 状态机；所有 drop/lag 都有按事件类别统计的可观测信号。
- [ ] 覆盖主 Agent 与 SubAgent 转发路径的回归测试。
- [ ] `cargo test -p peri-agent --lib` 和相关 `peri-acp` 测试通过。

## 非目标

- 不要求把所有事件都改为无界队列。
- 不要求每个流式 token 都无损投递。
- 不在本 issue 中统一 ACP/TUI 双轨事件协议；该工作由关联 issue 处理。

## 关联 Issue

- `spec/issues/2026-07-25-event-identity-diverges-across-dual-delivery-paths.md` —— 统一事件身份与双轨语义；本 issue 只负责交付可靠性。

## 涉及文件

- `peri-agent/src/agent/events_v2.rs` —— EventBus 通道、容量与发送语义。
- `peri-agent/src/agent/events_v2_test.rs` —— 当前满队列行为及新增饱和回归测试。
- `peri-acp/src/event/forwarder.rs` —— EventBus 下游转发边界。
- `peri-agent/src/agent/subagent_event_forwarder.rs` —— SubAgent 事件转发路径。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-25 | — | Open | agent | 根据架构审查 A1 创建 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
