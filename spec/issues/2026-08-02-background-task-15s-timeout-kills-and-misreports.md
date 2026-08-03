# 后台任务受默认 15s 超时约束，被杀后通知误报失败

**状态**：Fixed
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
| 2026-08-02 | Open | Fixed | agent | 修复：bg 默认不超时 + 进程组 kill + 同步超时转后台续跑 + timed_out 结构化标记 |

## 修复记录

### 修复 #1（2026-08-02）

- **操作人**：agent
- **用户原意**：后台任务不应受 15s 默认超时约束；超时/取消必须杀整个进程组（不留孤儿进程）；前台命令超时不能输出全丢；超时终止的通知不能误报"执行失败"
- **修复内容**：
  1. **Step 1 进程组 kill 工具**：`peri-middlewares/src/process/mod.rs` 新增 `kill_process_group(pid, signal)`（Unix 执行 `kill -<SIG> -- -<pid>` 杀整个进程组，Windows 回退 `taskkill /T /F`）；`background.rs` cancel() 的 Pid 分支与 `terminal.rs` bg 超时分支改用，TERM 无效时 2s 后升级 KILL
  2. **Step 2 超时语义**：新增 `parse_timeout(input, is_background)`——后台未传/显式 0 → 不超时（跑完为止），同步未传 → 15s，显式 0 → 不超时，>0 → clamp [min, 600_000]；删除 `timeout_ms == 0` 死代码；`parameters()` 与 `descriptions/bash.md` 文案同步
  3. **Step 3 同步路径流式重构**：`cmd.output()` 改手动 spawn + 双 pipe 流式读取（共享缓冲 2MB 上限，超限继续排空防子进程写阻塞）；无注册表超时 → 杀进程组 + 部分输出落盘（`persist_partial_output`，提示含 "partial output"），Err 含 "timed out"
  4. **Step 4 同步超时转后台续跑**：有注册表时超时不杀进程——部分输出落盘 → 构造 `BackgroundTask`（`shell-<uuid8>`，handle=Pid）→ `register_with_kind` → spawn 续跑任务读 pipe 至 EOF + `child.wait()` → 复用提取的 `finalize_bg_shell` 收尾 helper → agent 收 bg-task-completed 通知；Err 文案含 task_id + "the process is now running as a background task; you will be notified when it completes" + 部分输出路径；注册失败（SHELL_LIMIT 满）回退杀组并注明原因
  5. **Step 5 结构化超时标记**：`BackgroundTaskResult` 新增 `#[serde(default)] timed_out: bool`；`to_notification()` 对 success=false && timed_out 输出"[后台任务 X 超时被终止]（进程组已终止，逃逸子进程可能存活）"；全仓库 18 处构造点机械补齐（terminal ×4、execute_bg ×2、spawner ×2、workflow、executor、测试 ×5），超时路径置 true
  6. **测试**：`terminal_test.rs` 新增 `parse_timeout` 纯函数单测、bg 显式超时杀进程组（marker 验证无孤儿）、同步超时 promote（回调 success + active_count 归零 + task_id 一致）、同步超时回退杀组 + 部分输出落盘内容断言；`background_test.rs` 新增 cancel() 杀进程组测试
- **涉及文件**：`process/mod.rs`、`background.rs`、`background_test.rs`、`terminal.rs`、`terminal_test.rs`、`descriptions/bash.md`、`events.rs`、`execute_bg.rs`、`spawner.rs`、`workflow/mod.rs`、`executor.rs`、`async_router_test.rs`、`mapper_test.rs`、`concurrent_bg_agent_test.rs`
- **涉及 commit**：未提交（用户统一提交）
- **验证状态**：见验证命令输出（period-middlewares lib 测试、peri-agent lib 测试、workspace 构建、fmt/clippy）
