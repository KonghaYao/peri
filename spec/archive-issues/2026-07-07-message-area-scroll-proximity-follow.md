> 归档于 2026-07-18，原路径 spec/issues/2026-07-07-message-area-scroll-proximity-follow.md
# 消息区自动吸底应基于滚动位置就近判断，而非二元开关

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-07

## 问题描述

当前消息区的自动吸底跟随采用二元 `auto_scroll: bool` 开关：用户**任何**滚动/点击/快捷键操作都会将其置为 `false`，整轮后续内容不再自动跟随。用户稍一往上滚（即使只滚了 1 行），就失去了自动跟随能力——想看一点历史就回不去，除非等下一轮开始。

期望改为**就近判断**：用户视口在底部附近时自动跟随内容增长而滚底，往上滚出一定距离后停止吸底；若用户再自行滚回底附近，恢复跟随。

## 症状详情

| 维度 | 当前行为 | 期望行为 |
|------|---------|---------|
| 用户滚 1 行上 | `auto_scroll = false`，整轮不再跟随 | 仅滚 1 行，基本还在底部，应继续跟随 |
| 用户回看上轮内容 | 停在回看位置，不跟随 | 不跟随——回看语义正确 |
| 用户在底部附近 | 滚过一次后就不会自动跟了 | 只要在底部附近就应继续跟 |
| 用户滚回底部 | 不会恢复跟随 | 应恢复跟随（回到底部附近即重新吸底） |
| 新轮次开始 | 无条件重置 `auto_scroll = true` | 同上逻辑：若用户当时就在底部则跟，否则不抢 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 TUI，发送 prompt 等待流式输出
  2. 在流式输出期间按 `Ctrl+Up` 向上滚动任意行数（包括仅 1 行）
  3. 观察后续流式 chunk 是否自动滚到底部
  4. **实际**：不再跟随到最底，即使后续滚回底部也不恢复
  5. **期望**：仅当用户向上滚出视口 50% 后才停止跟随；若又滚回底部附近则恢复跟随
- **环境**：所有终端 / 所有 OS

## 期望改进方向

将 `auto_scroll: bool` 二元开关改为基于**滚动位置与底部距离**的就近判断：

### 核心算法

```
每帧内容变化时：
  distance_to_bottom = total_visual_rows - scroll_y - vis_height
  threshold = max(vis_height / 2, 5)   // 视口高度 50%，最少 5 行

  if distance_to_bottom <= threshold:
      scroll_to_bottom()   // 在底部附近 → 跟随
  else:
      不做任何滚动操作     // 用户在看历史 → 不抢
```

**不需要 `auto_scroll` flag**：是否跟随完全由当前滚动位置决定。用户滚回底部附近 → 自动恢复跟随；滚上去 → 自然停止跟随。

### 对比当前方案的优势

| 维度 | 当前（二元 flag） | 提议（就近判断） |
|------|------------------|------------------|
| 用户滚 1 行 | 全程失去跟随 | 仍在底部区域内，继续跟随 |
| 用户滚很多行回看历史 | 失去跟随 ✓ | 不在底部区域，不跟随 ✓ |
| 用户滚回底部 | 不恢复跟随 ✗ | 自动恢复跟随 ✓ |
| 新轮次开始 | 无条件重置 flag | 若用户已滚上去则不抢 ✓ |
| 需要额外状态 | `auto_scroll` + `had_ct` | 仅需 scroll 位置本身 |

### 实现位置

`peri-tui/src/kit/message_area.rs` 中 `use_effect` 闭包（第 529-545 行）——将条件从 `if a.get()` 改为 `if distance_to_bottom <= threshold`，即可删除 `auto_scroll` 和 `had_ct` 两个 `use_state`。

### 注意事项

- `total_visual_rows` 和 `vis_height` 需在 `use_effect` 内从 `scroll_state` 和 `area_rect`（或最后一个 `wrap_map` 条目）获取
- `scroll_state.read().offset().y` 在 `use_effect` 内读取当前值——`use_effect` 不捕获闭包外 snapshot，每次执行都用最新值
- 阈值 `vis_height / 2` 确保用户在底部半屏内时跟随——这恰好符合「视口 50% 以内自动，往上走就不吸」

## 涉及文件

- `peri-tui/src/kit/message_area.rs` —— 第 274 行 `auto_scroll` / 第 275 行 `had_ct` / 第 529~545 行 `use_effect`、以及事件 handler 中多处 `auto_scroll.set(false)`

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-07 | — | Open | agent | 创建（issue-create skill） |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加）
