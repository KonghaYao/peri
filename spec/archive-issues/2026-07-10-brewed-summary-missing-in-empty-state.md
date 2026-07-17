> 归档于 2026-07-17，原路径 spec/issues/2026-07-10-brewed-summary-missing-in-empty-state.md
# MessageArea 空态时不显示「✻ Brewed for Xm Xs」总结行

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-10

## 问题描述

Peri TUI 的 MessageArea 在空态（无消息、无 loading、无 todo）时，设计规范要求在 footer 保留上次 loading 完成后的总结行「✻ Brewed for Xm Xs」（灰色 MUTED）。当前实际表现是：空态时直接跳转到 Welcome 视图，总结行不显示。

## 症状详情

根据 TUI-PAGE.md 设计规范第 400 行：
```
│ │ │ │ - 空态时保留「✻ Brewed for Xm Xs」（灰色 MUTED）                    │ │ │ │
```

**期望行为**：loading 结束后，即使消息区无任何内容，底部仍显示灰色「✻ Brewed for Xm Xs」总结行。

**实际行为**：loading 结束后无消息时，界面展示 Welcome 视图，不显示总结行。

## 根因分析

两个独立问题叠加导致：

1. **单帧延迟**：`build_footer_lines` 中 `has_summary` 检查在 mutation block **之前**，loading 结束那帧读到旧值 0 → 早退返回空。下一帧 `has_summary` 变为 true 但 auto-scroll deps 未变 → Brewer 行在 ScrollView 内容中但视口以下。

2. **footer 与 ScrollView 耦合**：footer_lines 通过 `all_lines.extend()` 内嵌在 ScrollView 内容中。空态时走 Welcome 早退分支直接 `return`，整个 ScrollView + footer 渲染路径被跳过后，footer_lines 被丢弃。

3. **空态无判空**：`brewed_lines = Some(footer_lines.clone())` 未检查 `footer_lines.is_empty()`，首次启动时 footer 为空仍进入 Brewed 渲染分支。

## 涉及文件

- `peri-tui/src/kit/message_area.rs` —— 4 处变更

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-10 | — | Open | agent | 创建 |
| 2026-07-10 | Open | Fixed | agent | 4 处修复，编译+测试通过 |

## 修复记录

### 修复 #1（2026-07-10）

- **操作人**：agent
- **用户原意**：loading 结束后空态显示灰色 Brewed 总结行；/clear 后回干净的 Welcome
- **根因**：① build_footer_lines 单帧延迟 ② footer 内嵌 ScrollView，空态 Welcome 早退丢弃 footer ③ brewed_lines 未判空
- **修复内容**：
  1. `has_summary` 检查从 mutation block **前**移到**后**——消除单帧延迟，loading 结束同帧生成 Brewer
  2. 空态时 `footer_lines` 不 extend 进 `all_lines`——分离后用 `brewed_lines` 在 Welcome 下方独立渲染
  3. `brewed_lines` 加 `!footer_lines.is_empty()` 判空——首次启动不走 Brewed 分支
  4. 新增 `BRIDGE_RESET_COUNTER` 检测——/clear 时清零 `summary_elapsed_ms`，回干净的 Welcome
- **涉及文件**：`peri-tui/src/kit/message_area.rs`
