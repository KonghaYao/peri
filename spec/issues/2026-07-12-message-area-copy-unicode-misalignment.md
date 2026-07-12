# 消息区拖拽复制时 Unicode 字符后段错位（越往后偏移越大）

**状态**：Open
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

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
