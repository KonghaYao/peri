# E2E 测试报告 #1

**日期**: 2026-07-28（更新于 2026-07-29）  
**总测试数**: 30  
**通过**: 26 ✅  
**失败**: 4 ❌  
**通过率**: 86.7%

---

## 通过列表 (26/30)

| # | 测试文件 | 耗时 |
|---|---------|------|
| 1 | smoke/basic-question | 26.1s |
| 2 | panels/model-switch | 16.1s |
| 4 | scenarios/clear-chat | 46.0s |
| 5 | scenarios/compact-command | 41.4s |
| 7 | scenarios/thread-switch | 29.9s |
| 8 | scenarios/user-bubble-scrollbar | 10.2s |
| 9 | scenarios/ask-user-question | 47.8s |
| 10 | scenarios/goal-continuation | 62.5s |
| 11 | scenarios/hitl-approval | 27.6s |
| 12 | subagent/bg-agent-task-area | 31.1s |
| 13 | subagent/fork-bg-callback | 32.2s |
| 14 | subagent/internal-toolcards-visibility | 89.3s |
| 16 | subagent/sync-agents | 42.5s |
| 17 | tool-cards/agent-output-position | 54.4s |
| 18 | tool-cards/bash-running-duration | 30.2s |
| 19 | tool-cards/edit-diff-display | 46.0s |
| 20 | tool-cards/edit-write-diff-summary | 53.1s |
| 22 | tool-cards/glob-grep-match-count | 35.9s |
| 23 | tool-cards/read-line-count | 21.0s |
| 24 | tool-cards/skill-tool | 33.4s |
| 26 | tool-cards/tool-error-no-suffix | 26.9s |
| 27 | tool-cards/tool-output-truncation | 27.2s |
| **4**  | **tool-cards/first-tool-stuck-running** | **49.4s** |
| **6**  | **workflow/workflow-panel-columns** | **40.5s** |
| **7**  | **workflow/workflow-run** | **52.8s** |
| 29 | workflow/workflow-reporting | 59.7s |

> 加粗项为 2026-07-29 修复后通过。

---

## 失败列表 (4/30)

### 1. panels/plugin.test.ts — Marketplaces Tab 不通过

**错误**: `AssertionError: expected false to be true` at line 91  
**根因**: LLM Judge 对 Marketplaces tab 的检查返回 `pass: false`。plugin 面板打开后，Marketplaces 标签页的内容展示不符合预期（可能是列表为空、布局异常或内容不完整）。

### 2. scenarios/streaming-tool-interleave.test.ts — 流式工具交错渲染

**错误**: `AssertionError: expected false to be true` at line 56  
**根因**: LLM Judge 对"流式文本与工具调用交错输出不渲染错位"的检查返回 false。可能在流式输出过程中，文本块与工具调用卡的渲染顺序或布局出现了错位。

### 3. subagent/multi-subagent-toolcards.test.ts — Phase 2 Judge 失败

**错误**: `AssertionError: expected false to be true` at line 123  
**根因**: Phase 2（两个 SubAgent 同时运行中）的 LLM Judge 检查返回 false。可能是第二个 SubAgent 的内部工具条目未正确显示，或两个 Agent 卡片的层级关系渲染不正确。

### 5. tool-cards/tool-error-display.test.ts — 错误态显示不通过

**错误**: `AssertionError: expected false to be true` at line 70  
**根因**: Phase 2（agent 对错误做出响应后）的 LLM Judge 检查返回 false。可能是工具错误后的卡片状态（错误色/错误图标/错误消息展示）与预期不符，或者 agent 的后续响应缺少对错误的引用。

---

## 已修复项

### #4 tool-cards/first-tool-stuck-running ✅

**原错误**: batch 完成后 `hasRead` 为 false，第一个工具卡片疑似消失。  
**根因**: **误报**。经逐段代码验证（`start_tool`→`end_tool`→`TurnDone/flush`→`build_view_models`→`push_view_models`→渲染），工具卡片从未被删除。失败原因为并发运行时 tmux session 冲突导致 `can't find pane`。  
**修复**: 无代码变更，同步执行后通过。

### #6 workflow/workflow-panel-columns ✅

**原错误**: tmux Server 崩溃 / "已完成" 未找到。  
**根因**: E2E 测试使用 `/workflow`（单数），但 `panel_registry.rs:328` 注册为 `slash_command: "workflows"`（复数），导致 slash command 不匹配，面板无法打开。  
**修复**: `e2e/tests/workflow/workflow-panel-columns.test.ts:53` — `sendText("/workflows")`。

### #7 workflow/workflow-run ✅

**原错误**: workflow 完成后 `/workflow` 无法打开面板（`Text "Workflow" not found`）。  
**根因**: 同 #6——slash command 不匹配 + `waitForStableScreen` 后 `sendPrompt` 字符级输入与 TUI 事件循环产生竞态。  
**修复**: 
- `e2e/tests/workflow/workflow-run.test.ts:48,66` — `/workflow`→`/workflows`
- `workflow-run.test.ts:63` — `waitForStableScreen` 后加 2s 延迟，第二次面板打开改用 `sendText`+`sendKey("Enter")`

---

## 风险评级

| 风险等级 | 测试 | 说明 |
|---------|------|------|
| 🟡 中 | #2 streaming-tool-interleave | 流式渲染布局问题 |
| 🟡 中 | #5 tool-error-display | 错误态 UI 展示问题 |
| 🟢 低 | #1 plugin | Plugin Marketplaces tab 内容显示 |
| 🟢 低 | #3 multi-subagent-toolcards | 多 SubAgent 并行时内部工具显示 |

---

## 建议

1. **#2、#5、#1、#3** 均为 LLM Judge 判定不通过，建议人工查看录制快照 (`e2e/results/`) 确认是否为 Judge 误判还是真实 UI 问题。
2. 新增 Workflow 面板 slash command 测试用例到 `submit_request_test.rs`，防止命令名不匹配再次发生。
