> 归档于 2026-07-10，原路径 spec/issues/2026-07-09-message-area-periodic-white-flash-streaming.md

# 消息区在 agent 流式回复中周期性闪白（每 2-5 秒）

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-09

## 问题描述

agent 流式回复过程中，消息区每隔约 2-5 秒会出现一次整体闪白——画面短暂全白/全黑后立即恢复正常内容。闪烁不影响 agent 运行，但持续不断的闪白严重影响阅读体验和使用感受。该问题一直存在，非近期引入。

## 症状详情

| 维度 | 观察到的现象 |
|------|--------------|
| 表现 | 消息区整体闪白（或闪黑），瞬间恢复——类似整屏全量重绘 |
| 触发时机 | 仅在 agent 回复/流式输出 token 过程中出现 |
| 周期性 | 约每 2-5 秒闪一次，持续不停，直到流式输出结束 |
| event / resize 触发闪烁 | 与此无关（已有独立 issue #2026-07-07 跟踪 resize + stream end 一次性闪烁） |
| 空闲态 | 不闪烁 |

### 补充（2026-07-09）—— 根因已确认

**根因不是渲染管线竞态，而是吸底 `use_effect` 的 `scroll_to_bottom()` 写入环路。**

`message_area.rs` 中 I23-b 就近判断吸底 effect 的 deps 为 `(entries_len, raw_ch)`，流式期间每个 chunk 都递增这两个值 → effect 高频触发 → `st.write().scroll_to_bottom()` 写入 `ScrollViewState` atom → ratatui-kit 检测到 atom 变更 → 触发下一帧重渲染 → 重新计算 `total_visual_rows` → `scroll_y` 仍比新 `max_scroll` 小一点 → 下一帧 effect 再次 `scroll_to_bottom()`。形成 **render → effect → state write → render** 紧耦合环路，每秒几十帧都在跑这个。

thinking 阶段尤为明显：reasoning chunk 比普通 text chunk 更密集，effect 触发频率更高。

**验证方式**：完全移除 `use_effect` 吸底逻辑后，闪烁消失。

| 组件 | 表现 |
|------|------|
| 消息区 | 闪白 |
| 状态栏 | 不受影响 |
| 输入区 | 不受影响 |

## 复现条件

- **复现频率**：必现（任何 agent 流式回复过程中）
- **触发步骤**：
  1. 在 TUI 中输入任意需 agent 回复的消息
  2. 等待 agent 流式输出 token
  3. 每隔 2-5 秒观察消息区整体闪白一次，持续至流式结束
- **环境**：macOS 26.5.1，ratatui-kit 架构，任意模型

## 涉及文件

| 文件 | 角色 |
|------|------|
| `peri-tui/src/kit/message_area.rs` | I23-b 吸底 `use_effect`——流式期间每帧 `scroll_to_bottom()` 写入 `ScrollViewState` atom，形成 render↔effect 紧耦合环路导致闪烁。修复 #3 用增量门控 `last_scrolled_at` + 底 guard 解决 |
| `peri-tui/src/kit/render_bridge.rs` | ~~（误判）poll tick 全量重建竞态~~ 回退，与此 bug 无关 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-09 | — | Open | agent | 创建 |
| 2026-07-09 | Open | Fixed | agent | 根因确认：吸底 `use_effect` 的 `scroll_to_bottom()` 写入环路，移除 effect 后闪烁消失 |
| 2026-07-09 | Fixed | Fixed | agent | 重新实现吸底逻辑：增量门控 + 跳写 guard + 就近跟随（距底 ≤3 行）。待用户验证 |

## 修复记录

### 修复 #1（2026-07-09）

- **根因**：`render_bridge` 的 1 秒 poll tick 在流式期间触发全量 `rebuild_entries`，其 async yield 点与 event 分支增量更新产生并发写 RENDER_CACHE 的竞态，消息区读到中间态/空缓存 → 渲染空白帧
- **修复**：`render_bridge.rs` — 流式期间（`ACP_STATE.is_loading`）poll tick 检测到 generation 变化后跳过全量重建，仅同步 `last_generation`。事件路径已负责所有增量更新
- **涉及文件**：`peri-tui/src/kit/render_bridge.rs`（+10/-2）
- **结论**：❌ 误判。用户反馈"还是会有"，且后续确认为滚动相关，非 render_bridge 竞态。修复已回退。

### 修复 #2（2026-07-09）

- **根因**：`message_area.rs` I23-b 吸底 `use_effect`（`(entries_len, raw_ch)` deps）在流式期间高频触发，每帧 `scroll_to_bottom()` 写入 `ScrollViewState` atom → ratatui-kit 检测 atom 变更 → 重渲染 → `total_visual_rows` 变化 → 下帧 effect 再次触发 → render↔effect 紧耦合环路
- **修复**：完全移除 `use_effect` 吸底 block（lines 524-573），同时移除仅被该 effect 使用的 `prev_entries_len` state。手动滚动不受影响
- **涉及文件**：`peri-tui/src/kit/message_area.rs`（-51 lines）
- **结论**：✅ 暂时移除了吸底逻辑，闪烁消失——确认根因在吸底 effect。但需要恢复吸底功能。

### 修复 #3（2026-07-09）

- **根因**：旧 I23-b 代码两个问题：(a) 每 chunk 无条件调用 `scroll_to_bottom()` 写入 atom，即使已经在底部；(b) 阈值 (`vh/2`, ~20–30 行) 过大，流式期间几乎每帧都触发 atom 写入
- **修复**：用 **`last_scrolled_at` 增量门控** 代替距离阈值门控：
  1. **首次/收缩** → 无条件滚到底
  2. **已在底部** (`scroll_y >= max_scroll`) → 跳写（`scroll_to_bottom()` 把 `offset.y` 设为 `content_height - 1`，远大于 `max_scroll`，即使内容增长 30+ 行仍满足这个 guard）
  3. **就近跟随** → 仅当用户距底部 ≤3 行 **且** `total_visual_rows` 确实增长了才滚一次。`last_scrolled_at` 记录上次实际滚动时的内容高度，防止同一内容版本多次 atom 写入
- **涉及文件**：`peri-tui/src/kit/message_area.rs`（新增 `last_scrolled_at` state，重写 effect 逻辑）
- **待验证**：用户在实际流式场景中测试闪烁是否消除
