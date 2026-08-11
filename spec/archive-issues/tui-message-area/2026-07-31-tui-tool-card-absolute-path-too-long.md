> 归档于 2026-08-11，原路径 spec/issues/2026-07-31-tui-tool-card-absolute-path-too-long.md

# TUI 工具卡片 Read 等路径显示过长——去掉 cwd 前缀显示相对路径

**状态**：Fixed
**优先级**：低
**创建日期**：2026-07-31

## 问题描述

TUI 中工具卡片头行展示文件路径时使用 LLM 传入的绝对路径，如 `Read (/Users/konghayao/code/ai/perihelion/peri-model/src/protocol/mod.rs) — 18 lines`。路径前缀（cwd）占据了大量显示空间，影响可读性。期望显示为去掉 cwd 前缀后的相对路径，如 `Read (peri-model/src/protocol/mod.rs) — 18 lines`。

## 现状

- 工具卡片头行的 `input_summary` 由 `summarize_input`（`peri-tui/src/truncate.rs`）从工具参数中提取，`Read`/`Write`/`Edit` 直接取 `file_path` **原样显示、不截断、不做路径处理**。
- 渲染层 `render_generic_tool_card_lines`（`peri-tui/src/kit/message_area/render.rs`）将 `input_summary` 组装为 `● Read ({summary}) — N lines`。
- LLM 按工具参数描述（"The absolute path to the file"）传入绝对路径，因此卡片长期显示完整绝对路径。

## 期望改进方向

用户原话："希望能够简化表达 Read 这些工具括号里面的信息，他们太长了，可以改为 peri-model 之后的链接，从而不需要前面的。"

已确认的决策：

| 维度 | 决策 |
|------|------|
| 修改范围 | **仅 TUI 显示层**——不动 ACP 协议/agent 事件，IDE 等其他消费方不受影响 |
| 非 cwd 路径 | **保持绝对路径原样**——只在路径以 cwd 开头时去前缀，其余（/tmp/xxx、~/xxx 等）不动 |
| 工具范围 | **所有路径类工具**——Read/Write/Edit 为核心，folder_operations、artifact 等含路径参数的工具统一处理 |

## 涉及文件

- `peri-tui/src/truncate.rs` —— `summarize_input` 提取 file_path，需在此（或渲染层）做路径精简
- `peri-tui/src/kit/message_area/render.rs`（524 行起）—— `render_generic_tool_card_lines` 组装头行显示
- `peri-tui/src/app/mod.rs`（130 行）—— `app.services.cwd` 为 TUI 持有的工作目录，需注入渲染链
- `peri-tui/src/kit/tool_display.rs` —— `format_tool_args` 同样输出 file_path 原样，若走该路径需一并处理

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-31 | — | Open | agent | 创建 |
| 2026-07-31 | Open | Fixed | agent | 修复：summarize_input 源头精简 cwd 前缀路径 |

## 修复记录

### 修复 #1（2026-07-31）

- **操作人**：agent
- **用户原意**：工具卡片头行 `Read (/Users/.../peri-model/src/protocol/mod.rs)` 太长，希望显示为去掉 cwd 前缀的相对路径 `Read (peri-model/src/protocol/mod.rs)`
- **修复内容**：
  - `peri-tui/src/truncate.rs`：新增 `set_display_cwd`（OnceLock 全局，启动时设置一次）与纯函数 `shorten_path_for_display`（cwd 前缀裁剪，非 cwd 路径/根目录/空 cwd 均原样）；`summarize_input` 对路径类字段（Read/Write/Edit 的 file_path/path、folder_operations 的 folder_path、artifact 的 file_path、兜底 path/file_path）统一应用精简
  - `peri-tui/src/app/mod.rs`：`App::new()` 中调用 `set_display_cwd` 注入启动 cwd
  - `peri-tui/src/truncate_test.rs`：新增 4 个纯函数边界测试（前缀裁剪、尾斜杠、非 cwd 原样、根目录/空 cwd/Windows 分隔符）
- **涉及 commit**：无（未提交）
- **验证状态**：待验证
