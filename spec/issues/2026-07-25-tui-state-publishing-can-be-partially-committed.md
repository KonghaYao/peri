# TUI 事件处理可能只提交部分派生状态

**状态**：Open
**优先级**：中
**类型**：技术债
**创建日期**：2026-07-25
**来源**：`docs/architecture-review-2026-07-24.md` A5
**最后核查**：2026-08-11

## 最新情况（2026-08-11）

handler 仍直接调用 `push_view_models`/`push_acp_state`（`kit/acp_events/` 下 8 个文件），v2 路径还存在绕过 reducer 直写 atom；统一 reducer/单点一次性提交派生状态未落地，部分提交问题仍在。

**状态**：Open（保持）

## 问题描述

TUI 的 `BridgeState` 由多个事件 handler 修改，但 handler 需要自行决定是否调用 `push_view_models`、`push_acp_state`，部分 notifier 和 v2 bridge 路径还会绕过 reducer 直接写 atom。一次事件处理应发布哪些派生状态目前不是统一的模块后置条件，因此新增或修改 handler 时可能漏提交、重复提交，或让组件短暂观察到不一致状态。期望 reducer 只描述状态变化和 UI effects，由单一 dispatcher 统一、一次性提交派生状态。

## 现状

架构审查对 `acp_events` 的静态统计约为：

- 25 处手动 `push_view_models(state)`；
- 17 处手动 `push_acp_state(state)`；
- 调用分散在 Turn、Tool、Streaming、Compact、SubAgent、System 等 handler。

已观察到的不同模式：

- `TurnDone` 和 `TurnCommitted` 分别手动 flush 并 push 两类 atom；
- `inject_system_note` 已封装“状态注入 + 两次 push”，但只覆盖 SystemNote；
- streaming handler 按分支采用不同 push 策略；
- `v2_bridge` 的 `StateSnapshot` 直接写 `CONTEXT_USAGE` 和 heartbeat；
- notifier 对 usage、命令和 cache warning 等事件直接产生 atom 副作用。

同一事件因此可能只改 `BridgeState`、只 push 一个 atom、push 两个 atom、多次 push，或完全绕过 `BridgeState`。

## 期望改进方向

建立统一 reduce/commit 边界。reducer 返回声明式 dirty flags 与 `UiEffect`（或语义等价结构），dispatcher 在 handler 完成后统一发布 `VIEW_MODELS`、session state 和其他 atom。具体 API 不强制，但事件处理与 atom 提交必须成为一个模块级不变量。

优先覆盖历史高回归区域：SystemNote、Turn lifecycle、Compact、SubAgent、session reset 和 streaming 边界。

## 验收标准

- [ ] 所有 `AcpEventData` 业务事件通过一个统一 dispatcher 完成 reduce 与 commit。
- [ ] handler 不再直接调用 `push_view_models` / `push_acp_state`；若存在例外，必须集中声明并说明为何不属于 reducer state。
- [ ] notifier 与 `v2_bridge` 的业务 atom 写入改为声明式 `UiEffect` 或进入同一 commit boundary。
- [ ] 单个事件最多触发一次版本化状态提交，`VIEW_MODELS.generation` 与 session snapshot 来源一致。
- [ ] TurnDone、TurnCommitted、SystemNote、Compact、SubAgent、StateSnapshot、session reset 均有精确的 reducer/effect contract test。
- [ ] 测试证明“内部 state 已变化但 UI atom 未更新”和“同一事件重复 wake/render”不会出现。
- [ ] 保持 VIEW_MODELS 为消息区唯一数据源，组件 render body 不写 atom。
- [ ] 现有 TUI 行为与事件时序不变，相关 crate 测试通过。

## 非目标

- 不重写 message-area 渲染器。
- 不调整现有 UI 文案、样式或快捷键。
- 不把所有全局 atom 无条件合并为一个超大 atom；只要求提交边界一致。

## 关联 Issue

- `spec/issues/2026-07-25-event-identity-diverges-across-dual-delivery-paths.md` —— 统一 reducer 输入语义。
- `spec/issues/2026-07-25-stale-v2-events-bypass-session-filter.md` —— stale filter 必须发生在统一 commit 前。

## 涉及文件

- `peri-tui/src/kit/acp_events/mod.rs` —— `BridgeState`、dispatch 与当前 push helpers。
- `peri-tui/src/kit/acp_events/` —— 分散的事件 handlers。
- `peri-tui/src/kit/acp_bridge.rs` —— reducer 调用、reset 与状态提交入口。
- `peri-tui/src/kit/acp_notifier.rs` —— ACP 通知及直接 atom 副作用。
- `peri-tui/src/kit/v2_bridge.rs` —— v2 直连映射及直接 atom 副作用。
- `peri-tui/src/kit/atoms.rs` —— 需要统一提交的全局 atoms。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-25 | — | Open | agent | 根据架构审查 A5 创建 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
