# 待发送队列下方输入框边线出现间隙

**状态**：Fixed
**优先级**：低
**创建日期**：2026-09-16

## 问题描述

用户截图显示待发送队列下方的输入框边线左侧出现 `─ ─ ─ ───`，期望连续实线。

## 症状详情与复现条件

中文 TUI，已有对话运行时继续提交 `dd` 进入待发送队列。真实 tmux 测试复现前三个宽字符尾列出现空隙；单独渲染 Block 的内存测试不能暴露此故障。

## 涉及文件

- `peri-tui/src/kit/input_area.rs`
- `e2e/tests/smoke/steer-queue-live.test.ts`

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-09-16 | — | Open | agent | 用户截图报告 |
| 2026-09-16 | Open | Fixed | agent | 修复样式覆盖，真实终端回归通过 |

## 修复记录

### 修复 #1（2026-09-16）

- **操作人**：agent
- **用户原意**：queue 下方边线应连续显示为实线。
- **修复内容**：直接通过 widget 渲染 composer Paragraph，保留已经设置的主题背景；原 Text 组件重新设置 Paragraph style，导致显式背景丢失。无需额外 Clear 或强制逐帧刷新。
- **验证证据**：新增真实终端测试在修复前 exit 1，实际捕获 `─ ─ ─ ───`；修复后目标测试通过。完整队列文件 `npm run e2e -- --file tests/smoke/steer-queue-live.test.ts --serial --retry 0` 首轮通过、无重试（exit 0）；`cargo test -p peri-tui --lib -- kit::input_area` 43 项通过（exit 0）；`cargo build -p peri-tui` 通过（exit 0，已有 compact unwind 链接警告）。
- **验证状态**：自动验证通过，待用户终端确认。
