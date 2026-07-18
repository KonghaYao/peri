> 归档于 2026-07-18，原路径 spec/issues/2026-07-17-system-note-level-color-not-rendered.md
# SystemNote 的 Warning/Error 等级字体颜色未区分，全部显示为灰色

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-17

## 问题描述

SystemNote 数据结构已定义 `TuiNoteLevel` 三级枚举（Info / Warning / Error），创建 SystemNote 实例时也正确设置了 `level` 字段。但渲染函数 `render_system_note_lines` 完全忽略 `data.level`，改用文本关键词启发式判断颜色，默认 fallback 为 `semantic.text.muted`（灰色）。导致 Warning 和 Error 等级的 system note 在界面上与 Info 无差异，用户难以快速区分通知的严重程度。

## 症状详情

| 场景 | 期望 | 实际 |
|------|------|------|
| BudgetWarning 触发时 | 黄色警告色字体 | 灰色（muted） |
| AgentExecutionFailed 时 | 红色错误色字体 | 灰色（muted） |
| 普通 Info 通知 | 灰色（muted） | 灰色（muted），唯一正确的情况 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 触发任意 Warning 或 Error 级别的 system note（如上下文预算超阈值、agent 执行失败）
  2. 观察消息流中 system note 的字体颜色
- **环境**：所有环境下均必现

## 涉及文件

- `peri-tui/src/kit/message_area/render.rs:442-502` —— `render_system_note_lines` 函数，颜色决策完全基于文本关键词而非 `data.level`
- `peri-tui/src/kit/tui_render_unit.rs:222-227` —— `TuiNoteLevel` 枚举定义，已有 Info/Warning/Error 三级但未被渲染使用
- `peri-tui/src/kit/acp_events.rs:364-420,642-712` —— 多处创建 TuiSystemNote 时已正确设置 level 字段

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-17 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
