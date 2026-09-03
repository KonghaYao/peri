# 恢复 Subagent 后的新消息归入旧 Agent 调用

**状态**：Fixed
**优先级**：中
**创建日期**：2026-09-03

## 问题描述

TUI 中一个前台 Agent 调用因 `model stream interrupted` 失败后，主 Agent 使用
`resume_thread_id` 恢复该 Subagent。恢复调用已经显示为新的 `Agent Continue ...` 行，
但恢复后产生的 Shell、Read 和文本消息仍追加在先前失败的 Agent 分组下。期望恢复后的
所有子消息归属于新的 Continue 调用，旧失败分组保持封闭。

## 症状详情

- 第一个 Agent 分组显示 `child_thread_id` 和 `model stream interrupted`，状态为失败。
- 随后出现新的 `Agent Continue S30 ...` 调用行。
- Continue 调用执行期间产生的子消息显示在旧失败分组上方，导致事件归属和时间顺序看起来错误。
- 截图中的红框和注释指出：“resume 的 agent 的消息都归到上面去了”。

## 复现条件

- **复现频率**：截图中至少观察到一次，是否必现待自动化测试确认。
- **触发步骤**：
  1. 启动一个会向 TUI 转发子事件的前台 Agent。
  2. 让该 Agent 因模型流中断而失败并返回 `child_thread_id`。
  3. 使用 `resume_thread_id` 发起新的 Continue Agent 调用。
  4. 观察恢复后的 Shell、Read 与文本消息所属的 TUI Agent 分组。
- **环境**：Peri TUI；前台 Agent；恢复既有 child thread；截图未提供模型和 OS 版本。

## 涉及文件

- `peri-tui/src/kit/acp_types/current_turn.rs` —— 当前主 turn 内 Subagent occurrence 的创建、事件路由和 Agent ToolCard 配对。
- `peri-tui/src/kit/acp_types_test.rs` —— 同一 child ID 中断后恢复的回归测试。
- `peri-middlewares/src/subagent/` —— 核查确认 resume 正确复用 child identity，本次无需修改。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-09-03 | — | Open | agent | 根据 TUI 截图创建 |
| 2026-09-03 | Open | Fixed | agent | 允许已停止 child ID 创建新 occurrence，后续事件路由到最新分组 |

## 修复记录

### 修复 #1（2026-09-03）

- **操作人**：agent
- **用户原意**：恢复 Subagent 后产生的消息应显示在新的 Continue Agent 调用下，而不是回流到旧失败分组。
- **修复内容**：`CurrentTurn` 仅去重仍在运行的同 ID Subagent；旧 occurrence 停止后允许创建新分组并 claim 新 Agent ToolCard。Text、Reasoning、Tool 和 Stop 事件均从尾部匹配最新 occurrence。新增回归测试覆盖中断、同 ID 恢复、两组内容隔离与时序配对。
- **涉及 commit**：未提交
- **验证状态**：待用户验证（自动化构建、crate 全量测试和 clippy 已通过）

## 残余风险

同一主 turn 内的多个 occurrence 仍共享 child `agent_id` 作为折叠覆盖和详情选择键。
本次修复保证主消息流的内容与 Agent 调用配对正确；若用户手动操作两个同 ID 分组的折叠状态，
或分别打开详情面板，交互身份仍可能相互影响，后续应拆分 child 路由 ID 与展示 occurrence ID。
