>> stage: create

## 问题描述

Micro Compact 黑名单（`micro_excluded_tools`）默认为空，导致所有工具（包括 `AskUserQuestion`、`goal`、`TodoWrite`）的消息在超过 `micro_compact_stale_steps` 轮后被截断（`truncated` 标记）。

这会导致：
- Agent 忘记用户之前的回答（AskUserQuestion 的 Tool 消息被截断）
- Agent 忘记长期目标（goal 的交互历史被截断）
- Agent 忘记任务规划（TodoWrite 的状态被截断）

用户反馈：agent 每次问一个问题就重新开始，不符合预期。

## 症状详情

- AskUserQuestion 的 Tool 消息被截断后，agent 不知道用户之前选了/回答了啥，导致后续行为异常
- 对话工程层面的修改（上次 commit `a17f318c`）已将 auto-issue-fixer/brainstorming/05_using_tools 改为批量提问，但 Micro Compact 层面上没有同步保护 AskUserQuestion

## 决策依据

对全部工具按"是否可恢复/重建"进行保留决策：

| 工具 | 保留 | 理由 |
|------|:--:|------|
| AskUserQuestion | ✅ P0 | 用户答案不可恢复，丢失=对话断裂 |
| goal | ✅ P0 | 长期目标状态，丢失=agent 漂移方向 |
| TodoWrite | ✅ P0 | 任务列表结构，agent 工作记忆，丢失=忘记规划 |
| Read/Grep/Glob | ❌ | 磁盘数据不变，可重读 |
| Write/Edit | ❌ | 改动已落盘 |
| Bash | ❌ | 副作用已应用 |
| WebFetch/WebSearch | ❌ | 可重搜 |
| Agent/Workflow | ❌ | 结果已被下游消费 |
| Skill/DiscoverSkills | ❌ | 每 turn 从磁盘重载 |
| folder_operations | ❌ | 磁盘状态不变 |

## 修复内容

- `peri-agent/src/agent/compact_v2/config.rs`: `default_excluded_tools()` 从 `vec![]` 改为 `vec!["AskUserQuestion", "goal", "TodoWrite"]`
- `peri-agent/src/agent/compact_v2/config_test.rs`: 更新默认值断言
- `peri-agent/src/agent/compact_v2/micro_test.rs`: 新增 2 个测试验证 AskUserQuestion 和 TodoWrite 的保留行为

## 涉及文件

- `peri-agent/src/agent/compact_v2/config.rs`
- `peri-agent/src/agent/compact_v2/config_test.rs`
- `peri-agent/src/agent/compact_v2/micro_test.rs`

## 类型

重构

## 优先级

中

**状态**：Archived

## 状态变更记录

| 日期 | 原状态 | 新状态 | 操作人 | 操作说明 |
|------|--------|--------|--------|----------|
| 2026-07-25 | 创建 | Fixed | agent | 修改默认黑名单为 3 类不可恢复工具的保守值 |
| 2026-07-25 | — | Archived | agent | 归档：移动到 spec/archive-issues/agent-core/ |

## 修复记录

### 修复 #1（2026-07-25）

- **操作人**：agent
- **用户原意**：Micro Compact 应保护用户交互和任务状态工具的消息不丢失
- **修复内容**：`default_excluded_tools()` 返回 `["AskUserQuestion", "goal", "TodoWrite"]`，测试覆盖
- **涉及 commit**：待提交
- **验证状态**：已验证（62 test pass）
