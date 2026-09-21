# [P1] 后台 Bash 清理提示优先停止父 PID，可能遗留服务并阻塞 print 退出

**状态**：Open（指引已修正，待原评测复验）
**优先级**：P1（用户定级）
**类型**：Bug / 后台任务清理指引
**创建日期**：2026-09-21

## 问题与归因边界

原评测中，后台 Bash 内再次用 `&` 启动临时 HTTP 服务；模型依据工具提示尝试停止外层 shell PID，随后报告完成，但没有后台任务完成通知和 print 最终 result。现有提示优先推荐 `kill <pid>`，将停止整个进程组放在括号中，容易让模型误把父进程退出视为整项任务结束。

Peri 已给 Unix shell 创建独立进程组，现有执行器在整组退出、输出管道关闭后能够结算。本次先修正清理指引，不据这一案例扩展后台任务状态机或增加工具。Git、题目业务实现、verifier 编译失败均不在范围内。

## 症状详情

现场：`peri-3165-fail49-mt64/updo-policy-alerting__6jpcRNp`。

| 顺序 | 已观察事实 | 原始证据 |
|---|---|---|
| 1 | Bash 使用 `run_in_background=true`，命令内部又执行 `python3 /tmp/updo_e2e_server.py &`，启动常驻 HTTP 测试服务 | `agent/peri.txt:7514` |
| 2 | 工具返回任务 ID `shell-01a0bec9-0753-7ec0-a755-b2a8df57abd6`、PID `5036` 和 Running 状态；提示分别给出单 PID 与进程组的停止命令 | `agent/peri.txt:7516` |
| 3 | 模型执行 `kill 5036 2>/dev/null; ...; echo cleaned`，未检查该任务终态、服务端口关闭或后代进程退出 | `agent/peri.txt:7530` |
| 4 | 清理调用返回 `cleaned`、exit code 0；该结果属于清理命令，不能证明被清理的后台任务已结束 | `agent/peri.txt:7532` |
| 5 | 随后模型输出“完成”；导出轨迹没有该后台任务的完成通知，也没有最终 `type: result` | `agent/peri.txt` 尾部 |
| 6 | 外部 runner 以 `AgentTimeoutError` 结束，agent 执行区间恰为 10800 秒 | `result.json`、`exception.txt` |

时间：2026-09-20 12:17:44Z 开始，15:17:44Z 超时。E2E 测试输出中已有 12:27:43 时间戳；此后仍有其他操作，且最后回答无时间戳，因此不能精确计算回答后的等待时长，也不能把三小时全部归为模型工作耗时。

完整工具调用与结果有 138 对，无孤立调用；这只说明工具调用均有返回，不代表被启动的后台执行已完成。

## 最小修复

- 显式后台、超时转后台（有输出/无输出）和残留后代三个路径、四处结果共用平台清理提示。
- Linux/macOS：明确返回 `pgid`（本任务独立进程组，等于启动时 shell PID）；首先 `kill -TERM -- -<pgid>`，宽限后仍存活才 `kill -KILL -- -<pgid>`。不再推荐仅杀父 PID。
- Windows：提示 `taskkill /PID <pid> /T`，需要强制停止时加 `/F`；不输出 Unix PGID 或负 PID 命令。父 PID 已退出时，不能依赖 taskkill 找到其后代，应使用现有任务取消入口或确认剩余子进程。
- 已使用后台模式时，服务在外层 shell 内以前台方式运行：不再追加 `&`、`Start-Job`，不使用不带 `-Wait` 的 `Start-Process` 脱离父进程。
- 保留清理错误，核验实际进程退出与后台完成通知；不能用被隐藏的 kill 失败或末尾 `echo cleaned` 的成功代替终态。

## 验证与未验证项

- 平台提示测试分别检查 Linux、macOS、Windows 的指令和身份字段，Windows 不出现 Unix 负 PID 指令。
- 真实 Unix 回归通过原有 Bash 启动“父 shell 提前退出、后代持管道”的任务，再使用返回的 PGID 通过 Bash 整组停止，检查完成通知、输出采集和任务注册表结算。
- `cargo test -p peri-middlewares --lib middleware::terminal` 在 macOS 上 41 项通过（含新增两项）；Linux/Windows 实机和原 Updo 评测尚未复跑。提示测试不等于验证 Windows 进程树清理的运行行为。
- 提示修正不能保证模型一定选择正确命令；原评测是否恢复正常仍需复跑确认，因此 issue 保持 Open。

## 涉及文件

- `peri-middlewares/src/middleware/descriptions/bash.md`：平台清理纪律。
- `peri-middlewares/src/middleware/terminal.rs`：`background_cleanup_hint` 与四处结果提示。
- `peri-middlewares/src/middleware/terminal_test.rs`：平台提示及原执行链的整组清理回归。
- `docs/code-index/peri-middlewares.md`：入口索引。

## 状态记录

| 日期 | 状态 | 说明 |
|---|---|---|
| 2026-09-21 | Open | 从评测记录定位清理指引问题，按用户意见将方案收敛为现有 Bash 的跨平台清理指导；撤销先前执行层、ACP 和 print 扩展，待原评测复验 |

原始数据：`/Users/konghayao/Downloads/peri-3165-fail49-mt64/updo-policy-alerting__6jpcRNp/`。现场未保存进程树及文件描述符快照，不能把受控复现直接当作现场阻塞位置证明。

命令语义参考：[Linux kill](https://man7.org/linux/man-pages/man2/kill.2.html)、[Microsoft taskkill](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/taskkill)。
