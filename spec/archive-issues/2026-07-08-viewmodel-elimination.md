# 消灭 ViewModel 共享类型体系，自定义事件精简化


> 归档于 2026-07-20，原路径 spec/issues/2026-07-08-viewmodel-elimination.md
**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-08

---

## 问题描述

`peri-acp-types/src/view_model.rs` 定义了 8 种 ViewModel 变体，通过 `peri-acp-types` crate 在 ACP 层和 TUI 层之间共享。经分析，这 8 种类型全部可以删除——TUI 端从标准 ACP `session/update` 事件直接派生渲染结构。同时 `peri/unstable-event` 自定义事件可进一步精简为仅保留 ACP 真正没有的 2 个。

## 症状详情

| 概念 | 现状 | 问题 |
|------|------|------|
| 8 种 ViewModel | `peri-acp-types` 共享 crate + `acp_bridge` 累积 → `VIEW_MODELS` atom | 多一层类型转换，ACP 层不应关心 TUI 渲染细节 |
| `turn-done` / `turn-interrupted` | router.rs 自定义事件 | ACP 标准 `session/prompt` 响应已有 `StopReason`（EndTurn/Cancelled/MaxTurnRequests），已有代码使用 |
| `subagent-started` / `subagent-stopped` | router.rs 自定义事件 | SubAgent 本质是 Agent tool 调用，可用 ToolCall 生命周期 + `_meta` 表达 |
| `budget-warning` | router.rs 自定义事件 | ACP `UsageUpdate` 只报用量不表达阈值警告语义，保留 |
| `rewind-preview` | router.rs 自定义事件 | ACP 无此概念，保留 |
| `system-note` / `collapsed-group` / `divider` | ViewModel 变体 | 纯 TUI 派生逻辑，不应发送 |

## 期望改造方向

1. **删除 `peri-acp-types/src/view_model.rs`**：8 种 ViewModel 全部移除
2. **`peri-tui` 不再依赖 `peri-acp-types`**：移除 Cargo.toml 依赖
3. **`turn-done` / `turn-interrupted`**：改用 ACP 标准 `session/prompt` 响应 `StopReason`（已有代码支持，删除 router.rs 中的事件定义）
4. **`subagent-started` / `subagent-stopped`**：改用 Agent ToolCall 生命周期 + `SessionNotification._meta` 表达
5. **`system-note` / `collapsed-group` / `divider`**：纯 TUI 端派生，不再作为事件发送
6. **router.rs 只保留 2 个事件**：`budget-warning` + `rewind-preview`

## 涉及文件

| 文件 | 改动类型 |
|------|----------|
| `peri-acp-types/src/view_model.rs` | 删除整个文件 |
| `peri-acp-types/src/event_data.rs` | 删除 TurnInterrupted 等结构体 |
| `peri-acp/src/event/router.rs` | 删除 turn-done/turn-interrupted/subagent-started/subagent-stopped 分支，仅保留 budget-warning + rewind-preview |
| `peri-acp/src/event/mod.rs` | 移除 router 相关转发 |
| `peri-acp/src/session/event_sink.rs` | 移除 peri/unstable-event 通知发送中的对应事件 |
| `peri-tui/src/kit/acp_types.rs` | 删除 AcpEventData 对应变体；重写 CurrentTurn 为 TUI 纯内部类型 |
| `peri-tui/src/kit/acp_events.rs` | BridgeState 改用 TUI 内部类型；dispatch_and_notify 适配 |
| `peri-tui/src/kit/acp_notifier.rs` | 移除对应 unstable-event 解码；增强 _meta 解析 |
| `peri-tui/src/kit/render_bridge.rs` | 从新 TUI 内部状态读取 |
| `peri-tui/src/kit/view_render.rs` | 渲染函数适配新内部类型 |
| `peri-tui/src/kit/atoms.rs` | VIEW_MODELS atom 替换为 TUI 内部渲染状态 atom |
| `peri-tui/Cargo.toml` | 移除 peri-acp-types 依赖 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-08 | — | Open | agent | 创建 |
| 2026-07-08 | Open | Partial | agent | Phase 1-5 完成：view_model.rs 删除、router.rs 精简为 2 分支、TurnDone→StopReason、SubAgent 双通道删除、9 个 unit test 追加。peri-tui 仍依赖 peri-acp-types（event_data 类型），后续迭代处理。 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
