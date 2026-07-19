# SubAgent 工具调用卡片回归不显示（Agent 卡片容器可见但内部为空壳）

**状态**：Open
**优先级**：高
**创建日期**：2026-07-18

## 问题描述

主 agent 调用 Agent 工具（同步 SubAgent）时，Agent 卡片容器（SubAgentGroup）正常显示在消息区，但卡片内部的子工具调用卡片（Grep、Read、Bash 等）完全不渲染——看起来像一个空壳。用户只能看到 Agent 卡片的外壳（名称/状态），看不到 SubAgent 执行了哪些工具。

这是 issue `spec/archive-issues/2026-07-13-sync-agent-tool-cards-not-showing.md`（2026-07-13，状态 Fixed）的回归。与旧 issue 的关键差异：
- **旧 issue**：第二轮及之后的 Agent 调用才出现（`or_insert_with` 保留已关闭的 `event_tx`），第一轮正常
- **本次**：第一轮就出现，每次 Agent 调用都是空壳

## 症状详情

| 维度 | 当前行为 | 期望行为 |
|------|----------|----------|
| 子工具卡片可见性 | ❌ 完全不显示 | ✅ 可见且嵌套在 Agent 卡片内 |
| Agent 卡片容器 | ✅ 正常显示（含名称/状态） | ✅ 正常显示 |
| Agent 卡片内容 | 空壳（无子工具卡片） | 子工具卡片嵌套在内 |
| 影响范围 | 所有 SubAgent 类型（explorer / coder / 通用等） | — |
| 触发轮次 | 第一轮就出现 | — |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 Peri TUI
  2. 发起会话，让主 agent 派发一个同步 SubAgent（如 explorer、coder 等）
  3. SubAgent 在内部执行若干工具调用
  4. 观察 Agent 卡片——容器正常显示，但内部没有任何工具调用卡片，是空壳
- **环境**：TUI 前端，所有同步 Agent 工具调用均出现

## 涉及文件

- `peri-acp/src/agent/builder_v2.rs` —— 旧修复点（`or_insert_with` → `insert`），SubAgentTool 实例注册逻辑
- `peri-tui/src/kit/acp_events.rs` —— Agent 工具事件 → `VIEW_MODELS` 的写入路径，决定子工具调用卡片是否被推入 `TuiSubAgentGroup.view_models`
- `peri-tui/src/kit/acp_types.rs` —— ACP 类型到 `TuiRenderUnit` 的转换逻辑，SubAgentAccumulator 的管理
- `peri-tui/src/kit/message_area/render.rs` —— `render_subagent_group_lines`，递归渲染 Agent 卡片内的 children

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-18 | — | Open | agent | 创建。关联 issue `spec/archive-issues/2026-07-13-sync-agent-tool-cards-not-showing.md`（同症状，不同触发条件） |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
