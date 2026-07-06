# Slash 命令弹窗中 panel 命令重复显示（橙色 + 灰色各一条）

**状态**：Open
**优先级**：中
**创建日期**：2026-07-06

## 问题描述

`build_available_commands()` 在 `dispatch/commands.rs` 中硬编码了 model、hooks、plugin、mcp、cron、agents、memory、login 等命令。这些命令本就由 PANELS 注册表提供顶层入口，属于残留代码。由于 `build_slash_items()` 合并了 PANELS + `AVAILABLE_SLASH_COMMANDS` 双源，导致 slash 弹窗中每条这类命令出现两次——一条橙色（PANELS）、一条灰色（ACP 残留）。

## 症状详情

| 命令 | 应当保留的来源 | 误打入的残留代码 |
|------|-------------|-------------|
| `/model` | PANELS[Model] | `build_available_commands()` 硬编码，冗余 |
| `/hooks` | PANELS[Hooks] | 同上 |
| `/plugin` | PANELS[Plugin] | 同上 |
| `/mcp` | PANELS[MCP] | 同上 |
| `/cron` | PANELS[Cron] | 同上 |
| `/agents` | PANELS[Agent] | 同上 |
| `/memory` | PANELS[Memory] | 同上 |
| `/login` | PANELS[Login] | 同上 |

其他命令（help、clear、compact、context、cost、mode、effort、loop、history、rename、lang、exit）以及 skills 来源的命令没有对应 Panel，不受影响，仅出现一条。

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 TUI
  2. 在输入框输入 `/`
  3. 观察 slash 命令补全弹窗——上述命令出现两条，一条橙色一条灰色

## 涉及文件

- `peri-tui/src/kit/input_area.rs:967-999` —— `build_slash_items()`：PANELS + `AVAILABLE_SLASH_COMMANDS` 双源合并，未去重
- `peri-acp/src/dispatch/commands.rs:8-41` —— `build_available_commands()`：残留硬编码了 8 条与 PANELS 冲突的命令（model/hooks/plugin/mcp/cron/agents/memory/login），这些应由 PANELS 注册表统一管理

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-06 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
