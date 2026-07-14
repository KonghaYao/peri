# 消息区拖拽复制时 Unicode 字符后段错位（越往后偏移越大）

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-12

## 问题描述

消息区鼠标拖拽选中文本后，复制到系统剪贴板的字符串**前几个字符正确，越往后偏移越大**——选中范围里如果包含 Unicode 字符（CJK 中文 / 日文 / 韩文 / emoji 等），Unicode 字符之后的文本会整体偏移，复制出来的字符串与用户在终端看到的高亮选区不一致。

典型表现：选中"abc你好def"这种 ASCII + CJK 混合的文本，复制得到的字符串可能是"abc你好de"或"abc你好defg"（前后偏移），并不是用户高亮的那一段。

用户判断是字符宽度计算问题——CJK 字符在终端占 2 列显示宽度，但复制提取时可能按 1 字符 / 1 byte 处理，导致后续每个 CJK 字符累积 1 列偏移。

## 症状详情

| 场景 | 现象 |
|------|------|
| 选中纯 ASCII 文本 | 复制正确，无错位 |
| 选中包含 CJK / emoji 的混合文本 | ASCII 部分正确，遇到 Unicode 字符后开始偏移，越往后偏移越大 |
| 偏移累积性质 | 每个 Unicode 字符累积约 1 列偏移（典型 2 列显示宽度 vs 1 列内部坐标的不一致） |
| 选区高亮 vs 实际复制内容 | 终端显示的高亮范围与剪贴板内容不一致——高亮的是 A 段，复制出的是 A' 段（后段偏移） |

## 复现条件

- **复现频率**：必现（含 Unicode 字符时）
- **触发步骤**：
  1. 启动 TUI，进入有中文 / emoji / CJK 字符的消息流
  2. 鼠标按住左键拖拽选中一段含 Unicode 字符的文本，**跨越多个 Unicode 字符**使偏移累积明显
  3. 松开鼠标，自动复制到剪贴板
  4. 在外部（如编辑器 / 浏览器输入框）粘贴，对比选中范围与实际粘贴内容——后段错位
- **环境**：所有平台；与最近一次提交 `7ca2a632`（视口裁剪 + 复制功能修复）相关

## 关联历史

- `spec/issues/2026-07-11-message-area-mouse-selection-regression.md`（Fixed）—— 该 issue 记录的是"复制功能完全失效 + CPU 暴涨"。本次 issue 是该问题修复后的**残留 bug**：复制链路已通，但 Unicode 字符宽度处理有偏移。两个 issue 不重复，是同一功能的不同症状

## 涉及文件

- `peri-tui/src/kit/message_area.rs` —— `extract_visual_range` 函数：视口裁剪后从 `lines` + `wrap_map` 提取选区文本，进行字符级 / 列级偏移换算
- `peri-tui/src/kit/text_selection.rs` —— `visual_col_to_byte_offset`（100-110 行）：将视觉列号转为字节偏移；`line_to_plain_text` / `highlight_selected_lines`：文本提取与高亮换算涉及 Unicode 宽度
- 事件处理器（`message_area.rs` 的 `Down/Drag/Up`）—— `visual_row` / `visual_col` 的计算起点

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-12 | — | Open | agent | 创建 |
| 2026-07-12 | Open | Fixed | agent | 修复根因（见下） |

## 修复记录

### 根因（两层）

1. **折行偏移公式错误**（`peri-tui/src/kit/message_area/selection.rs:142-144`）：旧公式
   `c = vis_col + (vis_row - visual_start) * width` 假设每个视觉行恰好占满 `width` 列。
   但 ratatui 用 `WordWrapper` 做 word-level wrap——CJK 文本（无空格）被当成单个 word，
   超宽也不拆分；ASCII 在空格处优先换行；每视觉行实际占列数不固定。按 `width×k` 推算会
   累积偏移。

2. **双宽字符半字符边界处理错误**（`peri-tui/src/kit/text_selection.rs:100-110`）：
   `visual_col_to_byte_offset` 把落在双宽字符中间的 target_col 一律当作"字符起点不含"，
   但终端鼠标报告 col 落在双宽字符右半（如 '你' 占 col 2-3 中的 col 3）时，光标实际已
   越过该字符——选区应包含该字符。原逻辑会让复制结果少一个字符。

### 修复

1. **`selection.rs::wrap_byte_starts`**（新增）：直接用 `Paragraph::wrap` 渲染 `Line` 到
   offscreen `Buffer`，按 cell 流匹配 plain text 字符，确定每个视觉行的 byte 起始偏移。
   这是唯一能 100% 复刻 ratatui 实际 wrap 行为的方法。`extract_visual_range` 改用此函数
   替代 `width×k` 公式。

2. **`selection.rs::row_start_byte` + `row_end_byte`**（新增，替代 `row_col_to_byte`）：
   区分选区起点（落在字符范围内时总返回字符起点，含字符）与终点（左半不含 / 右半含）。

3. **`text_selection.rs::visual_col_to_byte_offset`**：加半字符规则——target_col 落在
   `[col, col + ceil(cw/2))` 视为左半不含，落在 `[col + ceil(cw/2), col + cw)` 视为右半含。

### 测试覆盖

`peri-tui/src/kit/message_area/selection.rs::tests` 新增 16 个单元测试：
- `wrap_byte_starts`：纯 ASCII / CJK / 混合 / 空 / 单 CJK / 跨宽度（5/4/3/10）
- `wrap_byte_starts` 行数与 ratatui `Paragraph::line_count` 一致性（关键不变量）
- `extract_visual_range`：CJK 同行左右半、CJK 跨视觉行、ASCII 同行/跨视觉行/跨逻辑行、反向拖拽规范化

### 涉及文件

- `peri-tui/src/kit/message_area/selection.rs`（核心修复 + 测试）
- `peri-tui/src/kit/text_selection.rs`（半字符规则）
