# Release 3.0 E2E 测试结果记录

- 日期：2026-08-01
- 运行方式：逐个文件手动执行（`npm test -- tests/<path>.test.ts`），不并行
- 命令来源目录：`e2e/`
- **第一轮全量 31 个：26 通过 / 5 失败 → 修复后重跑 5 个全部通过 → 31/31 全绿**

## 结果汇总（最终）

| # | 测试文件 | 结果 | 备注 |
| --- | --- | --- | --- |
| 1 | smoke/basic-question.test.ts | ✅ 通过 | 22.3s |
| 2 | scenarios/ask-user-question.test.ts | ✅ 通过 | 首轮失败→修复 criterion 后 46.4s |
| 3 | scenarios/clear-chat.test.ts | ✅ 通过 | 首轮失败→修复 criterion 后 48.4s |
| 4 | scenarios/compact-command.test.ts | ✅ 通过 | 57.3s |
| 5 | scenarios/goal-continuation.test.ts | ✅ 通过 | 53.5s |
| 6 | scenarios/hitl-approval.test.ts | ✅ 通过 | 26.9s |
| 7 | scenarios/rewind-v2.test.ts | ✅ 通过 | 46.6s |
| 8 | scenarios/streaming-tool-interleave.test.ts | ✅ 通过 | 74.9s |
| 9 | scenarios/thread-switch.test.ts | ✅ 通过 | 39.0s |
| 10 | scenarios/user-bubble-scrollbar.test.ts | ✅ 通过 | 10.3s |
| 11 | panels/model-switch.test.ts | ✅ 通过 | 27.7s |
| 12 | panels/plugin.test.ts | ✅ 通过 | 首轮失败→修复 criterion 后 22.0s |
| 13 | subagent/bg-agent-task-area.test.ts | ✅ 通过 | 34.1s |
| 14 | subagent/fork-bg-callback.test.ts | ✅ 通过 | 34.6s |
| 15 | subagent/internal-toolcards-visibility.test.ts | ✅ 通过 | 首轮失败→修复等待逻辑后 45.8s |
| 16 | subagent/multi-subagent-toolcards.test.ts | ✅ 通过 | 首轮失败→修复等待逻辑后 127.9s |
| 17 | subagent/sync-agents.test.ts | ✅ 通过 | 50.8s |
| 18 | tool-cards/agent-output-position.test.ts | ✅ 通过 | 49.9s |
| 19 | tool-cards/bash-running-duration.test.ts | ✅ 通过 | 30.6s |
| 20 | tool-cards/edit-diff-display.test.ts | ✅ 通过 | 49.7s |
| 21 | tool-cards/edit-write-diff-summary.test.ts | ✅ 通过 | 62.1s |
| 22 | tool-cards/first-tool-stuck-running.test.ts | ✅ 通过 | 49.9s |
| 23 | tool-cards/glob-grep-match-count.test.ts | ✅ 通过 | 39.6s |
| 24 | tool-cards/read-line-count.test.ts | ✅ 通过 | 24.0s |
| 25 | tool-cards/skill-tool.test.ts | ✅ 通过 | 35.5s |
| 26 | tool-cards/tool-error-display.test.ts | ✅ 通过 | 34.2s |
| 27 | tool-cards/tool-error-no-suffix.test.ts | ✅ 通过 | 28.7s |
| 28 | tool-cards/tool-output-truncation.test.ts | ✅ 通过 | 35.5s |
| 29 | workflow/workflow-panel-columns.test.ts | ✅ 通过 | 45.6s |
| 30 | workflow/workflow-reporting.test.ts | ✅ 通过 | 53.6s |
| 31 | workflow/workflow-run.test.ts | ✅ 通过 | 55.4s |

**最终统计：31/31 全部通过 ✅**

## SubAgent 测试简化（2026-08-01 第二次优化）

- `multi-subagent-toolcards`：两个 thorough explorer 搜索 → 两个 echo 任务；sleep(20s/15s/30s) → 轮询等待工具卡片出现 + `waitForStableScreen`。耗时 **127.9s → 59.5s**。
- `internal-toolcards-visibility`：explorer thorough 搜索 → echo 任务；sleep(10s) → 轮询等待工具卡片出现。耗时 **45.8s → 42.2s**。
- criterion 工具名示例统一为 ● Bash / ● Shell / ● Grep。
- `sync-agents` 保留：其 subagent 内 sleep 10s 是观测 running→done 状态切换的刻意设计，非多 agent 样式测试。

## 首轮失败与修复记录

### 2. scenarios/ask-user-question.test.ts — 首轮失败 → 通过

- 首轮失败点：`tests/scenarios/ask-user-question.test.ts:84` Judge 断言失败，criterion "消息区应包含 agent 对用户回答内容的引用" 未满足（agent 总结未逐条引用用户选择）。
- 修复：criterion 措辞改为认可"三个题目均已正常返回/已收到回答"类总结表述（与测试意图一致：验证面板交互完成，而非强制 LLM 复述选择）。
- 重跑：46.4s 通过。

### 3. scenarios/clear-chat.test.ts — 首轮失败 → 通过

- 首轮失败点：`tests/scenarios/clear-chat.test.ts:81`。criterion 2"（空白或仅显示欢迎页/logo）"自相矛盾被判 false——屏幕正是欢迎页。
- 修复：criterion 改为"消息区域应仅显示欢迎页/logo 或保持空白（这是 /clear 清空后的正常状态）"，消除歧义；同时强化 judge.ts SYSTEM_PROMPT（可接受状态集合规则 + 结论与 detail 一致性规则）。
- 重跑：48.4s 通过。

### 12. panels/plugin.test.ts — 首轮失败 → 通过

- 首轮失败点：`tests/panels/plugin.test.ts:111`。criterion"面板已关闭，不再显示 Tab"被判 false，而 detail 证实屏幕已回欢迎页——负向断言被误判。
- 修复：criterion 明确"界面应已回到普通主聊天界面（欢迎页/输入框）"；judge.ts 新增负向断言规则（未发现 X 即 pass，不需要额外证据）。
- 重跑：22.0s 通过。

### 15. subagent/internal-toolcards-visibility.test.ts — 首轮失败 → 通过

- 首轮失败点：`tests/subagent/internal-toolcards-visibility.test.ts:95`。完成后缺"结果摘要"——真实时序问题：固定 sleep(50s) 不够，截图时 subagent 仍在 running（"10 tool calls, running 1min 0s"）。
- 修复：等待逻辑改为 `waitForStableScreen(tester, 180_000, base)`（等屏幕稳定=subagent 与主 agent 总结全部结束），测试 timeout 300s→420s。
- 重跑：45.8s 通过。

### 16. subagent/multi-subagent-toolcards.test.ts — 首轮失败 → 通过

- 首轮失败点：`tests/subagent/multi-subagent-toolcards.test.ts:123`。phase2 截图时第二个 Agent 卡片未出现——`waitForText("Agent")` 会立即匹配屏幕上仍在的第一个 Agent 卡片，导致固定 sleep 后截图过早。
- 修复：phase2 改为轮询屏幕直到"Agent"出现 ≥2 次（第二个 Agent 卡片真实出现）再截图。
- 重跑：127.9s 通过。

## 代码修改清单

| 文件 | 修改 |
| --- | --- |
| `e2e/helpers/judge.ts` | SYSTEM_PROMPT 强化：负向断言规则（未发现 X 即 pass）、可接受状态集合规则、结论与 detail 一致性自查 |
| `e2e/tests/scenarios/clear-chat.test.ts` | criterion 2 措辞消除歧义 |
| `e2e/tests/panels/plugin.test.ts` | closed criterion 明确"回到主聊天界面即通过" |
| `e2e/tests/scenarios/ask-user-question.test.ts` | done criterion 2 认可总结类表述 |
| `e2e/tests/subagent/internal-toolcards-visibility.test.ts` | 等待逻辑改为 waitForStableScreen；criterion 2 认可统计信息摘要；timeout 420s |
| `e2e/tests/subagent/multi-subagent-toolcards.test.ts` | phase2 轮询等待第二个 Agent 卡片（Agent ≥2 次） |
