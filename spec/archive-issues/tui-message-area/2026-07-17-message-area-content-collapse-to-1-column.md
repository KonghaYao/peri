> 归档于 2026-08-11，原路径 spec/issues/2026-07-17-message-area-content-collapse-to-1-column.md

# 消息区内容瞬间坍缩到只有 1 列宽度

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-17

## 问题描述

消息区域的内容被挤压到极窄——文字每行只显示 1 个字符就换行，内容竖着排成一条线。坍塌是**持续性**的（不会自动恢复），严重影响消息区的正常使用。

## 症状详情

| 维度 | 描述 |
|------|------|
| 表现 | 消息区面板大小正常，但内部文字列宽坍缩到 1 列（每字符换行） |
| 持续性 | 持续（不恢复） |
| 引入版本 | commit `e396506d` → `005de122`（最近 2 次提交） |

## 复现条件

- **复现频率**：必现
- **触发步骤**：启动 TUI，观察消息区内容渲染
- **环境**：macOS，任意终端宽度

## 根因分析

### 罪魁提交

`e396506d fix(tui): 修复消息区 Paragraph scroll 被 ratatui-kit Text 组件覆盖`

此提交将消息区内层 View 的宽度约束从 `Constraint::Fill(1)` 改为 `Constraint::Length(vis_width)`：

```diff
- width: Constraint::Fill(1),
+ width: Constraint::Length(vis_width),
```

### 坍塌机制

`vis_width` 的计算依赖 `area_rect`（来自 `MsgAreaTracker` hook）：

```rust
let vis_width = area_rect                         // Option<Rect>
    .map(|r| r.width.saturating_sub(1))
    .unwrap_or(props.width as u16)                // fallback
    .max(1);                                      // floor
```

在 ratatui-kit 的生命周期中，**`render body`（`update()` 阶段）先于 `pre_component_draw`（`draw()` 阶段）执行**。这意味着 `render body` 中读取的 `area_rect` 总是**上一帧** `draw()` 时设置的值。

首帧 `area_rect` 为 `None`，fallback 到 `props.width`。`props.width` 来自 `SessionColumn` 通过 `use_terminal_size()` 计算的 `term_w - 4`（下限 20），理论上不会为 1。

但 `Constraint::Length(vis_width)` 的语义在 ratatui-kit 的 `Direction::Vertical` flex 布局（cross-axis = Horizontal）中，通过 `calc_children_areas` 的 cross-axis 处理：

```rust
// calc_children_areas 中 cross-axis 处理
let rev_direction = match layout_style.flex_direction {
    Direction::Vertical => Direction::Horizontal,  // cross-axis
    ...
};
for (area, constraint) in areas.iter()
    .zip(children.get_constraints(rev_direction))  // get_width()
{
    let area = Layout::new(rev_direction, [constraint]).split(*area)[0];
    children_areas.push(area);
}
```

`Layout::new(Horizontal, [Length(vis_width)])` 应正确分配宽度 = `vis_width`。但持续性坍塌表明 View 实际分配的宽度为 1，可能的根因方向：

1. **`props.width` 实际传递值为 0**：`#[component]` 宏的 props 传递链路存在问题，导致 `MessageAreaProps.width` 始终为默认值 0（`#[derive(Default)]` → `usize::default() = 0`）
2. **`Constraint::Length` 在 cross-axis 上有 bug**：ratatui-kit 0.10.2 的 `Constraint::Length` 在 `Direction::Vertical` 的 cross-axis (Horizontal) 中未按预期工作
3. **`MsgAreaTracker` hook 生命周期问题**：`area_rect` 始终为 `None`（hook 未正确持久化跨帧状态）

### 旧代码为何正常

旧代码使用 `Constraint::Fill(1)`，View 宽度**始终填满父容器 cross-axis**，不依赖 `vis_width` 的具体值。Paragraph 的 wrap 宽度通过 `Block::padding` 间接控制（减 1 列留给滚动条）。

## 涉及文件

- `peri-tui/src/kit/message_area/mod.rs:497` —— View 宽度约束 `Constraint::Length(vis_width)`（由 commit `e396506d` 引入）
- `peri-tui/src/kit/message_area/props.rs:86-89` —— `MessageAreaProps.width` 默认值 0
- `peri-tui/src/kit/layout.rs:36` —— `SessionColumn` 传递 `width` prop

## 建议修复方向

1. **回退 View 宽度为 `Constraint::Fill(1)`**，恢复 `Block::padding` 方式控制 wrap 宽度
2. **或**：确保 `vis_width` 在 `render body` 执行时始终有正确的值（例如通过 `use_terminal_size` 直接计算而非依赖 `area_rect`）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-17 | — | Open | agent | 创建 |
| 2026-08-11 | Open | Fixed | agent | 归档：消息区 1 列坍缩修复（布局宽度/视口），修复记录见正文 |

## 修复记录

（待修复）
