# 每个 batch 的第一个工具调用始终显示为 Running 状态，不闭合

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-20

## 问题描述

每次对话中，每个 batch 的第一个工具调用始终停留在 Running 状态——工具卡片显示 `● Tool (params)` + `Running (0s)`，不会过渡到绿色完成态。输出也不显示。

后续同一 batch 的其他工具调用正常显示完成态和输出。

## 症状详情

| 维度 | 现象 |
|------|------|
| 影响范围 | 所有工具（Bash/Read/Edit/Glob/Grep 等） |
| 影响位置 | 每个 batch 的第一个工具调用 |
| 视觉表现 | 指示器始终为白色 Running 态，不转绿色；无输出显示 |
| batch 内后续工具 | 正常 |
| 复现频率 | 必现（所有对话） |
| 引入时间 | 不确定（可能与 e5239171 和工具显示重构有关） |

### 日志示例

```
思考了 653 字符

● Shell (cd ... && git log --oneline -15)
  ⎿ Running (0s)                          ← 卡住，不闭合

思考了 302 字符

● Shell (cd ... && git show --stat e5239171)
  ⎿ commit e5239171...                    ← 正常显示输出
```

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 发送任意 prompt（如 `/issue-create test`）
  2. 观察第一批工具调用
  3. 第一个工具卡片始终 Running，后续正常
- **环境**：macOS 26.5.1, perihelion 终端模式

## 涉及文件

- `peri-tui/src/kit/acp_events.rs` —— ToolStarted/ToolEnded 事件处理
- `peri-tui/src/kit/acp_types.rs` —— ToolCardAccumulator / CurrentTurn / build_view_models
- `peri-tui/src/kit/message_area/render.rs` —— 工具卡片渲染（e5239171 修改）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-20 | — | Open | agent | 创建 |
| 2026-07-20 | Open | Fixed | agent | 修复 `else if data.is_running` 缺失导致的折叠逻辑回归 |

## 修复记录

### 根因

commit e5239171 重构 `render.rs` 的折叠判断逻辑时，错误地移除了 `else if data.is_running { false }` 检查：

```diff
 let collapsed = if data.is_error {
     false
+} else if data.is_running {
+    false  // ← 此行被误删
 } else if AUTO_EXPAND.contains(..) {
     false
```

导致所有在 `COLLAPSED_BY_DEFAULT` 中的工具（Bash/Read/Glob/Grep/Edit/Write）在运行状态下也被折叠，而非展开。

### 修复

恢复 `else if data.is_running { false }` 检查（`render.rs:462-463`）。

### 回归测试

新增 `test_first_tool_in_batch_is_running_false_after_end`（`acp_types.rs`），模拟 reasoning → tool1 → more reasoning → tool2 → tool1 done → tool2 done 的典型 batch 流程，验证第一个工具完成后的 `is_running` 为 false。测试通过。

### 涉及文件

- `peri-tui/src/kit/message_area/render.rs` —— 恢复 `is_running` 检查
- `peri-tui/src/kit/acp_types.rs` —— 新增回归测试

### 修改时间

2026-07-20

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
