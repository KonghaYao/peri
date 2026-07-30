# 自动 micro compact 后 Agent 工具上下文丢失并持续失忆

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-29
**设计文档**：[Micro Compact 字段级压缩设计](./2026-07-29-micro-compact-field-level-design.md)

## 问题描述

在 `peri-tui` 长会话自动触发 micro compact 后，随后调用 Agent 工具会稳定出现工具上下文丢失。显示层只剩 `_compact_note: tool input compacted`，同时工具执行报错 `missing required parameter prompt`；后续会话还表现出持续失忆。期望 compact 不导致必填工具参数缺失、Agent 调用失败或后续上下文无法延续；具体修复语义暂不预设。

## 症状详情

观察到以下输出：

```text
● Agent (_compact_note: tool input compacted)
  ⎿ Tool execution failed: Agent - Error: missing required parameter prompt
```

已确认的影响：

- 显示层中的 Agent tool input 被 `_compact_note` 替代，原始上下文不可见。
- Agent 工具因缺少必填 `prompt` 参数而执行失败。
- micro compact 后会话出现持续失忆，当前明确观察到工具上下文丢失；其他失忆细节待补充。

## 复现条件

- **复现频率**：稳定复现
- **触发步骤**：
  1. 在 `peri-tui` 中持续交互，直至自动触发 micro compact。
  2. 在 compact 后调用 Agent 工具。
  3. 观察 Agent tool input 显示为 `_compact_note: tool input compacted`。
  4. 观察工具因缺少 `prompt` 参数而失败，后续会话无法持续保留相关工具上下文。
- **环境**：`peri-tui`；模型及其他配置待补充。

## 涉及文件

- `peri-agent/src/agent/compact_v2/` —— micro compact 的稳定代码入口；具体涉及文件待修复阶段定位。
- `peri-tui/src/kit/acp_events/` —— TUI 事件显示链路的稳定入口；具体涉及文件待修复阶段定位。

## 定位记录

### 2026-07-29：故障归属为 Agent compact 层

结论：问题产生于 `peri-agent` 的 micro compact 投影；`peri-tui` 只显示服务端已发出的工具调用参数，不会改写真实执行参数。

证据链：

1. `peri-agent/src/agent/compact_v2/projection.rs:603-618` 将被压缩的历史 tool input 整体替换为 `{"_compact_note":"tool input compacted"}`。
2. `peri-agent/src/agent/stages/reason.rs:70-152` 把该投影视图作为后续 LLM 请求上下文。原始 transcript 消息未被直接覆写，但持久化 projection directive 会让后续 Reason 阶段持续使用压缩视图。
3. 历史 tool call 不会被 Act 直接重新执行；风险路径是模型根据压缩后的历史生成一个新的 Agent tool call，并沿用缺少 `prompt` 的占位参数。
4. `peri-middlewares/src/subagent/tool/define.rs:350-358` 在真实执行 Agent 工具时校验 `prompt`，缺失即返回观察到的错误。
5. `peri-acp/src/event/mapper.rs:80-95` 只把服务端 `ToolStart.input` 透传为 ACP `rawInput`；`peri-tui/src/kit/acp_notifier.rs:429-458` 与 `peri-tui/src/truncate.rs:103-119` 只将其转换成显示摘要。

已有测试 `test_tool_input_projection_preserves_object_root` 已验证当前占位投影行为：

```text
running 1 test
test agent::compact_v2::projection_tests::test_tool_input_projection_preserves_object_root ... ok

test result: ok. 1 passed; 0 failed
```

该测试证明占位对象是当前 Agent 层的预期实现，但尚无端到端回归测试覆盖“Agent 历史 prompt 被压缩 → 模型返回占位调用 → 工具缺少 prompt”这一危险连接。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-29 | — | Open | agent | 创建 |
| 2026-07-29 | Open | Fixed | agent | 字段级 micro compact 实现完成 |

## 修复记录

### 修复 #1（2026-07-29）

- **方案**：Micro Compact 从整体 tool input 替换改为字段级 head/tail 截断。仅超过 500 字符的顶层字符串字段被截断；短字段、必填参数和 JSON 结构完整保留。
- **涉及文件**：
  - `peri-agent/src/agent/compact_v2/config.rs` —— 新增 micro_field_threshold_chars (500)、micro_field_keep_head_chars (350)、micro_field_keep_tail_chars (100)
  - `peri-agent/src/agent/compact_v2/projection.rs` —— PROJECTION_POLICY_VERSION=2, CompactToolInput 字段级投影，apply_head_tail Unicode 安全截断
  - `peri-agent/src/agent/compact_v2/planner.rs` —— 仅选择超过阈值的顶层字符串字段和成功 ToolResult
  - `peri-agent/src/agent/compact_v2/planner_test.rs` —— 回归测试：Agent 短 prompt 不产生 action
  - `peri-agent/src/agent/stages/reason.rs` —— v1 directive 自动重规划为 v2
  - `peri-agent/src/agent/compact_v2/smart.rs` —— 统一使用 set_flags_projection
- **移除的旧行为**：整体替换为 `{"_compact_note":"tool input compacted"}`（`projection.rs` 的 CompactToolInput 路径）。
- **测试**：`cargo test -p peri-agent --lib compact_v2` 172 passed。
- **设计文档**：[Micro Compact 字段级压缩设计](./2026-07-29-micro-compact-field-level-design.md)
- **实现计划**：`docs/superpowers/plans/2026-07-29-micro-compact-field-level-implementation.md`
