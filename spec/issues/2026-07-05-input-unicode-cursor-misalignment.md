# 输入框 Unicode 字符删除时光标估算错误，出现多个白色光标残影

**状态**：Partial
**优先级**：中
**创建日期**：2026-07-05

## 问题描述

主输入框在编辑包含 Unicode 宽字符（CJK 中文/日文/韩文、emoji 等）的文本时，会出现两个视觉问题：

1. **空输入框第一个字符位置出现两个光标**：空态时的 `▌` 块光标和终端硬件光标重叠
2. **Backspace 删除 Unicode 字符后出现多个白色光标**：光标样式（反色白底）被渲染到错误的屏幕位置，因为光标位置以字符索引而非显示列计算

## 症状详情

### 症状 1：空态双光标

当输入框为空且未处于加载态时，`render_multiline_with_cursor()`（`input_area.rs:1098-1104`）渲染 `\u{258C}`（LEFT HALF BLOCK）作为组件层视觉光标。`build_composer_lines`（`input_area.rs:775`）在其前追加 `" ❯ "` 前缀。渲染结果为 ` ❯ ▌`。

同时，ratatui-kit 框架可能在输入区域设置终端硬件光标（用于 IME 定位锚）。两套光标独立决策，在位置 0 处重叠，表现为两个光标。

> 此问题在 `docs/blogs/terminal-cursor-maintenance/terminal-cursor-maintenance.md:21-23` 已有文档记录——"两套光标并存——textarea 的 REVERSED 空格和 Frame::set_cursor_position 设置的终端硬件光标"。之前 tui-textarea 的修复方案是"移除 textarea 的行尾光标渲染，光标可视化统一由终端硬件光标负责"，但 ratatui-kit 迁移后此策略未延续到新 InputArea。

### 症状 2：Unicode 字符删除后多光标残影

`render_multiline_with_cursor()`（`input_area.rs:1088-1178`）在光标定位上**完全未使用 `unicode-width`**。核心问题链路：

1. `EditorState::cursor` 是字符索引（如 "你好世界" 中 cursor=2 表示在 "世" 之前，共 2 个 Rust char）
2. `render_multiline_with_cursor` 将 `target_col`（字符索引）经 `char_to_byte` 转为字节偏移
3. 用字节偏移切分 `line[..col_byte]` → 渲染前半部分
4. 对后半部分的下一个完整 char 施加 `cursor_style`（反色白底）

**当文本全部是 ASCII 字符时，此逻辑正确**——因为 ASCII 1 char = 1 显示列 = 1 字节。

**当文本包含 CJK/宽字符时，问题出现**：
- CJK 字符 1 char = 2 显示列 ≠ 1 字符索引单位
- 终端硬件光标位置由 ratatui-kit 基于**显示列**计算（假设有的话）
- 组件层视觉光标基于**字符索引**渲染
- 两者位置不一致，视觉上出现多个光标

**对比**：同仓库的 `text_selection.rs:100-110` 中的 `visual_col_to_byte_offset()` 正确使用了 `unicode_width::UnicodeWidthChar::width()` 将显示列映射到字节偏移。`render_multiline_with_cursor` 缺少同等的宽度感知。

## 复现条件

- **复现频率**：每当输入框包含 CJK/宽字符时必现
- **触发步骤**：
  1. 在输入框输入中文文本，如 "你好世界"
  2. 将光标移到文本中间位置
  3. 按 Backspace 删除一个字
  4. 观察：光标高亮（反色白底）位置与实际删除后字符不对齐，出现多个白色光标
- **环境**：macOS 终端（iTerm2 / Terminal.app / kitty 等）

## 涉及文件

| 文件 | 说明 |
|------|------|
| `peri-tui/src/kit/input_area.rs:1088-1178` | `render_multiline_with_cursor()` —— 核心问题函数，未使用 unicode-width |
| `peri-tui/src/kit/input_area.rs:1098-1104` | 空态 `▌` 光标渲染（症状 1） |
| `peri-tui/src/kit/input_area.rs:753-787` | `build_composer_lines` —— 追加 " ❯ " 前缀 |
| `peri-tui/src/kit/input_area.rs:69-76` | `EditorState::backspace()` —— 字符索引维护正确 |
| `peri-tui/src/kit/text_selection.rs:100-110` | `visual_col_to_byte_offset()` —— 正确实现（参考） |
| `peri-tui/src/kit/theme.rs:283-292` | `InputTokens` cursor_fg/cursor_bg —— 白色反色样式 |

## 根因分析

**直接原因**：`render_multiline_with_cursor()` 以字符索引而非显示列定位光标。

**深层原因**：ratatui-kit 迁移后，InputArea 是自己实现的 `EditorState` + `render_multiline_with_cursor`，未复用之前 tui-textarea 中已修复的光标方案（统一由终端硬件光标负责，不渲染组件层光标）。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-05 | — | Open | agent | 初始创建 |
| 2026-07-05 | Open | Fixed | agent | 修复空态双光标 + unicode-width 感知绑定 |
| 2026-07-05 | Fixed | Partial | agent | 光标残影修复失败，`Paragraph.style(bg)` 不生效 |
| 2026-07-09 | Partial | Partial | agent | 追加：软换行迁移后问题复现，涉及文件变更记录 |
| 2026-07-09 | Partial | Partial | agent | 追加：Span bg 三方案测试，提出 CJK continuation marker 假设 |

## 修复记录

### 修复 #1（2026-07-05）

- **操作人**：agent
- **用户原意**：输入框第一个字符处出现两个光标，backspace 删除 unicode 字符时出现多个白色光标残影
- **修复内容**：
  1. **空态光标**：将 `▌`（U+258C LEFT HALF BLOCK）替换为 `" "` + cursor_style（styled space），与行尾光标保持一致，消除空态"双光标"视觉效果（`input_area.rs:1107-1114`）
  2. **unicode-width 感知**：新增 `display_width_before()` 辅助函数，使用 `unicode_width::UnicodeWidthChar::width()` 计算字符前显示列宽度，确保 CJK 双宽字符场景下光标定位于正确字符（`input_area.rs:336-342`）
  3. **光标渲染**：`render_multiline_with_cursor()` 基于 `display_width_before` 双重定位（visual col → byte offset），光标采用**字符反色高亮**（用户期望的"字反色"行为），行尾为反色空白（`input_area.rs:1156-1184`）
  4. **光标残影修复**：`Paragraph` 添加显式背景色 `surface.default` 通过 `.style()` 设置，确保 ratatui 在文本缩短时用背景色填充超出列，消除旧帧像素残留（`input_area.rs:653-658`）
  5. **回归测试**：新增/更新 6 个 CJK 光标渲染测试 + `display_width_before` 单测（`input_area.rs:1317-1379`）
- **涉及 commit**：待提交
- **验证状态**：待验证

### 修复 #2（2026-07-05）—— 光标样式 + 残影修复（失败）

- **操作人**：agent
- **用户反馈**：(1) 光标不该是占位 space 而应是字符反色；(2) backspace 时光标残影仍未消除
- **尝试**：
  1. **光标样式回退为"字反色"**：`render_multiline_with_cursor` 中光标使用 `Span::styled(char_at_cursor, cursor_style)` 高亮光标所在字符 ✔️
  2. **残影根因假设**：`Paragraph` 未设显式背景 → ratatui 仅渲染文本 span 所在单元格，文本缩短后超出列保留终端原有像素
  3. **尝试修复**：`Paragraph.style(Style::default().bg(theme::semantic().surface.default))` 设置显式背景色
- **结果**：❌ 残影仍存在。`Paragraph.style(bg)` 不足以覆盖残留。
- **分析**：残影可能不来自 Paragraph 层，而是来自 ratatui-kit `View` 容器或 `AppShell` 根布局的帧间不清除。`element!` 宏树中的 `View` 容器可能未将 Paragraph 的显式背景传播到终端帧缓冲，残影发生在框架层而非 widget 层。需从 ratatui-kit 渲染管线或 `Frame` 层面排查。

### 修复 #3（2026-07-09）—— Span 显式 bg + 行填充

- **操作人**：agent
- **用户原意**：删除 CJK 字符后光标残影未消除
- **尝试**：
  1. 给 `render_multiline_with_cursor` 新增 `default_style` 参数，非光标 Span 使用显式 bg
  2. 行尾 padding 填满至 `max_width`，确保整行背景统一
  3. 三次 bg 值测试：`Rgb(0,0,0)`（残影消失但背景色不一致）→ `Color::Reset`（视觉透明但残影复现）
- **涉及 commit**：`392941c6`（当前保留 `Color::Reset` 版本）
- **结论**：残影并非 cell bg 值问题——只有 `Rgb(0,0,0)` 等具体颜色有效，`Color::Reset` 无效。推测 ratatui 的 diff 引擎对 `Color::Reset` 做了优化跳过。残影可能需要从 **CJK continuation marker** 或**终端帧缓冲 diff** 层面解决。

### 现象 3（2026-07-09 复现）—— 软换行迁移后问题仍存

**背景**：commit `9df8f6ce` 完成了 textarea 软换行（soft wrapping）重构，`render_multiline_with_cursor` 已从 `input_area.rs` 迁移至 `peri-widgets/src/textarea/render.rs`，`EditorState` 替换为 `TextAreaState`。渲染管线现在走 `wrap_text()` 折行 → `render_multiline_with_cursor`（视觉行渲染）。

**复现**：
1. 在 composer 输入 "你好世界"
2. 光标移到中间位置
3. 按 Backspace 删除一个中文
4. 观察：光标残影与之前一致——被删除字符位置的白色反色高亮残留

**当前版本涉及文件变更**（相比 2026-07-05 原始 issue）：

| 文件 | 说明 |
|------|------|
| `peri-widgets/src/textarea/render.rs` | `render_multiline_with_cursor` 已迁入，使用 `display_width_before` 做光标定位 |
| `peri-widgets/src/textarea/state.rs` | `TextAreaState` 替代 `EditorState`，cursor 仍为字符索引 |
| `peri-tui/src/kit/input_area.rs` | 通过 `render_multiline_with_cursor_for_themed` 间接调用，已设 `Paragraph.style(bg)` |

`Paragraph.style(bg)` 的背景色修复在旧管线和新管线中均未消除残影，进一步支持残影来自 ratatui-kit 框架层而非 widget 层的判断。

### 现象 4（2026-07-09 二次调试）—— Span bg 修复部分有效但未根治

**尝试 1**：给 `render_multiline_with_cursor` 增加 `default_style` 参数，非光标 Span 使用 `bg(surface.default = Rgb(0,0,0))`。

- 结果：残影消失 ✅，但输入框背景色与周围 UI 不一致（文字区域黑底，空白区域终端默认色）

**尝试 2**：行尾 padding 填满至 `max_width`，消除文字/空白区域分界。

- 结果：背景色统一 ✅，但整个输入框变成 `Rgb(0,0,0)` 黑底，与终端默认背景色冲突

**尝试 3**：改用 `bg(Color::Reset)` —— 显式 `Some(Color::Reset)` 覆盖旧 cell bg，同时视觉匹配终端默认。

- 结果：commit `392941c6`，58 tests pass。但用户反馈残影**仍未完全消除**。

**分析**：三种不同的显式 bg 方案中，只有 `Rgb(0,0,0)` 有效（方案 1），`Color::Reset`（方案 3）无效。这暗示问题不是"bg 值是什么"，而是 **ratatui 是否会写入该 cell**——`Rgb(0,0,0)` 强制写入，`Color::Reset` 可能被 ratatui 的 diff 优化跳过。

**新假设**：残影可能与 CJK 字符的双 cell 机制有关。CJK 字符占 2 个终端 cell（第 2 个 cell 是 continuation marker）。当光标从 CJK 字符位置移走后，ratatui 可能更新了第 1 个 cell 但跳过了第 2 个 continuation cell（如果新旧都是 CJK，continuation marker 相同，ratatui diff 可能认为无需更新），导致旧 cursor_style bg 残留在第 2 个 cell 中。

**下一步**：验证 continuation marker 假设——在渲染输出中检查 CJK 字符的第 2 个 cell 是否被正确覆盖。
