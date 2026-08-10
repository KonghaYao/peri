# Bash 超时错误定位失误：等 stdin 的进程被无条件 promote 成永不结束的后台任务

**状态**：Fixed
**优先级**：高
**类型**：缺陷
**创建日期**：2026-08-10

## 问题描述

Bash 工具同步路径超时后**无条件 promote 为后台任务续跑**（2026-08-02 修复引入的"长任务不白跑"语义），且文案承诺"you will be notified when it completes"。但当进程**永远不会自行结束**时（等待终端输入、stdio 服务、交互式命令），promote 产生的是：

1. **孤儿进程泄漏**：后台任务永不 complete，通知永不到达，只有 `kill` 能清理；
2. **误导性诊断**：文案把"挂死"表述为"正在后台正常运行"，agent 基于错误前提继续决策。

**根因**：Bash 工具 spawn 时未设置 stdin（`terminal.rs` 同步路径与 `async_tasks.rs::spawn_shell` 后台路径均只设 stdout/stderr 为 piped）→ stdin 继承终端 → 读 stdin 的进程（如 JSON-RPC stdio 服务 `node peri-workflow.js --help`）**永不 EOF** → 挂死到超时；超时分支不区分"慢但活跃"与"挂起无进展"，一律 promote。

## 症状详情

- 用户场景：`node ~/.peri/workflow/0.1.1/.../peri-workflow.js --help 2>&1 | head -40`（stdio 服务被当 CLI 调用）挂 30s 超时 → 被 promote 成后台任务，进程永不结束，通知永不完成；旧文案"running as a background task; you will be notified when it completes"是错误承诺。
- 实测对照：stdin 重定向 `/dev/null` 后同一命令 **34ms 快速退出**（readline 立即 EOF），agent 立即看到空输出即可正确推断"命令用法不对"。

## 修复记录

### 修复 #1（2026-08-10）

- **操作人**：agent
- **修复内容**：
  1. **预防根因（stdin 置 null）**：`terminal.rs` 同步路径与 `async_tasks.rs::spawn_shell` 后台路径均加 `.stdin(Stdio::null())`——Bash 工具是非交互执行，读 stdin 的进程立即 EOF 快速失败，错误立刻可见而非挂死到超时。全仓库显式需要 stdin 的 spawn（LSP transport、MCP client、acp-hub child、hooks executor）均显式设 `Stdio::piped()`，无依赖 stdin 继承的调用方，零误伤。
  2. **超时诊断分流**：`terminal.rs` 超时分支新增 `process_status_snapshot(pid)`（`ps -o pid,stat,etime,command`，非 Unix/失败静默降级）附入文案；按**有无部分输出**分流：
     - 有输出 → "was producing output, so it is likely progressing"（续跑合理，行为不变）；
     - 无输出 → 如实说明 "may never complete on its own"，列出可能原因（等待输入/资源、应为 `run_in_background: true` 的服务/守护进程、静默启动的慢任务），并给出 kill 指引——反映不确定性，不再武断承诺完成。
  3. **文档**：`descriptions/bash.md` Platform behavior 补 stdin 重定向 /dev/null 语义（交互命令快速 EOF 失败而非挂死）。
  4. **测试**：`terminal_test.rs` 新增 3 个——`read` 快速 EOF（stdin null）、无输出 promote 文案含 stall 诊断、有输出 promote 文案含 progress 说明；既有 promote 测试断言在新文案下兼容。
- **涉及文件**：`peri-middlewares/src/middleware/terminal.rs`、`peri-middlewares/src/middleware/terminal_test.rs`、`peri-middlewares/src/middleware/descriptions/bash.md`、`peri-agent/src/agent/async_tasks.rs`
- **验证状态**：`cargo test -p peri-middlewares --lib terminal`（26 passed）、`cargo test -p peri-agent --lib agent::async_tasks`（35 passed）、`cargo clippy -p peri-middlewares -p peri-agent --all-targets -- -D warnings` 与 fmt 全绿；用户场景 `/dev/null` 对照实测 34ms 退出。
- **遗留说明**：当前运行中的 agent 进程仍为旧二进制，改动需重启后生效；超时分支保留 promote（避免误杀 cargo build 类静默启动的慢任务），仅文案如实分流。
