# TUI 固定宽度换行全面审计

**状态**：Partial
**优先级**：中
**创建日期**：2026-07-17

## 问题描述

对 peri-tui 中所有使用固定宽度（而非终端实际宽度）进行文本换行、分隔线渲染、布局计算的代码位置做系统性审计。用户观察到消息流中的文本（如 cache 命中率警告 "Prompt cache hit rate 50% < 80% ..."）在未铺满右侧时就提前换行，怀疑存在固定数值的换行逻辑。

## 审计结果

### 审计覆盖范围

| 模块 | 路径 | 宽度策略 |
|------|------|----------|
| 消息区 Markdown 渲染 | `message_area/mod.rs` + `markdown/*.rs` | 动态（`vis_width = area.width - 1`） |
| SystemNote 渲染 | `message_area/render.rs:442-500` | 依赖 Paragraph 自动换行（width 由上层传入） |
| AskUser 内联面板 | `panels/ask_user.rs` | 混合（部分已修复为动态，部分仍固定） |
| AskUser Popup 弹窗 | `popups/ask_user_popup.rs` | 固定 |
| 输入区软换行 | `input_area.rs` | 动态（回退值固定 80） |
| 弹窗/浮层 | `popup_overlay.rs` | 动态 + `modal_max_width=90` 上限 |
| 状态栏 | `panels/status.rs` 等 | 动态布局 |

### 发现清单

#### ✅ 已动态（无需修改）

| # | 位置 | 说明 |
|---|------|------|
| 1 | `message_area/mod.rs:150-153` | `vis_width = area_rect.width - 1`，跟随终端实际渲染宽度 |
| 2 | `message_area/mod.rs:496` | 内层 View 用 `Constraint::Fill(1)`，Paragraph 实际 wrap 宽度 = `vis_width` |
| 3 | `panels/ask_user.rs:48-52` | 换行宽度已从 `WRAP_WIDTH=80` 改为 `(term_w as usize).saturating_sub(2).max(40)` |
| 4 | `panels/ask_user.rs:496` | 分隔线已从 `"─".repeat(60)` 改为 `"─".repeat(wrap_width)` |
| 5 | `input_area.rs:52` | `PROMPT_AND_BORDER_WIDTH = 5` 是提示符固定开销（`> ` 前缀 + border），正确做法 |
| 6 | `popup_overlay.rs:58` | `term_w.saturating_sub(4).min(modal_max_width).max(1)` — `modal_max_width=90` 是有意限制 |

#### ❌ 仍为固定宽度（待修复）

| # | 位置 | 当前值 | 影响 |
|---|------|--------|------|
| F1 | `markdown/convert.rs:142` | `"─".repeat(max_width.min(80))` | Markdown 水平分隔线 `---` 在 >80 列终端中最多渲染 80 列，不能铺满 |
| F2 | `popups/ask_user_popup.rs:252` | `"─".repeat(80)` | AskUser 旧版 Popup 分隔线固定 80 列 |
| F3 | `panels/ask_user.rs` 面板高度 | `PanelLayout::fixed(60, 18)` | 面板高度固定 18 行，与内容行数不匹配（已有独立 issue） |

#### △ 回退值固定（运行期动态，仅空态/首帧使用）

| # | 位置 | 说明 |
|---|------|------|
| F4 | `input_area.rs:343,359,596` | `.unwrap_or(80)` — composer area 未就绪时的回退值 |

---

## 症状详情

### F0: 消息区 Paragraph scroll 被 ratatui-kit Text 组件覆盖（已修复）

**状态**：Fixed

`message_area/mod.rs` 的原实现通过 `Block::default().padding(Padding::new(0, 1, 0, 0))` 给 Paragraph 加右 padding 1 列来控制 wrap 宽度 = `vis_width`，并通过 `.scroll((scroll_offset_y, 0))` 设置视口偏移。

**根因**：ratatui-kit 的 `Text` 组件（`text.rs:121-126`）会 clone 传入的 `Paragraph` 并调用 `.scroll((props.scroll.x, props.scroll.y))`，其中 `props.scroll` 默认值为 `(0, 0)`——覆盖了 Paragraph 已有的 scroll 偏移。这导致：

- 视口裁剪后的 `viewport_lines` 只包含视口范围内行，但 Paragraph 从第 0 行开始渲染
- 用户看到的文字位置与实际 scroll 偏移不一致（视觉上可能表现为"没铺满就换行"）

**修复**（`mod.rs`）：

1. View `width` 从 `Constraint::Fill(1)` 改为 `Constraint::Length(vis_width)`——直接限定 wrap 宽度，不再依赖 padding 间接控制
2. 移除 Paragraph 的 `.block(Block::default().padding(...))` 和 `.scroll(...)` builder 方法
3. 将 `scroll: Position::new(scroll_offset_y as u16, 0)` 作为 Text 组件的 prop 传入，绕过 ratatui-kit 的覆盖

### F1: Markdown 水平分隔线 80 列上限

`convert.rs:142`：
```rust
let rule_char = "─".repeat(max_width.min(80));
```

`max_width` 来自终端实际宽度（`vis_width`），但被 `.min(80)` 钳制。在 120+ 列宽终端中，Markdown 的 `---` 只渲染 80 列，视觉上显得右侧有大量空白未填充。与 cache 命中率警告等系统通知**无直接关系**，但属于同一类"固定宽度导致未铺满"问题。

**复现条件**：终端宽度 >80 列，agent 回复中包含 Markdown 分隔线 `---`。

### F2: AskUser Popup 分隔线 80 列

`popups/ask_user_popup.rs:252`：
```rust
lines.push(Line::from("─".repeat(80)).fg(semantic.border.default));
```

旧版 AskUser Popup（`Constraints::Percentage(60, 80)` 居中弹窗）。当前 TUI 已迁移到内联面板模式，此 popup 的 `render body` 已被清空，但 `render_popup()` 函数内的分隔线仍写死 80 列。

**实际影响**：极低——popup 当前未被使用，属于死代码清洁范畴。

### F3: AskUser 面板高度固定

已有独立 issue：`spec/issues/2026-07-16-ask-user-panel-height-wrap-mismatch.md`，此处不重复展开。

### F4: InputArea 回退值

`input_area.rs` 三处 `.unwrap_or(80)` 仅在第一帧渲染时 `composer_area` 为 `None` 时生效。实际运行期 `composer_area` 始终有值，回退值几乎不会被使用。属于防御性编程零影响项。

---

## 涉及文件

- `peri-tui/src/kit/markdown/convert.rs:142` —— 水平分隔线 80 列上限（F1）
- `peri-tui/src/kit/popups/ask_user_popup.rs:252` —— Popup 分隔线固定 80 列（F2）
- `peri-tui/src/kit/input_area.rs:343,359,596` —— 回退值 `unwrap_or(80)`（F4）
- `peri-tui/src/kit/panels/ask_user.rs:48-52,496` —— 已修复的动态宽度（供对照）

### 引用已有 issue

- `spec/issues/2026-07-15-ask-user-panel-layout-wrong-wide-terminal.md` —— AskUser 面板固定宽度问题（已部分修复，分隔线已改为动态，但状态仍为 Open）
- `spec/issues/2026-07-16-ask-user-panel-height-wrap-mismatch.md` —— AskUser 面板高度固定 18 行

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-17 | — | Open | agent | 创建 |
| 2026-07-17 | Open | Partial | agent | 修复 F0（消息区 Paragraph scroll 覆盖），F1/F2/F3 仍待处理 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加）

### 修复 #1（2026-07-17）

- **操作人**：agent
- **用户原意**：用户观察到消息流中文字未铺满右侧就换行，怀疑存在固定数值的换行逻辑。经排查发现 ratatui-kit Text 组件会覆盖 Paragraph 的 scroll 偏移，导致视口裁剪后的渲染与实际偏移不一致
- **修复内容**：
  - View `width` 从 `Constraint::Fill(1)` 改为 `Constraint::Length(vis_width)`，直接限定 wrap 宽度
  - 移除 Paragraph 的 `.block(Block::default().padding(...))` 和 `.scroll(...)` 
  - 将 `scroll: Position::new(scroll_offset_y, 0)` 作为 Text 组件 prop 传入，绕过 ratatui-kit 覆盖
- **涉及文件**：`peri-tui/src/kit/message_area/mod.rs`
- **验证状态**：`cargo build -p peri-tui` 通过，`cargo test -p peri-tui --lib` 全部 565 测试通过
