# TUI Slash 命令补全弹窗缺少 Skills 条目


> 归档于 2026-07-20，原路径 spec/issues/2026-07-03-slash-popup-missing-skills.md
**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-03

## 问题描述

在 TUI 输入区输入 `/` 触发 slash 命令补全弹窗时，弹窗中只显示了面板命令（/agent、/model 等）和 4 个 ACP 内置命令（/bg、/clear、/compact、/rewind），不包含任何已注册的 skill（如 /archify、/code-review 等）。用户无法通过补全弹窗发现和选择 skill，必须手动输入完整名称。

同时，ACP 层的 `build_available_commands()` 已经正确包含了 skills——它通过 `AvailableCommandsUpdate` 通知将完整命令列表（含 skills）推送给 ACP 客户端。但 TUI 的 slash 补全弹窗没有消费这份数据。

## 症状详情

| 维度 | 观察 |
|------|------|
| 触发方式 | TUI 输入区输入 `/`，弹窗出现 |
| 期望 | 弹窗中能看到已注册的 skill 名称（如 /archify、/code-review） |
| 实际 | 弹窗只有面板命令 + 4 个 ACP 命令，无 skill 条目 |
| 手动输入 | 手动输入 `/archify` 仍能正常工作（ACP 端会处理） |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 Peri TUI（确保有 skill 在 `.claude/skills/` 中注册）
  2. 在输入框中输入 `/`
  3. slash 补全弹窗弹出，但列表中无 skill 条目
- **环境**：任意

## 涉及文件

- `peri-tui/src/kit/input_area.rs` —— slash 补全弹窗数据源（`REMOTE_SLASH_COMMANDS` + `PANELS`），当前硬编码 4 条 ACP 命令，未纳入 skill
- `peri-acp/src/dispatch/commands.rs` —— ACP 端 `build_available_commands()` 已正确包含 skills，通过 AvailableCommandsUpdate 通知下发

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-03 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
