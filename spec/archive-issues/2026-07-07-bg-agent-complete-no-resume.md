> 归档于 2026-07-18，原路径 spec/issues/2026-07-07-bg-agent-complete-no-resume.md
# bg agent 完成后主 agent 永久卡死、合成消息未注入主消息区

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-07

## 问题描述

通过 `/bg <prompt>` 或主 agent 调用 Agent 工具（background 模式）启动的 bg agent，在它**完成时**，TUI 的顶部完成通知条出现了、Tasks 面板的运行中计数也正确归零——bg 完成事件本身流到了 TUI。但是主 agent 的消息区**完全没有**出现「[后台任务 XXX 已完成] 输出：...」这样的合成消息，主 agent 的 loading spinner 一直转，永久卡死，必须手动 Esc / Ctrl+C / 重启 TUI 才能恢复。

按 `2026-07-07-bg-tasks-unified-management.md` PRD 的 user story #9、#36，期望行为是 bg 完成时合成消息注入主 agent inbox、主 agent 下一轮 ReAct loop 续跑处理结果；当前实际行为是这个续跑链路从未发生。

**前置上下文**：本 issue 紧接 `bg agent 完成时 loading 永久卡住` 的 push_done 修复（同日），用户已确认「上面的 ok 了」（即修复方案中 push_done 那部分起作用了——bg 完成事件能流到 TUI、通知条 + 面板归零能触发），但主 agent 续跑这边的卡住仍然存在。

## 症状详情

### 用户可观察的现象对比

| 触发时机 | 期望行为（PRD 设计意图） | 实际行为 |
|---------|------------------------|---------|
| bg agent 完成时 | ① 顶部通知条 `[✓] bg-xxx 完成 (Ns)` 闪现 ② Tasks 面板 bg 任务移除 ③ **主消息区出现「[后台任务 bg-xxx 已完成] 输出：...」合成消息** ④ **主 agent loading 转入新一轮推理，按结果继续输出文字** | ① ✅ 通知条出现 ② ✅ Tasks 面板归零 ③ ❌ **合成消息根本没出现** ④ ❌ **loading 一直转，永久卡死** |
| 卡住后的恢复 | 主 agent 自动续跑完成本轮后 loading 退出，输入框恢复可输入 | 必须手动 Esc / Ctrl+C / 重启 TUI 才能恢复，不会自愈 |

### 关键现象信号

- **bg 完成事件流到 TUI 是正常的**：通知条出现 + Tasks 面板计数归零——说明 `BackgroundTaskCompleted` → ACP unstable event → `acp_notifier` → TUI 通知条 + `BG_TASKS` atom 这条路径是通的。
- **合成消息未注入主 agent inbox**：主消息区没出现「[后台任务 XXX 已完成]」类合成消息——说明 `MessageQueue + bg_results 通道注入主 agent inbox + Defer 消息 + 唤醒续跑` 这条路径（PRD 第 215-217 行设计意图）从未生效。
- **主 agent loading 永久转**：因为合成消息没注入，主 agent 的 ReAct loop 续跑从未被触发；同时 loading 不会被任何事件清回（因为 `push_done` 触发的 `TurnDone` 之后，没有新一轮的 `SubagentStarted` 之类的把 loading 设回，但 loading 也没退出——说明卡住可能更早就发生，或 push_done 触发后 TUI loading 退出瞬间又因别的路径被重新设回，需进一步排查）。

## 复现条件

- **复现频率**：必现（用户描述语气为"好像就是没有触发 agent loop 一样"，每次 bg agent 完成都会触发）
- **触发步骤**：
  1. 启动 TUI（`cargo run -p peri-tui -- -a`）
  2. 输入 `/bg <prompt>`（如 `/bg list files in /tmp`），或让主 agent 在对话中主动调用 Agent 工具（background 模式）
  3. 等待 bg agent 完成（状态栏 ` · 1 agent` 计数减为 0）
  4. 观察：顶部通知条出现 `[✓] bg-xxx 完成`、Tasks 面板归零——bg 完成事件流通
  5. **关键观察点**：主消息区**不会**出现「[后台任务 XXX 已完成] 输出：...」合成消息，主 agent loading 一直转
  6. 必须手动 Esc / Ctrl+C / 重启 TUI 才能恢复
- **环境**：
  - OS：Darwin 25.5.0（macOS）
  - 分支：`feature/v2-architecture`
  - 修复前置：刚修复 `bg agent 完成时 loading 永久卡住` 的 push_done（同日 commit），用户确认该修复生效（"上面的 ok 了"）

## 涉及文件

用户未提及具体文件。根据 PRD `2026-07-07-bg-tasks-unified-management.md` 第 215-217 行的设计意图，相关路径应为 `MessageQueue + bg_results 通道注入主 agent inbox`；具体实现位置与断点位置待 `fix-issue` / `diagnose` skill 排查（本 issue 文档不做诊断）。

相关参考：
- `spec/issues/2026-07-07-bg-tasks-unified-management.md` —— 后台任务统一管理 PRD（设计意图来源）
- 同日修复的 push_done 改动（`peri-acp/src/session/executor.rs` bg event pump 消费者）——本 issue 是该修复完成后观察到的剩余现象

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-07 | — | Open | agent | 创建（issue-create skill 访谈还原现象） |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
