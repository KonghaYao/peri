# 同一 turn 触发两个 Agent 时，子工具未嵌套在对应 Agent 下方

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-21

## 问题描述

主 Agent 在一次 turn 中同时调用两个 Agent 工具（均为 `explorer` 类型）后，TUI 消息区渲染的布局出现分组错误：两个 Agent 卡片（SubAgentGroup）先集中显示，然后两个 Agent 各自的 collapsed tools 段落再集中显示在所有 Agent 卡片之后。正确行为应该是每个 Agent 卡片内部嵌套其子工具，即 Agent A → Tools A → Agent B → Tools B 的交替分组布局。

## 症状详情

**实际渲染布局**（错误）：
```
● Agent (subagent_type: explorer)          ← Agent A 卡片
  ⎿ child_thread_id: ...
  ⎿ 报告已保存至 .peri/plans/...

● Agent (subagent_type: explorer)          ← Agent B 卡片
  ⎿ child_thread_id: ...
  ⎿ 完整报告已保存至 .peri/plans/...

  ▶ 25 collapsed tools                     ← Agent A 的子工具，出现在这里
  ● Glob (pattern: ...) — 1 matches
  ...

  ▶ 69 collapsed tools                     ← Agent B 的子工具，出现在这里
  ● Read (...)
  ...
```

**期望渲染布局**（正确）：
```
● Agent (subagent_type: explorer)          ← Agent A 卡片
  ⎿ child_thread_id: ...
  ⎿ 报告已保存至 .peri/plans/...
  ▶ 25 collapsed tools                     ← Agent A 的子工具，嵌套在内部
  ● Glob (pattern: ...) — 1 matches
  ...

● Agent (subagent_type: explorer)          ← Agent B 卡片
  ⎿ child_thread_id: ...
  ⎿ 完整报告已保存至 .peri/plans/...
  ▶ 69 collapsed tools                     ← Agent B 的子工具，嵌套在内部
  ● Read (...)
  ...
```

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 向主 Agent 发送 prompt，使其在一次 turn 中同时调用两个 Agent 工具（两个 subagent）
  2. 等待两个 Agent 执行完成
  3. 观察消息区的渲染布局——子工具（collapsed tools）未嵌套在对应 Agent 卡片内部
- **环境**：macOS 26.5.1，Peri TUI

## 涉及文件

- `peri-tui/src/kit/message_area/render.rs` —— SubAgentGroup 及其子工具（children）的渲染逻辑，决定 collapsed tools 相对 Agent 卡片的位置
- `peri-tui/src/kit/acp_events.rs` —— `push_view_models` 将事件转换为 VIEW_MODELS 原子写入，决定多个 Agent 完成时 view models 的写入顺序和分组结构
- `peri-tui/src/kit/acp_bridge.rs` —— 桥接 ACP 事件到 Atom 状态，可能影响多个 Agent 事件到达时序下的视图构建顺序

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-21 | — | Open | agent | 创建 |
| 2026-07-21 | Open | Fixed | agent | 修复：start_subagent() 前向扫描未 claim Agent ToolCard，在其后插入 SubAgent 段 |

## 修复记录

### 修复（2026-07-21）

- **操作人**：agent
- **修复内容**：`peri-tui/src/kit/acp_types.rs` (+29/-2 行)
  - `start_subagent()`：前向扫描找第一个 `tool_name == "Agent"` 且 `claimed_by_subagent == false` 的 ToolCard，在其后**插入** SubAgent 段（`Vec::insert`），替代旧有的末尾 `push`
  - `ToolCardAccumulator`：新增 `claimed_by_subagent: bool` 字段（默认 `false`），防止多 Agent 时 SubAgent 段错配
  - Fallback：无未 claim 的 Agent ToolCard 时保持旧有 `push` 行为
- **验证状态**：✅ PASS
  - Build: `cargo build -p peri-tui` PASS
  - Tests: `cargo test -p peri-tui --lib -- acp_types acp_events` 58 passed
  - Clippy: `cargo clippy -p peri-tui -- -D warnings` 0 warnings
  - Diff: `1 file changed, 29 insertions(+), 2 deletions(-)`
