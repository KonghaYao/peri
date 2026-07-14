# 后台 subagent 完成通知中的"工具调用"计数始终为 0，与 agent 实际调用的工具数不一致

**状态**：Open
**优先级**：中
**创建日期**：2026-07-10

## 问题描述

后台 subagent（bg-fork、bg-agent）完成时，系统注入到主 agent 对话流中的 `<system-reminder>` 通知消息始终显示 `工具调用: 0`，即使该 subagent 在运行期间实际调用了工具（如 Bash、Read 等）。

用户在对话中看到的通知格式为：
```
[后台任务 bg-edd0c 已完成] Agent: hello-agent | 工具调用: 0 | 耗时: 14630ms
```

其中 `工具调用: 0` 是固定值，不反映 subagent 实际执行的工具调用次数。agent 本身的回复内容能证明它确实调用了工具（如执行了 `sleep 2` 并给出 exit code 0），但计数显示为 0，形成矛盾。

## 症状详情

### 观察 1：后台 agent（2026-07-10）

| 观察点 | 详情 |
|--------|------|
| Agent 类型 | general-purpose（run_in_background=true） |
| 提示词要求 | 调用 Bash 执行 sleep 2 |
| Agent 回复声称 | 调用了 `Bash: sleep 2`，退出码 0 |
| 系统通知显示 | `工具调用: 0` |
| 耗时 | 7222ms（含初始化开销） |

### 观察 2：同步 agent（对比）

| 观察点 | 详情 |
|--------|------|
| Agent 类型 | coder（同步调用） |
| 提示词要求 | 调用 Bash 执行 sleep 2 |
| Agent 回复声称 | 调用了 `Bash: sleep 2`，退出码 0 |
| 反馈中是否有计数字段 | 无（同步调用不经过 bg task 通知通道） |

### 现象模式

- 后台 agent 的"工具调用"计数**恒为 0**，不管实际执行了多少工具
- 同步 agent 不经过此通知通道，不受影响
- Agent 回复中确实包含了工具调用的证据（输出中包含 exit code、命令结果等）

## 复现条件

- **复现频率**：必现（每次后台 subagent 完成）
- **触发步骤**：
  1. 使用 Agent 工具派发一个后台 subagent（如 `general-purpose` + `run_in_background=true`）
  2. 要求该 subagent 调用任意工具（如 Bash）
  3. 等待后台任务完成
  4. 观察系统通知中的 `工具调用: N`
- **环境**：macOS，当前开发版本

## 涉及文件

- `peri-agent/src/agent/events.rs:3-32` —— `BackgroundTaskResult` 结构体定义 `tool_calls_count: usize` 字段 + `to_notification()` 格式化 `工具调用: {}`
- `peri-middlewares/src/subagent/tool/execute_bg.rs:248-257` —— bg subagent **错误**路径构造 `BackgroundTaskResult`，硬编码 `tool_calls_count: 0`
- `peri-middlewares/src/subagent/tool/execute_bg.rs:301-314` —— bg subagent **成功**路径构造 `BackgroundTaskResult`，硬编码 `tool_calls_count: 0`
- `peri-middlewares/src/subagent/spawner.rs:270-279` —— fork subagent **错误**路径构造 `BackgroundTaskResult`，硬编码 `tool_calls_count: 0`
- `peri-middlewares/src/subagent/spawner.rs:312-325` —— fork subagent **成功**路径构造 `BackgroundTaskResult`，硬编码 `tool_calls_count: 0`

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-10 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
