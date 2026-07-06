# Slash 弹窗重复命令清理 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 从 `build_available_commands()` 中删除 7 条与 PANELS 重合的硬编码命令，消除 slash 弹窗中的重复条目。

**Architecture:** `build_available_commands()` 是 ACP 层的命令注册器，它硬编码了 22 条内置命令 + skills 动态追加。其中 model/hooks/mcp/plugin/cron/memory/login 共 7 条与 PANELS 注册表重复，应删除这些残留代码。PANELS 继续作为这些命令的唯一来源。

**Tech Stack:** Rust, peri-acp crate, peri-tui crate

**Background:** `build_slash_items()` (input_area.rs:967) 合并 PANELS + `AVAILABLE_SLASH_COMMANDS` 双源构建 slash 弹窗条目。`AVAILABLE_SLASH_COMMANDS` atom 由 ACP `AvailableCommandsUpdate` 通知写入，其数据来自 `build_available_commands()`。两个来源中存在 7 条同名命令导致弹窗重复。

**重名验证：**
| ACP 命令 | 精确匹配 PANELS? | 
|----------|-----------------|
| "model" | ✓ PANELS[Model].slash_command = "model" |
| "hooks" | ✓ PANELS[Hooks].slash_command = "hooks" |
| "mcp" | ✓ PANELS[Mcp].slash_command = "mcp" |
| "plugin" | ✓ PANELS[Plugin].slash_command = "plugin" |
| "cron" | ✓ PANELS[Cron].slash_command = "cron" |
| "memory" | ✓ PANELS[Memory].slash_command = "memory" |
| "login" | ✓ PANELS[Login].slash_command = "login" |
| "agents" | ✗ PANELS[Agent].slash_command = "agent"（不同，不重复） |

---

### Task 1: 删除 `build_available_commands()` 中 7 条重复命令

**Files:**
- Modify: `peri-acp/src/dispatch/commands.rs:8-33`

- [ ] **Step 1: 删除 7 条重复行**

删除第 18, 23, 24, 25, 26, 28, 29 行的 `AvailableCommand::new(...)` 条目。

修改前（lines 18-29）：

```rust
        AvailableCommand::new("model", "Switch the current LLM model"),
        AvailableCommand::new("mode", "Switch the current permission mode"),
        AvailableCommand::new("effort", "Configure LLM reasoning/thinking effort"),
        AvailableCommand::new("loop", "Control agent iteration loop"),
        AvailableCommand::new("history", "View and resume previous conversations"),
        AvailableCommand::new("mcp", "Manage MCP (Model Context Protocol) servers"),
        AvailableCommand::new("hooks", "Manage Claude Code hooks"),
        AvailableCommand::new("plugin", "Manage installed plugins"),
        AvailableCommand::new("cron", "Manage scheduled/cron tasks"),
        AvailableCommand::new("agents", "Manage sub-agent definitions"),
        AvailableCommand::new("memory", "Manage persistent memory entries"),
        AvailableCommand::new("login", "Configure authentication"),
```

修改后：

```rust
        AvailableCommand::new("mode", "Switch the current permission mode"),
        AvailableCommand::new("effort", "Configure LLM reasoning/thinking effort"),
        AvailableCommand::new("loop", "Control agent iteration loop"),
        AvailableCommand::new("history", "View and resume previous conversations"),
        AvailableCommand::new("agents", "Manage sub-agent definitions"),
        AvailableCommand::new("rename", "Rename the current session"),
```

保留不变的行（不在面板命令集中）：help, clear, compact, context, cost, mode, effort, loop, history, agents, rename, lang, exit + skills 动态追加。

**注意**：`"agents"` 保留——PANELS 的 slash_command 是 `"agent"`（单数），两者不同，不会重复。

- [ ] **Step 2: 运行现有测试确认失败**

```bash
cargo test -p peri-acp --lib -- dispatch::commands_test
```

预期结果：`test_build_available_commands_includes_builtins` 失败——assert `cmds.len() >= 20` 不成立（现在只剩 15 条内置命令），且 `names.contains(&"model")` 失败。

- [ ] **Step 3: 更新测试断言**

更新 `peri-acp/src/dispatch/commands_test.rs:8-18`：

```rust
#[test]
fn test_build_available_commands_includes_builtins() {
    let cmds = build_available_commands(&[]);
    // 15 条内置命令（删除 7 条与 PANELS 重复的）
    assert!(cmds.len() >= 15, "至少 15 条内置命令，实际: {}", cmds.len());
    // 验证关键命令存在
    let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"help"), "help 命令应存在");
    assert!(names.contains(&"clear"), "clear 命令应存在");
    assert!(names.contains(&"compact"), "compact 命令应存在");
    assert!(names.contains(&"agents"), "agents 命令应存在");
}
```

注意：`"model"` 断言替换为 `"agents"`，因为 model 已删除，agents 保留且有意义。

- [ ] **Step 4: 运行所有相关测试**

```bash
cargo test -p peri-acp --lib -- dispatch::commands_test
```

预期：全部 3 个测试 PASS。

- [ ] **Step 5: 提交**

```bash
git add peri-acp/src/dispatch/commands.rs peri-acp/src/dispatch/commands_test.rs
git commit -m "fix: remove 7 duplicate panel commands from build_available_commands

Slash popup showed duplicate entries (orange from PANELS, gray from ACP)
for model/hooks/mcp/plugin/cron/memory/login. These commands are already
registered in PANELS registry — the ACP hardcoded copies were residual.

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```
