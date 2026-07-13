# 同步 Agent 子工具调用卡片完全不显示

**状态**：Open
**优先级**：高
**创建日期**：2026-07-13

## 问题描述

主 agent 调用 Agent 工具（同步 SubAgent）时，子 agent 内部执行的工具调用（Grep/Read/Bash 等）卡片**完全不渲染**。Agent 卡片容器（SubAgentGroup）仍然可见，但卡片内部的子工具调用卡片全部消失——看起来像一个空壳。

这是相对于 issue #2026-07-12 的**回归**：此前（7/12）子工具调用卡片虽然位置错误（跑到历史消息区而非嵌套在 Agent 卡片内），但至少还能看到；现在彻底不渲染了。

## 症状详情

| 维度 | 当前行为 | 7/12 时的行为 | 期望行为 |
|------|----------|--------------|----------|
| 子工具卡片可见性 | ❌ 完全不显示 | ✅ 可见（位置错） | ✅ 可见且嵌套在 Agent 卡片内 |
| Agent 卡片容器 | ✅ 正常显示（含 header/status） | ✅ 正常显示 | ✅ 正常显示 |
| Agent 卡片内容 | 空壳（无子工具卡片） | 无（子工具在外部） | 子工具卡片嵌套在内 |
| 影响范围 | 所有同步 Agent 必现 | 所有同步 Agent 必现 | — |
| 触发场景 | 同步 SubAgent（非 bg） | 同步 SubAgent | — |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 Peri TUI
  2. 发起会话，让主 agent 派发一个同步 SubAgent（如 explorer、coder 等）
  3. SubAgent 在内部执行若干工具调用
  4. 观察 Agent 卡片——容器正常显示，但内部没有任何工具调用卡片
- **环境**：TUI 前端，所有同步 Agent 工具调用均出现

## 涉及文件

- `peri-tui/src/kit/message_area/render.rs` —— `render_subagent_group_lines`（L466），递归渲染 Agent 卡片内的 children（子工具调用等）
- `peri-tui/src/kit/tui_render_unit.rs` —— `TuiSubAgentGroup` 定义，`view_models: im::Vector<TuiRenderUnit>` 承载子工具调用的容器
- `peri-tui/src/kit/acp_events.rs` —— Agent 工具事件 → `VIEW_MODELS` 的写入路径，决定子工具调用卡片是否被推入 `TuiSubAgentGroup.view_models`
- `peri-tui/src/kit/acp_types.rs` —— ACP 类型到 `TuiRenderUnit` 的转换逻辑

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-13 | — | Open | agent | 创建。关联 issue #2026-07-12（同模块，相同功能区域但症状不同） |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
