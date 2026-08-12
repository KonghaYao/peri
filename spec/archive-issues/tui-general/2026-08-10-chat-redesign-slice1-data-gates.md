> 归档于 2026-08-11，原路径 spec/issues/2026-08-10-chat-redesign-slice1-data-gates.md

# Chat 消息流 Redesign — Slice 1 数据门结论与性能基线

**状态**：Closed（Slice 1 定案，供 Slice 2-5 引用）
**创建日期**：2026-08-10
**规格**：`docs/design/tui-chat-message-flow-style.md`（§4/§4.1/§12）
**切片**：权威执行计划 Slice 1（地基：主题 token、终端能力、符号层、截断、数据门）

## 背景

Slice 1 为 chat 消息流 redesign 落地基（纯新增、零消费方），其中 §3 的 5 项数据门在
本切片以**只读代码核验**定案。本文件是后续切片的决策依据；若代码事实变化，
以代码为准并在此更新。

## 数据门结论（5 项）

### G-Diff：ToolEnded 无结构化 diff → 客户端解析，失败静默降级

- **代码事实**：`TuiToolEnded` 仅含 `{ tool_id, output_summary, is_error, agent_id }`
  （`peri-tui/src/kit/stream_data.rs:43-49`），无 diff 字段；`TuiToolCard.diff:
  Option<TuiDiffBlock>` 类型已存在但 `build_tool_card` 硬编码 `None`
  （`tui_render_unit.rs:726`）。
- **定案**：Slice 5 由 TUI 客户端从 tool 输出文本解析 unified diff；解析失败/非法/
  空 → 静默降级为 `+N −M` 计数（`diff_change_summary` 已有）。**不改 ACP 协议**。
- **影响切片**：Slice 5。

### G-Tokens：无逐回答 usage → 完成元数据仅 duration

- **代码事实**：全 TUI 事件面无逐回答 token usage；仅 `BudgetWarning { used, limit }`
  提供**上下文级** tokens（`acp_events/system.rs:23-45`），不归属单个回答。
- **定案**：回答完成元数据仅 `12.4s` 形式 duration（§6.2 的 `· 1.8k tokens` 在数据
  不可达时省略），并记入 `spec/issues/` 降级。
- **影响切片**：Slice 2/3（meta 字段）。

### G-Interjection：协议无区分标记 → 来源字段占位，行为不变

- **代码事实**：ACP 事件流无 interjection 区分标记；`LocalUserBubble` 构造点无来源
  字段（`input_area.rs:924-939` 附近）。
- **定案**：Slice 4 在 `LocalUserBubble` 构造点增加来源字段占位（如实标注协议依赖），
  **行为不变**；interjection 以 user 样式 + muted metadata 呈现。
- **影响切片**：Slice 4。

### G-started_at：ToolCardAccumulator 保留 started_at

- **代码事实**：`ToolCardAccumulator.started_at: Instant`（`acp_types.rs:761`）在 TUI
  侧保留；running duration（`4s`）已由它推导（`format_running_duration`）。
- **定案**：completed duration 取同源 `started_at`（running→completed 状态迁移不重建
  accumulator），无需新增时间字段。
- **影响切片**：Slice 3。

### G-Perf：PERI_RENDER_TIMING 基线

- **代码事实**：`PERI_RENDER_TIMING=1` 诊断已存在（`message_area/mod.rs`
  `render_timing_enabled`/`trace_phase`），按帧输出 `hash` / `concat` / `viewport` /
  `frame-total` 四段耗时（tracing target `perf.render`）。
- **基线获取方法**：`PERI_RENDER_TIMING=1 cargo run -p peri-tui -- -a`，流式输入后
  观察日志；Slice 5 以同口径对比（流式 rebuild ≈ 1 个 VM、视口行数 ≈ 终端高度）。
- **影响切片**：Slice 5 验收对比。

## 本切片落地项（摘要）

- 主题：`SemanticTokens` 增 `accents{primary,user,assistant,reasoning,tool}`、
  `syntax{command,path}`、`text.secondary`、`surface.raised/sunken`；dark 按规格 §4
  表（`status.running` → `#7DCFFF`），light 可读等值；`themes/{dark,light}.json`
  同步；loader 新键缺省回退内置 dark；`to_peri_colors()` 增 9 项映射，
  `to_palette()` 未动。
- 终端能力：`kit/terminal_caps.rs`（`TerminalCaps{color,unicode,italic,truecolor}` +
  `detect_caps` + `SymbolSet` 降级表）；`entry.rs` 启动探测一次写入
  `atoms::TERMINAL_CAPS`。
- NO_COLOR 剥离：`message_area/mod.rs` 视口裁剪后可见行颜色剥离 pass（保留
  modifier/符号/文本，G3 视口级）。
- 截断：`truncate_by_width`（grapheme + display width）已存在于
  `peri-tui/src/truncate.rs:80`（既有用户改动），本切片未新建重复实现，仅补
  combining mark 单测；调用点替换归 Slice 3。
- i18n：`msg-status-*` / `msg-user-prompt` / `msg-thinking` / `msg-new-output` 等
  后备文案入双 FTL（en + zh-CN）。
