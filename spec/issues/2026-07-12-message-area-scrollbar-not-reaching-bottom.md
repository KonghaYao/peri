# 消息区滚动到底部时滚动条未抵达底部（偏差可达视口一半以上）+ 滚动条异常吸底

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-12

## 问题描述

消息区滚动到最底部时，视口右侧的滚动条 thumb（滑块）没有贴到底部位置，而是停在距底部约**视口高度一半甚至更多**的位置。用户感知是"内容已经到底了，但滚动条看起来像只滚到一半"，给人内容还有很多的错觉。

实际内容确实已经到底（用户确认），但滚动条位置错误地暗示内容只滚动了一半。用户怀疑是滚动条 `content_length` 或 `viewport_length` 的行数估算出现较大偏差。

此外，还存在"滚动条异常吸底"现象——滚动条似乎被强制吸附在底部位置，用户尝试向上滚动浏览历史内容时被立刻拉回底部（具体触发条件见下方复现章节 B）。两个现象可能在同一根因下表现，也可能各自独立。

## 症状详情

### 现象 A：滚动条 thumb 未抵达底部

| 场景 | 现象 |
|------|------|
| 滚动到内容最底部 | 滚动条 thumb 距离底部约视口一半甚至更多，没有贴底 |
| 滚动条位置给人的错觉 | "内容只滚动了一半"，但实际内容已全部展示到底 |
| 偏差量级 | 视口高度一半以上（非 1-3 行的小偏差） |
| 是否影响内容展示 | 否——内容正确到底，仅滚动条 thumb 位置不对 |

### 现象 B：滚动条异常吸底（2026-07-12 追加）

| 场景 | 现象 |
|------|------|
| 用户主动向上滚动浏览历史 | 滚动条立刻被吸回底部，无法稳定停留在中间位置 |
| 视觉表现 | 滚动条像被磁铁吸住一样总贴在底部 |
| 是否影响内容展示 | 是——用户无法有效浏览历史消息 |

## 复现条件

### A. 滚动条 thumb 未抵达底部

- **复现频率**：必现
- **触发步骤**：
  1. 启动 TUI，进行多轮对话使消息区进入可滚动状态
  2. 滚动到内容最底部（鼠标滚轮 / 键盘下键 / `scroll_to_bottom`）
  3. 观察右侧滚动条 thumb 的位置——距底部有较明显空隙
- **环境**：所有平台；本次提交 `7ca2a632` 后首次引入独立 `ScrollbarHook`

### B. 滚动条异常吸底

- **复现频率**：待用户补充（疑似必现，但不排除仅在特定条件下，例如 agent loading 期间流式输出 / 新消息到达时）
- **触发步骤**（初步）：
  1. 启动 TUI，进行多轮对话产生足够长的历史消息
  2. 用鼠标滚轮 / 键盘上键尝试向上滚动浏览历史
  3. 观察滚动条是否被立刻吸回底部
- **待用户补充**：是否仅在 agent 流式输出期间出现？非 loading 状态下用户主动上滚能否稳定停留？

## 关联历史

- `spec/issues/2026-07-06-message-area-bottom-blank-at-scroll-end.md`（Open）描述的是"内容下方留白"，与本次"滚动条 thumb 本身没到底"是不同问题
- `spec/issues/2026-07-07-message-area-scrollbar-interaction.md`（Open）描述的是"滚动条缺少拖拽 / 箭头点击 / 刻度标记"，与本 issue 不重叠
- `spec/issues/2026-07-07-message-area-scroll-proximity-follow.md`（Open）描述智能跟随相关行为，可能与现象 B 有关联

## 涉及文件

- `peri-tui/src/kit/message_area.rs` —— `ScrollbarHook` / `ScrollbarFields`：`post_component_draw` 时基于 `content_length` / `position` / `viewport_length` 渲染 `ratatui::widgets::Scrollbar`，行数估算来自 `total_visual_rows` 和 `vis_height`（现象 A 相关）
- `peri-tui/src/kit/message_area.rs` —— `total_rows_cache` 相关计算：`core_total_visual_rows` 来自 `wrap_map_cache` 最后一个 entry 的 `visual_end`（现象 A 相关）
- `peri-tui/src/kit/message_area.rs` —— 智能跟随 `use_effect` 中的 `scroll_to_bottom` 调用与 `last_scrolled_at` 状态（现象 B 相关）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-12 | — | Open | agent | 创建 |
| 2026-07-12 | Open | Fixed | agent | 修复 A + B（见下） |

## 修复记录

### 现象 A（滚动条 thumb 未抵达底部）根因

`peri-tui/src/kit/message_area/mod.rs` 的 `vis_width` 用 `area.width - 1` 给
`Paragraph::line_count(vis_width)` 计算 `total_visual_rows`（即滚动条 `content_length`），
但主渲染分支的 `View` 用 `Constraint::Fill(1)` 占满 `area.width`，Paragraph **实际**
wrap 宽度 = `area.width`。

→ 估算的 `content_length` 偏大（更窄宽度需要更多视觉行），但视口实际只渲染 `area.width`
列的内容——滚动条 thumb 永远在底部之上，看起来"没到底"。

### 修复 A

把主渲染分支 `View` 的 `width` 从 `Constraint::Fill(1)` 改为 `Constraint::Max(vis_width)`
（`mod.rs:376-388`），让 Paragraph 实际 wrap 宽度 = `vis_width`，与 `line_count(vis_width)`
的估算完全一致。Scrollbar 在 `area` 最右 1 列渲染，View 占 `vis_width` 列，两者不重叠。

### 现象 B（滚动条异常吸底）根因

`peri-tui/src/kit/message_area/scroll.rs::run_auto_follow` 的 `is_loading` 分支：
只要 `scroll_y < max_scroll` 就 `scroll_to_bottom()`，**完全忽略用户是否主动上滚**。
用户在 loading 期间向上滚动浏览历史时被立刻吸回底部。

### 修复 B

在 `is_loading` 分支加距离阈值（`scroll.rs:284-298`）：仅当
`distance = max_scroll - scroll_y <= (vis_height / 4).max(5)` 时才 `scroll_to_bottom`——
与非 loading 分支阈值一致。用户上滚超过阈值后停止跟随，`last_scrolled_at` 也不更新，
下次 effect 重新检测；用户回到接近底部后自然恢复跟随。

### 涉及文件

- `peri-tui/src/kit/message_area/mod.rs`（View 宽度对齐 vis_width）
- `peri-tui/src/kit/message_area/scroll.rs`（is_loading 分支加距离阈值）

### 验证

`cargo test -p peri-tui --lib` 全部 410 个测试通过。现象 B 的修复依赖现有
`proximity_check` 阈值语义（已覆盖 7 个测试 case）。
