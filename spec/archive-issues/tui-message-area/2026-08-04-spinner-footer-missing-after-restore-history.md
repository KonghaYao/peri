> 归档于 2026-08-11，原路径 spec/issues/2026-08-04-spinner-footer-missing-after-restore-history.md

# 恢复历史/空闲后消息区下方 spinner 组件完全消失

**状态**：Fixed
**优先级**：中
**创建日期**：2026-08-04

## 问题描述

消息区下方应**常驻**一个 loading spinner 组件（只有 active/inactive 二态，不存在 hidden 态：idle 时为静止图标占位，agent 工作时转动）。当前实现中，长会话结束、恢复历史（加载历史线程/rewind/session new）后，spinner 组件**整个不渲染**——即使滚动到底部也没有任何内容，消息区下方完全空白。

## 症状详情

- 长会话结束后 footer 正常（显示 `Brewed for ...` summary 行）；一旦恢复历史（切换/加载历史会话），footer 整块消失。
- 全新会话/清空后空闲状态下，footer 同样不渲染。
- 用户期望：idle 时显示静止图标占位（如 `✳`），loading 时动画，组件永远占据消息区下方位置。

## 复现条件

- **复现频率**：必现（特定状态下）
- **触发步骤**：
  1. 进行一个长会话（产生 summary）；
  2. 恢复历史（加载历史线程 / rewind / session new——触发 BRIDGE_RESET_COUNTER 变更，`summary_elapsed_ms` 被清零）；
  3. 此时 `is_loading=false`、`todo_items` 为空、`has_summary=false`；
  4. footer 构建函数直接返回空 `(Vec::new(), None)`，spinner 组件完全不渲染。
- **环境**：所有环境

## 涉及文件

- `peri-tui/src/kit/message_area/footer.rs` —— build_footer_lines 在 `!is_loading && todo_items.is_empty() && !has_summary` 时提前返回空，是组件消失的直接原因
- `peri-tui/src/kit/message_area/mod.rs` —— empty/brewed_lines/welcome 分支对 footer 常驻的适配

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-04 | — | Open | agent | 创建 |
| 2026-08-04 | Open | Fixed | agent | 修复：footer 常驻渲染，idle 静止图标占位 |

## 修复记录

### 修复 #1（2026-08-04）

- **操作人**：agent
- **用户原意**：spinner 组件应常驻消息区下方（active/inactive 二态，不存在 hidden）——idle 时静止图标占位，agent 工作时动画；恢复历史/空闲后组件不应整个消失
- **修复内容**：
  - `peri-tui/src/kit/message_area/footer.rs`：删除 `build_footer_lines` 的 idle 早退（`!is_loading && todo_items.is_empty() && !has_summary` 时返回空）；新增 `render_idle_spinner_line`（固定第一帧 `✳` + muted 色，不参与动画），idle 且无 todo/summary 时渲染该行占位；footer 恒常渲染（保留空行 padding）
  - `peri-tui/src/kit/message_area/footer_test.rs`：新增 `test_render_idle_spinner_line_static_fixed_frame`（固定帧、不随壁钟变化、无 verb/elapsed 后缀）
  - `mod.rs` 无需改动：`footer_visual_rows`/`empty`/`brewed_lines`/`viewport_has_footer` 逻辑对 footer 常驻天然自洽（welcome 下显示静止图标，滚动到底 footer 可见）
- **涉及 commit**：未提交
- **验证状态**：已验证（cargo test -p peri-tui --lib 737 passed；clippy --all-targets -D warnings 通过）
