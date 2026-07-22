# Agent 用 shell + sleep 轮询等待异步操作结果

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-22

## 问题描述

Agent 在派发异步操作（后台 subagent、workflow 等）后，不按 prompt 指令停下来、不调用工具，而是用 `bash sleep N` 的方式主动轮询等待结果。系统 prompt 已明确指示 "do not call any tools until the notification arrives"，但 agent 绕过此规则——它知道 AgentResult 不能轮询，就用 shell sleep 作为变通手段。

## 症状详情

| # | 表现 | 影响 |
|---|------|------|
| 1 | **额外消耗 token** | agent 在 sleep 等待期间继续发消息、做推理，导致大量无效 token 消耗 |
| 2 | **单纯浪费时间** | agent sleep 阻塞，sleep 结束后再去查看结果，浪费了等待时间。实际上结果已通过 system-reminder 自动推送，无需等待 |
| 3 | **Sleep 后无法获取结果** | sleep 完再去读结果时发现读不到——结果已通过 system-reminder 推送过了，agent 错过了通知窗口 |
| 4 | **覆盖所有异步操作** | 不管是后台 subagent 还是 workflow，agent 都倾向于用 sleep 等待 |

典型行为序列：

```
1. Agent 调用 Agent(run_in_background=true) 或 Workflow
2. Agent 告知用户"后台任务已启动"
3. Agent 调用 Bash(sleep 30) 等待
4. 30s 后 agent 继续推理，尝试读取结果
5. 结果已通过 system-reminder 推送过但 agent 错过了
6. 或结果还未完成，agent 再 sleep 一次
```

## 复现条件

- **复现频率**：几乎每次都出现（只要 agent 派发后台任务/workflow）
- **触发步骤**：
  1. 要求 agent 执行需要异步操作的任务（如派发 bg subagent 或 workflow）
  2. Agent 调用异步工具后
  3. 观察 agent 是否在下一轮使用 Bash sleep 等待
- **环境**：当前所有模型都可能出现

## 涉及文件

- `peri-acp/prompts/sections/11_subagent.md:58-65` —— Background Tasks 指引段，已有 "do not call any tools until the notification arrives"，但未明确禁止 shell/sleep
- `peri-acp/prompts/sections/14_system_reminder.md` —— system-reminder 机制说明
- `CLAUDE.md:395` —— 已有 "AgentResult 禁止轮询" 规则，但未覆盖 sleep 变通

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-22 | — | Open | agent | 创建 |
| 2026-07-22 | Open | Fixed | agent | 修复：在 11_subagent.md、16_workflow.md、CLAUDE.md 中明确禁止 bash sleep 等待异步结果 |

## 修复记录

### 修复 #1（2026-07-22）

- **操作人**：agent
- **用户原意**：agent 用 shell + sleep 轮询等待异步结果，浪费 token、错过通知窗口，需要让 agent 正确停下来等待系统唤醒
- **修复内容**：三处 prompt 文本增强：
  1. `peri-acp/prompts/sections/11_subagent.md`：Background Tasks 段增加 "This includes Bash/Shell — do NOT use `sleep`, `timeout`, or any polling loop to wait for results. The system will wake you automatically when results are ready."
  2. `peri-acp/prompts/sections/16_workflow.md`：Workflow 指引段增加 "Do NOT use Bash/shell `sleep` or any polling loop to wait for results — the system will wake you automatically."
  3. `CLAUDE.md`：新增 "禁止 shell sleep 等待异步结果" 陷阱速查条目
- **涉及 commit**：待提交
- **验证状态**：已验证（2026-07-22 E2E 测试全绿）
