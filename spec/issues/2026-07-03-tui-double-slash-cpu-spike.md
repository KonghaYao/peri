# TUI 输入区输入 // 导致 CPU 持续高负载

**状态**：Verified
**优先级**：中
**创建日期**：2026-07-03

## 问题描述

在 TUI 输入区输入 `//`（连续两个斜杠）后，CPU 使用率飙高并持续不降。无需执行任何命令，仅在输入框中键入 `//` 即可触发。slash popup 关闭后 CPU 依然保持高位。

## 症状详情

| 维度 | 观察 |
|------|------|
| 触发方式 | 在 TUI 输入区连续输入两个 `/` 字符 |
| CPU 行为 | 飙高后持续不降（非短暂脉冲） |
| 是否需要执行 | 否，仅输入字符即触发 |
| 输入单 `/` 时 | slash popup 正常弹出，CPU 表现正常 |
| 输入 `//` 时 | popup 关闭（符合预期），但 CPU 随即飙高 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 Peri TUI
  2. 在输入框中输入 `/`（slash popup 弹出）
  3. 再输入 `/`（文本变为 `//`，slash popup 关闭）
  4. CPU 即刻飙高并持续
- **环境**：macOS，arm64，peri-tui

## 涉及文件

- `peri-tui/src/kit/input_area.rs` —— 输入区组件，含 `update_popup_prefix()` 决定 slash popup 激活/关闭逻辑（第 751-778 行），每次字符输入/删除/paste 都调用该函数更新 atom 状态
- `peri-tui/src/kit/slash_completion.rs` —— Slash 命令补全弹窗组件，根据 `SLASH_HINT_ACTIVE` atom 决定是否渲染

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-03 | — | Open | agent | 创建 |
| 2026-07-03 | Open | Verified | agent | 修复 + 用户验证通过 |

## 修复记录

### 修复 #1（2026-07-03）

- **操作人**：agent
- **用户原意**：修复 TUI 输入区输入 `//` 后 CPU 持续高负载的问题
- **修复内容**：移除 `SlashCompletion` 组件 render body 中的 `SLASH_SELECTED_INDEX` atom 写入调用。事件处理器（`next_selection`/`previous_selection`）已通过 `saturating_sub`/`min` 保持边界安全，render 期间仅用 `clamp_selection` 做只读裁剪显示，无需回写 atom。render body 写 atom 在 `slash_active` 从 `true→false` 过渡时（输入 `//` 触发）会与组件卸载生命周期交互，引发级联重渲染导致 CPU 100%。
- **涉及文件**：`peri-tui/src/kit/slash_completion.rs`
- **验证状态**：已验证

### 验证 #1（2026-07-03）—— 通过

用户验证通过，输入 `//` 后 CPU 不再飙高，问题已解决。
