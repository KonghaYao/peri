> 归档于 2026-07-10，原路径 spec/issues/2026-07-09-agent-prefix-triggers-command-without-slash.md

# 输入 "agent " 开头触发 OpenPanel 命令，无需 / 前缀

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-09

## 问题描述

用户在输入框中输入以 "agent" 开头的文本（如 `agent do something`），按下 Enter 提交后，不应该打开任何面板，但实际行为是打开了 Agent 管理面板。其他 panel 注册的 slash command（如 `model`、`hooks`、`tasks` 等 13 个）同样受影响。

## 根因分析

`panel_registry.rs` 的 `panel_for_slash_command()` 使用 `trim_start_matches('/')` 做归一化：

```rust
pub fn panel_for_slash_command(command: &str) -> Option<PanelKind> {
    let normalized = command.trim_start_matches('/').to_ascii_lowercase();
    // ...
    PANELS.iter().find(|m| m.slash_command == normalized).map(|m| m.kind)
}
```

`trim_start_matches('/')` 在没有 `/` 前缀时是 no-op，导致 `"agent"` 也能匹配 `slash_command: "agent"`。

`submit_request.rs` 的 `parse_submit_request()` 没有检查 command 是否以 `/` 开头就直接调用此函数：

```rust
pub fn parse_submit_request(input: &str) -> Option<SubmitRequest> {
    let command = trimmed.split_whitespace().next().unwrap_or("");
    // ... 其他命令检查 ...
    if let Some(kind) = crate::kit::panel_registry::panel_for_slash_command(command) {
        return Some(SubmitRequest::OpenPanel(kind));  // ← BUG: command 可能没有 /
    }
    Some(SubmitRequest::AgentText(trimmed.to_string()))
}
```

### 为什么其他命令（/clear、/rewind 等）不受影响

`is_clear_command()` 和 `is_rewind_or_undo_command()` 在匹配时严格要求 `/` 前缀：
```rust
fn is_clear_command(command: &str) -> bool {
    matches!(command, "/clear" | "/cls" | "/reset")
}
```

只有 `panel_for_slash_command` 路径缺少这个守卫。

### 为什么 slash popup 不受影响

slash popup 的 `on_select` 回调中也调用 `panel_for_slash_command(&item.insert_text)`，其中 `insert_text` 就是命令名（无 `/`）。但这是 slash popup 内部行为——用户已通过 `/` 触发 popup 并从列表中选择，此调用场景合理。

## 受影响范围

**直接影响的命令**（PANELS 中注册的全部）：

| 输入前缀 | 错误行为 | 可能影响 |
|---------|---------|---------|
| `agent ...` | 打开 Agent 面板 | **高频**——"agent" 是极常见英文词 |
| `model ...` | 打开 Model 面板 | 中频——对话中常提及 model |
| `tasks ...` | 打开 Tasks 面板 | 低频 |
| `hooks ...` | 打开 Hooks 面板 | 低频 |
| `config ...` | 打开 Config 面板 | 低频 |
| `threads ...` | 打开 ThreadBrowser | 低频 |
| `mcp ...` | 打开 MCP 面板 | 低频 |
| `plugin ...` | 打开 Plugin 面板 | 低频 |
| `cron ...` | 打开 Cron 面板 | 低频 |
| `status ...` | 打开 Status 面板 | 中频——"status" 常见 |
| `memory ...` | 打开 Memory 面板 | 中频——"memory" 常见 |
| `betas ...` | 打开 Betas 面板 | 低频 |
| `workflow ...` | 打开 Workflow 面板 | 低频 |
| `login ...` | 打开 Login 面板 | 低频 |

其中 **`agent`** 影响最大——对话中常出现 "agent 帮我..." 这类语句。

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 TUI
  2. 在输入框输入 `agent 请帮我做xxx`（不加 `/`）
  3. 按 Enter
  4. 观察：Agent 管理面板被打开，而非正常发送消息

## 建议方案

在 `parse_submit_request` 中调用 `panel_for_slash_command` 前增加 `/` 前缀检查：

```rust
// submit_request.rs
if command.starts_with('/') {
    if let Some(kind) = crate::kit::panel_registry::panel_for_slash_command(command) {
        return Some(SubmitRequest::OpenPanel(kind));
    }
}
```

**不修改 `panel_for_slash_command` 本身**——因为 slash popup 的 `on_select` 回调传入的是无 `/` 的命令名，该调用场景合理且正常。

### 改动范围

- **1 个文件**：`peri-tui/src/kit/submit_request.rs`
- **约 3 行**：在 `panel_for_slash_command(command)` 调用外包裹 `command.starts_with('/')` 守卫

## 测试要点

1. 输入 "agent help me"（无 /）→ 正常发送 AgentText，不打开面板
2. 输入 "/agent"（有 /）→ 打开 Agent 面板
3. 输入 "model is good"（无 /）→ 正常发送 AgentText
4. 输入 "/model"（有 /）→ 打开 Model 面板
5. 输入 "/clear" → 正常执行 clear
6. 输入 "/compact" → 正常发送 AgentText（pass-through to agent）
7. 输入 "help" → 正常发送 AgentText（虽然 "help" 是 ACP command，但不注册在 PANELS 中，不受影响）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-09 | — | Open | agent | 创建 |
| 2026-07-09 | Open | Fixed | agent | 修复 |

## 修复记录

### 修复 #1（2026-07-09）

- **操作人**：agent
- **用户原意**：输入 "agent " 开头不应触发 OpenPanel 命令
- **修复内容**：`submit_request.rs:64` —— 在 `panel_for_slash_command(command)` 调用外包裹 `command.starts_with('/')` 守卫（3 行）
- **涉及文件**：`peri-tui/src/kit/submit_request.rs`
- **验证状态**：待验证
