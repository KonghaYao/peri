> 归档于 2026-08-11，原路径 spec/issues/2026-08-02-session-title-stale-after-session-cleared.md

# 会话关闭（session id 置空）后状态栏标题残留上个会话

**状态**：Fixed
**优先级**：低
**创建日期**：2026-08-02

## 问题描述

`service_snapshot` 的会话标题刷新逻辑有 `!session_id.is_empty()` 守卫：active session 被清空（会话关闭）时整个块被跳过，`slow.current_title` 保留旧值，`CURRENT_SESSION_TITLE` 持续显示已关闭会话的标题。该 atom 全仓仅此一处写入，不会被其他路径清空。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- `service_snapshot.rs` 约 232-247 行：`if (session_changed || now >= next_title_refresh) && !session_id.is_empty()`。
- session_id 由非空变为空时 `session_changed = true`，但守卫短路，标题缓存不更新。
- `CURRENT_SESSION_TITLE` 仅在此处写入（`write_if_changed`，约 247 行），无其他清理路径。
- 状态栏/标题区域持续显示已关闭会话的标题。

## 复现条件

- **复现频率**：必现（关闭活动会话时）
- **触发步骤**：
  1. 打开一个会话，等待标题被缓存
  2. 关闭会话（session id 清空）
  3. 观察状态栏标题仍为旧会话标题
- **环境**：任意关闭会话路径

## 期望改进方向

- session_id 为空时主动清空 `current_title` 与 `current_title_session_id` 缓存，再走刷新分支。

## 涉及文件

- `peri-tui/src/kit/service_snapshot.rs` —— 标题刷新块（约 229-247 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: session_id 清空时主动清空标题缓存并写入空标题 |
| 2026-08-11 | Fixed | Fixed | agent | 终态确认归档：session_id 置空时清空标题缓存并写空标题（service_snapshot.rs），修复记录见正文 |

## 修复记录

- 改动：`peri-tui/src/kit/service_snapshot.rs` 标题刷新块——拆出 `session_changed && session_id.is_empty()` 分支：清空 `slow.current_title` 与 `slow.current_title_session_id` 并推进 `next_title_refresh`；随后 `write_if_changed` 将空标题写入 `CURRENT_SESSION_TITLE`，会话关闭后标题不再残留。
- 验证：`cargo check -p peri-tui --all-targets` 通过（7.61s，无警告）。
