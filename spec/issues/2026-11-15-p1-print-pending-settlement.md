# P1：print 模式在 pendingTasks>0 时仍报告 completed

**状态**：已确认，待修复。
**优先级**：P1。
**类型**：可观测性 / 生命周期错误（false success）。
**创建日期**：2026-11-15。
**来源**：DeepSWE 11-12 评测 P4a 固定响应实验（GitHub #402）。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `824559113`）。

## 确认证据

- P4a 真实二进制实验（固定响应，仅缩短等待窗口）：
  - `logs-scenario1.log`：`pending=1 status=completed exit=0`。
  - `logs-scenario3.log`：20 次截止竞争，18 行 `pending=1` 仍 `completed/exit=0`。
  - `logs-scenario4.log`：`pending=1 status=completed exit=0`。
  - `logs-scenario4b.log`：close error 时 `exit=1`，但 `status=completed`。
  - `logs-scenario2.log` / `logs-scenario5.log`：窗口内完成、嵌套子任务 `pending=null`，`completed/exit0`（正确路径）。
- 代码路径（基线 `824559113`）：
  - `peri-agent/src/session/exec/executor.rs:484-485` 按 session `TaskManager.active_count()` 填 `pending_tasks`。
  - `peri-acp/src/host/prompt.rs:118-124` 在 `pending>0` 时写 `_meta.peri.pendingTasks`。
  - `peri-tui/src/cli_print.rs:235-236` 只取 `response.stop_reason`，丢弃 `response.meta`。
  - `peri-tui/src/cli_print.rs:555-561` 的 `completed` 只看 `stop_reason==EndTurn`；`291-296` 同判据。
  - `peri-tui/src/cli_print.rs:259-263` 的 close 失败只产生 `cleanup_error`/`is_error`，`status` 不变；`cleanup_result?` 已使 `exit=1`。

## 根因链

后台任务仍 active → ReAct loop 退出时 `pending_tasks=n>0`（观察） → ACP 响应带 `_meta.peri.pendingTasks=n`（观察） → print 客户端不读 meta，把 `EndTurn` 映射为 `completed/exit0`（观察） → 调用方误判成功、后台结果未被消费（用户可见后果）。
close 失败时退出码已为 1，但 `status` 仍为 `completed`（观察）；这是同一投影判据的第二个缺口。

## 影响面

- 用户影响：`peri -p` / 自动化调用把未结算运行当成功；后台结果丢失且无机器可读的 pending 标记。
- 受影响模块/调用点：`peri-tui/src/cli_print.rs:235-236,259-263,291-296,555-561`。
- 是否可规避：人工检查 `_meta.pendingTasks` 或日志；无自动规避。

## 修复方案

- 范围：
  1. 从 `PromptResponse.meta` 读取 `peri.pendingTasks`（对象嵌套，不是点号 key）。
  2. `PrintOutput::result` 的 `completed` 纳入 `pending==0 && cleanup_error.is_none()`；`status` 在 pending/cleanup 失败时为 `incomplete`；JSON 增加 `pending_tasks`。
  3. `require_completed` 在 `pending>0` 时返回非零。
  4. 保持 `peri-acp` / `peri-acp-types` 不变（meta 已正确生成）。
- 明确不做：不改 `HANDOFF_MAX_WAIT`、Bash 120s、输出上限、TUI 行为、服务端 meta；不做提示词/thinking/版本实验。
- 回归测试：新增/搬运 `peri-tui/tests/print_settlement.rs` 五场景；更新 `cli_print_usage_test.rs` 的签名调用点。测试可用 `test-support` 缩短等待窗口；生产默认不变。

## 验收标准

1. `pending=1` → `status=incomplete`、`exit!=0`、结果 JSON `pending_tasks=1`。
2. `pending=null` → `status=completed`、`exit=0`。
3. close error → `status=incomplete`、`exit!=0`。
4. 截止竞争：`pending==1 ⇔ status=incomplete && exit!=0`。
5. 嵌套子任务：父子完成后 `pending=null`、`completed/exit0`。
6. 既有 `print_exit` / `print_bare` / `cli_print_usage_test` 通过。

## 验证记录

- 待修复完成后回填。
