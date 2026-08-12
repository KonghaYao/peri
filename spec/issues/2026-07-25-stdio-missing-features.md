# cli_print 四个 CLI 参数被丢弃 & acp_stdio 缺失 12 个 ACP 方法

**状态**：Partial
**优先级**：高
**创建日期**：2026-07-25
**类型**：Bug — Print/Stdio 模式
**最后核查**：2026-08-11

## 最新情况（2026-08-11）

问题 2 已解决：acp_stdio 缺失的 12 个 ACP 方法已实现（命令面见 `peri-acp/src/host/stdio/` 的 commands/context/freeze/model/session）。问题 1 仍在：`cli_print.rs:91` 仍 `let _ = (effort_override, max_turns, allowed_tools, disallowed_tools)` 丢弃四个 CLI 参数。状态记录（2026-08-11）：Open → Partial。

## 问题描述

`peri` 的 print/stdio 模式存在两处独立但均影响功能完整性的问题：

### 问题 1：cli_print 四个 CLI 参数被解析后丢弃

**位置**：`peri-tui/src/cli_print.rs:78`

`run_print()` 函数接收了 `--effort`、`--max-turns`、`--allowedTools`、`--disallowedTools` 四个参数，但第 78 行立即用 `let _` 丢弃：

```rust
let _ = (effort_override, max_turns, allowed_tools, disallowed_tools);
```

**影响**：用户在 print 模式下通过 CLI 指定的推理强度、最大轮次、工具白名单/黑名单静默失效。

### 问题 2：acp_stdio 缺失 12 个 ACP 方法

| 方法 | acp_server（TUI） | acp_stdio（stdio） |
|---|---|---|
| `workflow/list_runs` | ✅ | ❌ |
| `workflow/kill_agent` | ✅ | ❌ |
| `workflow/kill_run` | ✅ | ❌ |
| `workflow/resume` | ✅ | ❌ |
| `session/cancel-bg-task` | ✅ | ❌ |
| `session/rename` | ✅ | ❌ |
| `plugin/install` | ✅ | ❌ |
| `plugin/uninstall` | ✅ | ❌ |
| `plugin/toggle` | ✅ | ❌ |
| `plugin/search` | ✅ | ❌ |
| `plugin/update` | ✅ | ❌ |
| `marketplace/refresh` | ✅ | ❌ |

**影响**：IDE 客户端走 stdio 通道时无法使用工作流管理、后台任务取消、会话重命名、插件管理、marketplace 功能。

## 涉及文件

- `peri-tui/src/cli_print.rs:78` —— `let _` 丢弃参数行
- `peri-tui/src/acp_stdio/mod.rs` —— 需新增 12 个 ACP handler
- `peri-tui/src/acp_server/requests.rs` —— 参考实现

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-25 | — | Open | agent | 创建 |
| 2026-08-11 | Open | Partial | agent | 12 个 ACP 方法已实现（peri-acp/src/host/stdio/）；cli_print.rs:91 仍丢弃 4 个 CLI 参数，见「最新情况」 |
