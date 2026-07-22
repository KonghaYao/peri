> 归档于 2026-07-18，原路径 spec/issues/2026-07-09-textarea-no-soft-wrap.md
# textarea 缺少软换行（soft wrapping），长行被截断且视口跟随异常

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-09

## 问题描述

输入区域（composer）的 textarea 组件只做硬换行（按 `\n` 拆分），不支持软换行（按终端宽度自动折行）。当用户输入超过 composer 宽度的长文本时，超出部分被 ratatui 截断不可见。视口跟随和光标上下移动也基于逻辑行而非视觉行，导致软换行场景下行为异常。

三个连锁症状：
1. 长行超出输入框右边界被截断，用户看不到全部内容
2. 视口跟随基于逻辑行数（`text.matches('\n').count() + 1`），软换行后一个长逻辑行可能占多个视觉行，光标可能落在视口外
3. 上下移动光标时用逻辑列定位（如 CJK 文本折行后按 Down 应跳到同视觉列的下一行，当前跳到下一逻辑行）

## 症状详情

### 现象 1：长行被截断

在 composer 中输入一段超过终端宽度的长文本（如连续输入 100 个汉字），文本不会自动折行显示，右侧被截断。用户需要手动按 Enter 换行才能看到全部内容。

### 现象 2：视口跟随失效

当文本已通过手动换行超过 10 行（composer 最大可见行数），当前视口跟随基于 `\n` 计数。如果某逻辑行需折成 3 个视觉行显示，实际视觉行数可能远超 `\n` 计数，光标所在行可能不在视口内。

### 现象 3：Up/Down 光标跳转不符合预期

假设 composer 宽 4 列，文本 `"你好世界"` 显示为：
```
你好
世界
```
按 Down 键时期望光标从第 1 行跳转到第 2 行对应视觉列。当前实现中因为没有软换行，光标直接移动到文本末尾（因为只有一个逻辑行）。

## 现状

| 组件 | 当前行为 |
|------|----------|
| `render_multiline_with_cursor` | 只按 `\n` 拆分逻辑行渲染，无软换行 |
| `TextAreaState::cursor_line_up/down` | 使用逻辑行/逻辑列做上下移动 |
| 视口裁剪 | `total_line_count = text.matches('\n').count() + 1`，基于逻辑行 |
| `input_area.rs` 渲染 | 未传入 max_width 参数 |

现有 `render_multiline_with_cursor` 签名（`peri-widgets/src/textarea/render.rs:31`）：
```rust
pub fn render_multiline_with_cursor(
    text: &str, cursor: usize, cursor_style: Style,
    selection_range: Option<(usize, usize)>, selection_style: Style,
    placeholder: Option<&str>, placeholder_style: Style,
    viewport_height: usize, loading: bool, show_cursor: bool,
) -> Vec<Line<'static>>
```

**缺少 `max_width` 参数**，渲染层不知道当前可用宽度，无法做软换行。

## 期望改进方向

实现浏览器 textarea 式的软换行体验：
- 文本按可用宽度（display-width 感知）自动折行
- 视口跟随基于折行后的视觉行
- 上下移动光标时保持相同视觉列
- 输入、删除、粘贴后折行实时更新

## 设计方案摘要

（来自 2026-07-09 对话中的完整设计）

**折行策略**：任意字符处断行（`overflow-wrap: break-word`），与浏览器 textarea 默认一致。

**核心改动**：

| 文件 | 改动 |
|------|------|
| `peri-widgets/src/textarea/render.rs` | 新增 `wrap_text()` 折行函数 + `WrapResult`/`VisualLine` 结构；`render_multiline_with_cursor` 增加 `max_width` 参数 |
| `peri-widgets/src/textarea/state.rs` | `TextAreaState` 新增 `desired_col: Option<usize>` 视觉列记忆；`cursor_line_up/down` 增加 `max_width` 参数 |
| `peri-widgets/src/textarea/widget.rs` | `area.width` 作为 `max_width` 传入 |
| `peri-widgets/src/textarea/mod.rs` | 导出新公开类型 |
| `peri-tui/src/kit/input_area.rs` | 从 composer area 计算可用宽度传入渲染；Up/Down 事件传入 `max_width` |
| 测试文件 | 所有调用点增加 `max_width` 参数；新增折行场景测试 |

**关键设计决策**：折行在渲染层做，不存储到状态层（折行是纯视觉概念，文本内容不变）。

## 涉及文件

- `peri-widgets/src/textarea/render.rs`（233 行）—— 渲染核心，缺少软换行逻辑
- `peri-widgets/src/textarea/state.rs`（464 行）—— 光标移动用逻辑行坐标
- `peri-widgets/src/textarea/widget.rs`（89 行）—— widget 封装
- `peri-widgets/src/textarea/mod.rs`（7 行）—— 模块导出
- `peri-tui/src/kit/input_area.rs`（~1000 行）—— 调用渲染 + 事件处理
- `peri-widgets/src/textarea/state_test.rs`（559 行）—— 需更新所有渲染调用
- `peri-widgets/src/textarea/render_test.rs`（275 行）—— 需更新所有渲染调用

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-09 | — | Open | agent | 创建 |

## 修复记录

（待实施）
