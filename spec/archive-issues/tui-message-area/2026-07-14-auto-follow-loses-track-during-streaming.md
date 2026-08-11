> 归档于 2026-08-11，原路径 spec/issues/2026-07-14-auto-follow-loses-track-during-streaming.md

# 流式输出期间自动跟踪中断，滚动停在中间不再跟随

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-14
**类型**：Bug

## 问题描述

agent 流式输出 token 期间，消息区本应自动跟随到底部。但当一次更新中内容**突发性大量增长**（一次性推送很多 token，`total_visual_rows` 跳跃很大）时，`scroll_y`（旧底部）与 `max_scroll`（新底部）之间的 `distance` 会超过 `vis_height / 4` 阈值，proximity guard 误判为用户主动上滚而停止跟随——即使用户**完全没有任何操作**。

**期望行为**：流式输出期间，只要用户没有主动上滚脱离底部，滚动条应该始终跟随内容增长自动滚到底部。

## 症状详情

| 维度 | 观察 |
|------|------|
| 触发场景 | agent 流式输出 token 期间，某次更新中内容一次性大量增长（批量推送） |
| 用户操作 | 无任何操作（未滚动、未点击、未按键） |
| 实际表现 | 内容突发增长后，`distance = max_scroll - scroll_y` 超过 `vis_height / 4` 阈值，proximity guard 停止跟随，滚动停在中间 |
| 期望表现 | 内容突发增长时，也应自动跟随到底部（用户没主动上滚，不应被阈值拦截） |
| 后续状态 | 被甩开后，即使后续内容继续增长，滚动条也不再恢复跟随 |
| 复现频率 | 高频（取决于模型输出速度：输出越快/批量越大，越容易触发） |

## 复现条件

- **复现频率**：疑似高频复现
- **复现条件**：模型输出速度越快 / 单次更新内容越多，越容易触发。慢速逐 token 流式通常不受影响。
- **触发步骤**：
  1. 启动 TUI，发送一个会引发大量快速输出的 prompt（如让 agent 输出完整代码文件）
  2. 不做任何滚动/点击/键盘操作
  3. 观察：当模型一次性推送较多 token（`total_visual_rows` 单次跳跃 ≥ `vis_height / 4`），滚动条停住不再跟随
  4. 模型继续输出但滚动条停在原位置，需手动 `Ctrl+Down` 才追上
- **环境**：macOS，所有终端

## 涉及文件

- `peri-tui/src/kit/message_area/scroll.rs:590-610` —— `run_auto_follow` 的 `is_loading` 分支：通过 `total_visual_rows > prev_lsa` 哨兵 + `distance <= threshold` 就近判断决定是否 `scroll_to_bottom()`
- `peri-tui/src/kit/message_area/scroll.rs:498-639` —— `run_auto_follow` 完整逻辑（含 submit/history replay/loading/non-loading/shrink 五个分支）
- `peri-tui/src/kit/message_area/mod.rs:282-312` —— `use_effect` 依赖项 `(items_len, vm_generation, is_loading, total_visual_rows)` 及 `AutoFollowCtx` 构造
- `peri-tui/src/kit/message_area/mod.rs:225-235` —— `total_visual_rows`（`core_total_visual_rows`）的计算：各 VM slot 的 `visual_rows` 之和

## 关联历史

- `spec/issues/2026-07-07-message-area-scroll-proximity-follow.md`（Open）：将自动跟踪从二元开关改为就近判断的设计提案——当前 loading 分支已经实现了 proximity guard（`vis_height / 4`），但可能仍有导致"甩开"的遗漏路径
- `spec/issues/2026-07-12-message-area-scrollbar-not-reaching-bottom.md`（Fixed）：涉及滚动条 thumb 不到底 + 宽度变化后滚动失效等——修复的是不同的问题
- `spec/issues/2026-07-13-submit-no-scroll-to-bottom.md`（Open）：用户发送 prompt 后不跳到底部——提交瞬间的问题，本 issue 是流式期间的问题

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-14 | — | Open | deepseek-v4-pro | 创建（issue-create skill） |
| 2026-08-11 | Open | Fixed | agent | 归档：75abcdcf 粘性吸底（follow_bottom），流式跳增不再误判上滚 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加）
