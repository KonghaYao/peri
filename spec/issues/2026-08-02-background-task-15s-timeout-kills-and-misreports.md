# 后台任务受默认 15s 超时约束，被杀后通知误报失败

**状态**：Open
**优先级**：高
**创建日期**：2026-08-02

## 问题描述

`run_in_background=true` 的任务仍受 Bash 工具默认 15s 超时约束（与"后台"语义矛盾）。超时只杀 wrapper 不杀进程组，子进程孤儿存活跑完任务，通知却误报"失败"；前台命令超时进程树被杀、输出全丢，测试白跑。wander 报告（8-01 后样本）2/5 会话复现。

## 症状详情

- **019fbdbe**：
  - #59：后台 3 分钟监控实验被 15s 默认超时静默杀掉
  - #177：已知 15s 限制后仍未传 timeout，后台 npm test 又被杀——子进程孤儿存活跑完测试，通知却误报"失败"（#181），agent 靠读日志才发现实际通过
  - #165：前台 600s 超时，进程树被杀、输出全丢，靠 ps/tmux ls 拼状态，测试全部白跑
- **019fc204**：#150 run_in_background=true 未设 timeout → #157 15s 被杀 → #158 前台重跑成功（19.8s），agent 自述"我忘了设 timeout"

**根因线索**：`run_in_background` 仍受默认 15s 超时约束（设计意图矛盾）；超时只杀 wrapper 不杀进程组（孤儿进程存活、通知语义错乱）。上一轮演进见 `spec/archive-issues/tools/2026-07-28-bg-shell-no-timeout-no-callback-to-agent.md`。

## 复现条件

- **复现频率**：必现（后台运行 >15s 的命令且不传 timeout）
- **触发步骤**：
  1. 调用 Bash 工具 `run_in_background=true` 运行耗时 >15s 的命令（如 npm test）
  2. 不显式传 timeout
  3. 观察：15s 后任务被杀；若子进程孤儿存活，通知结果与实际运行结果不一致
- **环境**：peri agent 8-01 后会话，macOS

## 涉及文件

- Bash 工具实现（`run_in_background` / timeout / 进程组 kill 语义所在处，修复时定位）
- 参考：`spec/archive-issues/tools/2026-07-28-bg-shell-no-timeout-no-callback-to-agent.md`

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
